//! App state entity: the board data plus refresh/sync/rate-limit status.
//! All GitHub work happens on the background executor; results hop back to
//! the UI thread via `this.update`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use gpui::{Context, Subscription};
use prmarmot_core::attention::{
    semantic_needs_action, semantic_notice, ObservationKind, SnapshotNamespace, MAX_SNAPSHOTS,
};
use prmarmot_core::board::{
    carry_forward_conflicts, fetch_board_scoped_with_tracked, fetch_more_board_scoped, BoardConfig,
    BoardFetch, BoardPagination, BoardRow, BoardScope, Mode, TrackedPr, TrackedPrStatus,
};
use prmarmot_core::github::access::AccessGaps;
use prmarmot_core::github::gh_cli::RepoDiscovery;
use prmarmot_core::github::rate_limit::{backoff_secs, should_back_off, RateLimitInfo};
use prmarmot_core::github::{GhError, GithubTransport};
use prmarmot_local::config::AuthSettings;
use prmarmot_local::session;

use crate::attention_state::{
    observation, AttentionState, Snooze, SnoozeCondition, TrackedStatus, MAX_SNOOZES, MAX_WATCHES,
};
use crate::platform::{Platform, PlatformEvent};

#[derive(Debug, Clone, Copy)]
pub struct AttentionPreferences {
    pub notifications: bool,
    pub notification_sound: bool,
    pub notify_all_needs_action: bool,
    pub dock_badge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStatus {
    Checking,
    Ready,
    /// Neither a token stored by PR Marmot nor a usable `gh` — the onboarding
    /// screen offers signing in here.
    MissingGh,
    NotAuthenticated,
    Network(String),
    Failed(String),
}

/// One opened GitHub connection: which account, over which transport.
pub struct Connection {
    pub login: String,
    pub transport: Arc<dyn GithubTransport>,
    /// The host the attention state is namespaced by.
    pub host: String,
}

/// How the app opens that connection. Swappable so tests need no network and
/// no `gh`.
pub trait Connector: Send + Sync {
    fn connect(&self) -> Result<Connection, GhError>;
    /// Every repository this account can see, for the repo picker.
    fn list_repos(&self) -> Result<RepoDiscovery, GhError>;
}

/// The real one: resolved `[auth]` settings, re-read on every attempt so a
/// sign-in in the onboarding screen takes effect on the next Retry.
pub struct ConfiguredConnector {
    settings: AuthSettings,
    user_agent: String,
}

impl ConfiguredConnector {
    pub fn new(settings: AuthSettings) -> Self {
        Self {
            settings,
            user_agent: session::user_agent("prmarmot", env!("CARGO_PKG_VERSION")),
        }
    }

    fn open(&self) -> Result<session::Session, GhError> {
        session::connect(&self.settings, &self.user_agent)
    }
}

impl Connector for ConfiguredConnector {
    fn connect(&self) -> Result<Connection, GhError> {
        let session = self.open()?;
        let login = session.login()?;
        Ok(Connection {
            login,
            transport: session.transport_arc(),
            host: session.host.clone(),
        })
    }

    fn list_repos(&self) -> Result<RepoDiscovery, GhError> {
        self.open()?.list_repos()
    }
}

pub struct AppState {
    pub scope: BoardScope,
    pub me: Option<String>,
    pub setup: SetupStatus,
    pub mode: Mode,
    pub config: BoardConfig,
    pub transport: Arc<dyn GithubTransport>,
    connector: Arc<dyn Connector>,
    pub rows: Vec<BoardRow>,
    pub last_synced: Option<DateTime<Local>>,
    pub syncing: bool,
    pub error: Option<String>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
    /// What the token was not allowed to read on the last fetch (a
    /// fine-grained token cannot read checks), for the one-line notice under
    /// the header. Empty for a `gh` token.
    pub access: AccessGaps,
    pagination: BoardPagination,
    /// Bumped on every successful fetch; observers use it to detect new rows
    /// without diffing (and to gate their reactions — the PRFlow observer-loop
    /// lesson).
    pub generation: u64,
    /// Do not fetch before this epoch second (set after a rate-limit hit,
    /// always clamped 60..900s ahead).
    backoff_until: Option<u64>,
    /// Bumped on every repo/mode switch; an in-flight fetch from a previous
    /// epoch must not write its (now wrong-view) results.
    epoch: u64,
    setup_epoch: u64,
    /// Last-seen rows per queue, so switching restores the previous view
    /// instantly and refreshes in the background instead of flashing empty
    /// (critique #1). Bounded by construction — one entry per `Mode` — but the
    /// cap is explicit per the PRFlow bounded-everything guardrail.
    cache: HashMap<Mode, CachedQueue>,
    pub attention: Option<AttentionState>,
    pub attention_preferences: AttentionPreferences,
    platform: Platform,
    process_baselines: BoundedIds<MAX_PROCESS_BASELINES>,
    status_baselines: BoundedIds<MAX_STATUS_BASELINES>,
    focused_selected_pr: Option<String>,
    persistence_revision: u64,
    persistence_in_flight: bool,
    persistence_quitting: bool,
    persistence_task: Option<gpui::Task<()>>,
    persistence_quit_subscription: Option<Subscription>,
    pub badge_count: usize,
    pub badge_coverage_complete: bool,
    pub tracked_loaded: usize,
    pub tracked_total: usize,
    pub notification_error: Option<String>,
    tracked_cursor: usize,
    demo_seeded: bool,
}

/// A queue's last-known rows and when they were fetched. Rate-limit budget is
/// deliberately NOT cached: it is account-global, not per-queue, so restoring a
/// stale budget could wrongly trip the back-off on a switch.
struct CachedQueue {
    rows: Vec<BoardRow>,
    last_synced: Option<DateTime<Local>>,
    truncated: bool,
    pagination: BoardPagination,
}

/// One cache entry per queue; there are exactly two queues today. Kept explicit
/// so the bound survives a third view (roadmap "All open PRs").
const MAX_CACHED_QUEUES: usize = 2;
const MAX_PROCESS_BASELINES: usize = MAX_SNAPSHOTS;
const MAX_STATUS_BASELINES: usize = MAX_WATCHES + MAX_SNOOZES;

/// Set membership with deterministic FIFO eviction. Baselines only suppress a
/// process-local first-observation notification, so forgetting the oldest ID
/// at the durable state's corresponding bound is safe.
struct BoundedIds<const MAX: usize> {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl<const MAX: usize> BoundedIds<MAX> {
    fn new() -> Self {
        Self {
            ids: HashSet::with_capacity(MAX),
            order: VecDeque::with_capacity(MAX),
        }
    }

    fn insert(&mut self, id: String) -> bool {
        if self.ids.contains(&id) {
            return false;
        }
        if self.order.len() == MAX {
            if let Some(evicted) = self.order.pop_front() {
                self.ids.remove(&evicted);
            }
        }
        self.ids.insert(id.clone());
        self.order.push_back(id);
        true
    }
}

impl AppState {
    pub fn new(
        scope: BoardScope,
        mode: Mode,
        config: BoardConfig,
        transport: Arc<dyn GithubTransport>,
        connector: Arc<dyn Connector>,
        attention_preferences: AttentionPreferences,
    ) -> Self {
        Self {
            scope,
            me: None,
            setup: SetupStatus::Checking,
            mode,
            config,
            transport,
            connector,
            rows: Vec::new(),
            last_synced: None,
            syncing: false,
            error: None,
            rate: None,
            truncated: false,
            access: AccessGaps::default(),
            pagination: BoardPagination::default(),
            generation: 0,
            backoff_until: None,
            epoch: 0,
            setup_epoch: 0,
            cache: HashMap::new(),
            attention: None,
            attention_preferences,
            platform: Platform::new(),
            process_baselines: BoundedIds::new(),
            status_baselines: BoundedIds::new(),
            focused_selected_pr: None,
            persistence_revision: 0,
            persistence_in_flight: false,
            persistence_quitting: false,
            persistence_task: None,
            persistence_quit_subscription: None,
            badge_count: 0,
            badge_coverage_complete: false,
            tracked_loaded: 0,
            tracked_total: 0,
            notification_error: None,
            tracked_cursor: 0,
            demo_seeded: false,
        }
    }

    /// Snapshot the active queue's rows before we leave it, so returning to it
    /// restores instantly. Evicts the oldest-arbitrary entry if somehow over
    /// the (currently unreachable) bound.
    fn stash_current(&mut self) {
        if self.cache.len() >= MAX_CACHED_QUEUES && !self.cache.contains_key(&self.mode) {
            if let Some(&victim) = self.cache.keys().find(|k| **k != self.mode) {
                self.cache.remove(&victim);
            }
        }
        self.cache.insert(
            self.mode,
            CachedQueue {
                rows: self.rows.clone(),
                last_synced: self.last_synced,
                truncated: self.truncated,
                pagination: self.pagination.clone(),
            },
        );
    }

    /// Repo-picker switch: drop the old repo's rows immediately (stale rows
    /// from another repo are worse than an empty table) and fetch the new one.
    /// Every cached queue belonged to the old repo, so the cache is dropped.
    pub fn switch_scope(&mut self, scope: BoardScope, cx: &mut Context<Self>) {
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        self.cache.clear();
        self.reset_and_refetch(cx);
    }

    pub fn validate_setup(&mut self, cx: &mut Context<Self>) {
        self.setup_epoch += 1;
        let setup_epoch = self.setup_epoch;
        self.setup = SetupStatus::Checking;
        self.error = None;
        self.syncing = false;
        cx.notify();
        let connector = self.connector.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connector.connect() })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.setup_epoch != setup_epoch {
                    return;
                }
                match result {
                    Ok(connection) => {
                        state.attention = Some(AttentionState::load(SnapshotNamespace::new(
                            connection.host,
                            connection.login.clone(),
                        )));
                        state.transport = connection.transport;
                        state.me = Some(connection.login);
                        state.setup = SetupStatus::Ready;
                        state.refresh(cx);
                    }
                    Err(GhError::NotInstalled) => state.setup = SetupStatus::MissingGh,
                    Err(GhError::NotAuthenticated) => state.setup = SetupStatus::NotAuthenticated,
                    Err(GhError::Network(message)) => state.setup = SetupStatus::Network(message),
                    Err(error) => state.setup = SetupStatus::Failed(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Derived notes and issue links depend on configuration. Discard both
    /// queues and invalidate in-flight results before fetching with new rules.
    pub fn apply_config(&mut self, config: BoardConfig, cx: &mut Context<Self>) {
        if self.config == config {
            return;
        }
        self.config = config;
        self.cache.clear();
        self.reset_and_refetch(cx);
    }

    pub fn apply_attention_preferences(&mut self, preferences: AttentionPreferences) {
        self.attention_preferences = preferences;
        self.update_badge();
    }

    pub fn set_focused_selection(&mut self, pr_id: Option<String>, focused: bool) {
        self.focused_selected_pr = focused.then_some(pr_id).flatten();
    }

    /// The repository picker's discovery, over whichever transport this run
    /// authenticated with.
    pub fn connector(&self) -> Arc<dyn Connector> {
        self.connector.clone()
    }

    pub fn take_platform_event(&self) -> Option<PlatformEvent> {
        self.platform.take_event()
    }

    pub fn check_notification_permission(&self) {
        self.platform.check_notification_permission();
    }

    pub fn request_notification_permission(&self) {
        self.platform.request_notification_permission();
    }

    pub fn is_changed(&self, pr_id: &str) -> bool {
        self.attention
            .as_ref()
            .is_some_and(|attention| attention.is_changed(pr_id))
    }

    pub fn change_summary(&self, pr_id: &str) -> Vec<String> {
        self.attention
            .as_ref()
            .map(|attention| attention.change_summary(pr_id))
            .unwrap_or_default()
    }

    pub fn is_watched(&self, pr_id: &str) -> bool {
        self.attention
            .as_ref()
            .is_some_and(|attention| attention.is_watched(pr_id))
    }

    pub fn watched_fallback(&self, pr_id: &str) -> Option<(String, String)> {
        self.attention.as_ref()?.watch(pr_id).map(|watch| {
            (
                watch.url.clone(),
                format!("{}#{} · {:?}", watch.repo, watch.number, watch.status),
            )
        })
    }

    pub fn snooze_description(&self, pr_id: &str) -> Option<String> {
        self.attention
            .as_ref()?
            .snooze(pr_id)
            .map(Snooze::description)
    }

    pub fn acknowledge(&mut self, pr_id: &str, cx: &mut Context<Self>) {
        if self
            .attention
            .as_mut()
            .is_some_and(|attention| attention.snapshots.acknowledge(pr_id))
        {
            self.schedule_persist(cx);
            self.generation += 1;
            cx.notify();
        }
    }

    pub fn toggle_watch(&mut self, row: &BoardRow, cx: &mut Context<Self>) -> Option<String> {
        let attention = self.attention.as_mut()?;
        let was_watched = attention.is_watched(&row.id);
        let evicted = attention.toggle_watch(row);
        let message = if was_watched {
            format!("Stopped watching #{}", row.number)
        } else if let Some(evicted) = evicted {
            format!(
                "Watching #{} · evicted {}#{}",
                row.number, evicted.repo, evicted.number
            )
        } else {
            format!("Watching #{}", row.number)
        };
        self.schedule_persist(cx);
        self.generation += 1;
        cx.notify();
        Some(message)
    }

    pub fn set_snooze(
        &mut self,
        row: &BoardRow,
        condition: SnoozeCondition,
        cx: &mut Context<Self>,
    ) {
        if let Some(attention) = self.attention.as_mut() {
            attention.set_snooze(AttentionState::snooze_for(row, condition));
            self.schedule_persist(cx);
            self.update_badge();
            self.generation += 1;
            cx.notify();
        }
    }

    pub fn cancel_snooze(&mut self, pr_id: &str, cx: &mut Context<Self>) {
        if self
            .attention
            .as_mut()
            .is_some_and(|attention| attention.cancel_snooze(pr_id))
        {
            self.schedule_persist(cx);
            self.update_badge();
            self.generation += 1;
            cx.notify();
        }
    }

    pub fn wake_timed_snoozes(&mut self, cx: &mut Context<Self>) {
        let woke = self
            .attention
            .as_mut()
            .map(|attention| attention.wake_due(&self.rows, Utc::now()))
            .unwrap_or_default();
        if !woke.is_empty() {
            self.schedule_persist(cx);
            self.update_badge();
            self.generation += 1;
            cx.notify();
        }
    }

    /// Select one of the two prototype dashboards. The previous queue's rows
    /// are stashed and the target queue is restored from cache immediately (if
    /// seen before) so the table never flashes empty; a background refresh then
    /// updates it in place (critique #1).
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.stash_current();
        self.mode = mode;
        self.epoch += 1; // any in-flight fetch is now for the wrong view
        self.syncing = false; // don't let it dedup the refresh we start now
        self.error = None;
        self.access = AccessGaps::default(); // the refresh says again
        match self.cache.get(&mode) {
            Some(cached) => {
                self.rows = cached.rows.clone();
                self.last_synced = cached.last_synced;
                self.truncated = cached.truncated;
                self.pagination = cached.pagination.clone();
            }
            None => {
                self.rows.clear();
                self.last_synced = None;
                self.truncated = false;
                self.pagination = BoardPagination::default();
            }
        }
        self.generation += 1; // observers push the restored (or empty) rows
        self.refresh(cx); // background-update the queue we just switched to
        cx.notify();
    }

    fn reset_and_refetch(&mut self, cx: &mut Context<Self>) {
        self.rows.clear();
        self.last_synced = None;
        self.error = None;
        self.truncated = false;
        self.access = AccessGaps::default();
        self.pagination = BoardPagination::default();
        self.generation += 1; // observers push the (empty) rows to the table
        self.epoch += 1; // any in-flight fetch is now for the wrong view
        self.syncing = false; // don't let it dedup the fetch we start now
        self.refresh(cx);
        cx.notify();
    }

    /// Seconds until the next allowed fetch while backing off after a
    /// rate-limit hit, or `None` if a fetch could run now. Lets the UI show a
    /// "paused — retrying in Xm" state instead of a permanent "Loading…" when a
    /// switch to an unseen queue lands inside a back-off window.
    pub fn backoff_remaining(&self) -> Option<u64> {
        let now = Local::now().timestamp().max(0) as u64;
        self.backoff_until.filter(|&u| u > now).map(|u| u - now)
    }

    /// Kick off one fetch unless one is in flight or we are backing off.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.setup != SetupStatus::Ready {
            return;
        }
        if self.syncing {
            return;
        }
        let now = Local::now().timestamp().max(0) as u64;
        if let Some(until) = self.backoff_until {
            if now < until {
                return; // quietly wait out the backoff window
            }
            self.backoff_until = None;
        }
        // Preserve the reserve for the user's own gh/git usage.
        if let Some(rate) = &self.rate {
            if should_back_off(rate) {
                let reset = rate.reset_epoch();
                if reset.is_some_and(|r| now < r) {
                    self.backoff_until = Some(now + backoff_secs(reset, now));
                    self.error = Some(format!(
                        "GitHub budget low ({} left) — pausing refresh",
                        rate.remaining
                    ));
                    cx.notify();
                    return;
                }
            }
        }

        self.syncing = true;
        self.error = None;
        cx.notify();

        let transport = self.transport.clone();
        let scope = self.scope.clone();
        let Some(me) = self.me.clone() else {
            return;
        };
        let mode = self.mode;
        let config = self.config.clone();
        let epoch = self.epoch;
        const MAX_TRACKED_PER_REFRESH: usize = 50;
        let (tracked_ids, tracked_total) = self
            .attention
            .as_ref()
            .map(|attention| attention.tracked_ids(MAX_TRACKED_PER_REFRESH, self.tracked_cursor))
            .unwrap_or_default();
        self.tracked_total = tracked_total;
        if tracked_total > 0 {
            self.tracked_cursor = (self.tracked_cursor + tracked_ids.len()) % tracked_total;
        }

        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    // The `gh` subprocess blocks; that is fine on the
                    // background pool for a call made every few minutes.
                    fetch_board_scoped_with_tracked(
                        transport.as_ref(),
                        mode,
                        &scope,
                        &me,
                        &config,
                        &tracked_ids,
                    )
                })
                .await;

            let _ = this.update(cx, |state, cx| {
                if state.epoch != epoch {
                    return; // view switched mid-fetch; a newer fetch owns state
                }
                state.syncing = false;
                match fetched {
                    Ok(mut board) => {
                        state.keep_known_conflicts(&mut board.rows, mode);
                        for tracked in &mut board.tracked {
                            if let Some(row) = tracked.row.as_mut() {
                                // Tracked rows read as in Involving me: your PRs as
                                // authored, anyone else's as another author's.
                                state.keep_known_conflicts(
                                    std::slice::from_mut(row),
                                    Mode::Authored,
                                );
                            }
                        }
                        state.tracked_loaded = board.tracked.len();
                        state.process_tracked(&board.tracked, cx);
                        let mut observed_rows = board.rows.clone();
                        for tracked in &board.tracked {
                            if let Some(row) = &tracked.row {
                                if !observed_rows.iter().any(|visible| visible.id == row.id) {
                                    observed_rows.push(row.clone());
                                }
                            }
                        }
                        state.process_accepted_rows(&observed_rows, cx);
                        state.rows = board.rows;
                        state.rate = board.rate;
                        state.truncated = board.truncated;
                        state.access = board.access;
                        // A deliberate refresh starts again at page one; load-more
                        // cursors are only preserved by queue cache restoration.
                        state.pagination = board.pagination;
                        state.last_synced = Some(Local::now());
                        state.generation += 1;
                        state.update_badge();
                    }
                    Err(GhError::RateLimited { reset_epoch }) => {
                        let now = Local::now().timestamp().max(0) as u64;
                        let wait = backoff_secs(reset_epoch, now);
                        state.backoff_until = Some(now + wait);
                        state.error = Some(format!(
                            "GitHub rate limited — retrying in {}m",
                            wait.div_ceil(60)
                        ));
                    }
                    Err(e) => {
                        state.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn can_load_more(&self) -> bool {
        !self.syncing
            && self.backoff_remaining().is_none()
            && self.pagination.can_load_more(self.mode)
    }

    pub fn page_limit_reached(&self) -> bool {
        self.pagination.page_limit_reached(self.mode)
    }

    /// Fetch one page for every non-exhausted alias. This is user-invoked only;
    /// refresh and the timer never auto-paginate.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.can_load_more() {
            return;
        }
        let now = Local::now().timestamp().max(0) as u64;
        if self.backoff_until.is_some_and(|until| now < until) {
            return;
        }
        if let Some(rate) = &self.rate {
            if should_back_off(rate) {
                let reset = rate.reset_epoch();
                if reset.is_some_and(|r| now < r) {
                    self.backoff_until = Some(now + backoff_secs(reset, now));
                    self.error = Some(format!(
                        "GitHub budget low ({} left) — pausing refresh",
                        rate.remaining
                    ));
                    cx.notify();
                    return;
                }
            }
        }

        self.syncing = true;
        self.error = None;
        cx.notify();
        let transport = self.transport.clone();
        let scope = self.scope.clone();
        let Some(me) = self.me.clone() else {
            return;
        };
        let mode = self.mode;
        let config = self.config.clone();
        let epoch = self.epoch;
        let current = BoardFetch {
            rows: self.rows.clone(),
            rate: self.rate.clone(),
            truncated: self.truncated,
            pagination: self.pagination.clone(),
            tracked: Vec::new(),
            access: self.access,
        };
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    fetch_more_board_scoped(
                        transport.as_ref(),
                        mode,
                        &scope,
                        &me,
                        &config,
                        &current,
                    )
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.epoch != epoch {
                    return;
                }
                state.syncing = false;
                match fetched {
                    Ok(mut board) => {
                        state.keep_known_conflicts(&mut board.rows, mode);
                        state.process_accepted_rows(&board.rows, cx);
                        state.rows = board.rows;
                        state.rate = board.rate;
                        state.truncated = board.truncated;
                        state.access = board.access;
                        state.pagination = board.pagination;
                        // Loading older pages does not refresh the earlier rows.
                        // Keep their original sync timestamp honest.
                        state.generation += 1;
                        state.update_badge();
                    }
                    Err(GhError::RateLimited { reset_epoch }) => {
                        let now = Local::now().timestamp().max(0) as u64;
                        let wait = backoff_secs(reset_epoch, now);
                        state.backoff_until = Some(now + wait);
                        state.error = Some(format!(
                            "GitHub rate limited — retrying in {}m",
                            wait.div_ceil(60)
                        ));
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// GitHub reports mergeability as UNKNOWN while it recomputes (after
    /// every base-branch push); keep the conflict it reported last time so the
    /// row doesn't flicker and the attention state doesn't record a phantom
    /// "changed, then changed back".
    fn keep_known_conflicts(&self, rows: &mut [BoardRow], mode: Mode) {
        let (Some(attention), Some(me)) = (self.attention.as_ref(), self.me.as_deref()) else {
            return;
        };
        carry_forward_conflicts(
            rows,
            |id| attention.snapshots.last_conflict(id),
            mode,
            me,
            &self.config,
        );
    }

    fn process_tracked(&mut self, tracked: &[TrackedPr], cx: &mut Context<Self>) {
        let Some(attention) = self.attention.as_mut() else {
            return;
        };
        let mut dirty = false;
        let mut notices = Vec::new();
        for tracked in tracked {
            let status = match tracked.status {
                TrackedPrStatus::Open => TrackedStatus::Open,
                TrackedPrStatus::Closed => TrackedStatus::Closed,
                TrackedPrStatus::Merged => TrackedStatus::Merged,
                TrackedPrStatus::Inaccessible => TrackedStatus::Inaccessible,
            };
            let changed_from = attention.update_watch_status(&tracked.pr_id, status);
            dirty |= changed_from.is_some();
            let first = self.status_baselines.insert(tracked.pr_id.clone());
            if first
                || changed_from.is_none()
                || !self.attention_preferences.notifications
                || attention.snooze(&tracked.pr_id).is_some()
                || self.focused_selected_pr.as_deref() == Some(&tracked.pr_id)
            {
                continue;
            }
            let message = match status {
                TrackedStatus::Merged => {
                    Some(("Watched PR merged", "The watched pull request was merged"))
                }
                TrackedStatus::Closed => {
                    Some(("Watched PR closed", "The watched pull request was closed"))
                }
                TrackedStatus::Inaccessible => Some((
                    "Watched PR unavailable",
                    "GitHub no longer returned this pull request",
                )),
                TrackedStatus::Open | TrackedStatus::Unknown => None,
            };
            if let Some((title, body)) = message {
                notices.push((title.to_owned(), body.to_owned(), tracked.pr_id.clone()));
            }
        }
        for (title, body, pr_id) in notices {
            self.platform.notify(
                title,
                body,
                pr_id,
                self.attention_preferences.notification_sound,
            );
        }
        if dirty {
            self.schedule_persist(cx);
        }
    }

    fn process_accepted_rows(&mut self, rows: &[BoardRow], cx: &mut Context<Self>) {
        let Some(attention) = self.attention.as_mut() else {
            return;
        };
        if !self.demo_seeded && std::env::var_os("PRMARMOT_DEMO_ATTENTION").is_some() {
            if let Some(row) = rows.first() {
                let mut before = observation(row);
                before.updated_at = Some("2026-09-01T00:00:00Z".into());
                before.semantic.ci = prmarmot_core::attention::ObservedCi::Running;
                let _ = attention.snapshots.observe(&row.id, before);
            }
            if let Some(row) = rows.get(1) {
                if !attention.is_watched(&row.id) {
                    attention.toggle_watch(row);
                }
            }
            if let Some(row) = rows.get(2) {
                if attention.snooze(&row.id).is_none() {
                    attention.set_snooze(AttentionState::snooze_for(
                        row,
                        SnoozeCondition::Until {
                            deadline: Utc::now() + chrono::Duration::hours(6),
                        },
                    ));
                }
            }
            self.demo_seeded = true;
        }
        let woke = attention.wake_due(rows, Utc::now());
        let mut dirty = !woke.is_empty();
        let mut notices = Vec::new();
        for row in rows {
            let previous = attention
                .snapshots
                .snapshot(&row.id)
                .map(|snapshot| snapshot.latest.clone());
            let result = match attention.snapshots.observe(&row.id, observation(row)) {
                Ok(result) => result,
                Err(error) => {
                    attention.storage_error = Some(error.to_string());
                    continue;
                }
            };
            dirty |= result.kind != ObservationKind::Unchanged;

            // Persisted history may describe a transition, but the first
            // successful observation of each PR in this process is baseline-only.
            let first_this_run = self.process_baselines.insert(row.id.clone());
            if first_this_run
                || !matches!(
                    result.kind,
                    ObservationKind::Changed {
                        semantic_transition: true
                    }
                )
            {
                continue;
            }
            let snoozed = attention.snooze(&row.id).is_some();
            let watched = attention.is_watched(&row.id);
            let entered_action = self.attention_preferences.notify_all_needs_action
                && row.category == prmarmot_core::board::Category::Action
                && row.author.as_deref() == self.me.as_deref()
                && previous
                    .as_ref()
                    .is_none_or(|old| !semantic_needs_action(old));
            if !self.attention_preferences.notifications
                || snoozed
                || self.focused_selected_pr.as_deref() == Some(&row.id)
                || (!watched && !entered_action)
            {
                continue;
            }
            if let Some(notice) = semantic_notice(previous.as_ref(), row) {
                notices.push((notice.title, notice.body, row.id.clone()));
            }
        }
        for (title, body, pr_id) in notices {
            self.platform.notify(
                title,
                body,
                pr_id,
                self.attention_preferences.notification_sound,
            );
        }
        if dirty {
            self.schedule_persist(cx);
        }
    }

    fn update_badge(&mut self) {
        let snoozed = |id: &str| {
            self.attention
                .as_ref()
                .is_some_and(|attention| attention.snooze(id).is_some())
        };
        let mut authored_loaded = self.mode == Mode::Authored && self.last_synced.is_some();
        let mut review_loaded = self.mode == Mode::Review && self.last_synced.is_some();
        let mut ids = HashSet::new();
        let mut visit = |mode: Mode, rows: &[BoardRow]| {
            match mode {
                Mode::Authored => authored_loaded = true,
                Mode::Review => review_loaded = true,
            }
            for row in rows {
                let counts = match mode {
                    Mode::Authored => {
                        row.category == prmarmot_core::board::Category::Action
                            && row.author.as_deref() == self.me.as_deref()
                    }
                    Mode::Review => {
                        row.queue_provenance
                            == Some(prmarmot_core::board::QueueProvenance::Requested)
                            && row.category == prmarmot_core::board::Category::Todo
                    }
                };
                if counts && !snoozed(&row.id) {
                    ids.insert(row.id.clone());
                }
            }
        };
        for (mode, rows) in loaded_badge_sources(
            self.mode,
            &self.rows,
            self.last_synced.is_some(),
            &self.cache,
        ) {
            visit(mode, rows);
        }
        self.badge_count = ids.len();
        self.badge_coverage_complete = authored_loaded && review_loaded;
        self.platform
            .set_badge(self.badge_count, self.attention_preferences.dock_badge);
    }

    fn schedule_persist(&mut self, cx: &mut Context<Self>) {
        self.persistence_revision = self.persistence_revision.wrapping_add(1);
        self.ensure_persistence_flush_on_quit(cx);
        if self.persistence_in_flight {
            return;
        }
        self.spawn_persist(Duration::from_millis(250), cx);
    }

    fn ensure_persistence_flush_on_quit(&mut self, cx: &mut Context<Self>) {
        if self.persistence_quit_subscription.is_some() {
            return;
        }
        self.persistence_quit_subscription = Some(cx.on_app_quit(|state, cx| {
            // Await an in-flight writer before writing the newest snapshot,
            // preventing an older save from replacing quit-time state through
            // the shared temp path. A debounce-only task is canceled below.
            state.persistence_quitting = true;
            let pending = if state.persistence_in_flight {
                state.persistence_task.take()
            } else {
                // Do not spend GPUI's shutdown grace period waiting out the
                // 250 ms debounce; dropping the task cancels that timer and
                // the newest snapshot is written immediately below.
                state.persistence_task.take();
                None
            };
            let snapshot = state.attention.clone();
            let executor = cx.background_executor().clone();
            async move {
                if let Some(pending) = pending {
                    pending.await;
                }
                if let Some(snapshot) = snapshot {
                    let _ = executor.spawn(async move { snapshot.save() }).await;
                }
            }
        }));
    }

    fn spawn_persist(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let revision = self.persistence_revision;
        self.persistence_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let snapshot = match this.update(cx, |state, _| {
                if state.persistence_revision != revision || state.persistence_in_flight {
                    return None;
                }
                state.persistence_in_flight = true;
                state.attention.clone()
            }) {
                Ok(Some(snapshot)) => snapshot,
                _ => return,
            };
            let result = cx
                .background_executor()
                .spawn(async move { snapshot.save() })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.persistence_in_flight = false;
                if let Err(error) = result {
                    if let Some(attention) = state.attention.as_mut() {
                        attention.storage_error = Some(error);
                    }
                    cx.notify();
                }
                if state.persistence_revision != revision && !state.persistence_quitting {
                    // Changes made during the write are coalesced into exactly
                    // one follow-up writer; no blocking saves can overlap.
                    state.spawn_persist(Duration::ZERO, cx);
                }
            });
        }));
    }
}

fn loaded_badge_sources<'a>(
    active_mode: Mode,
    active_rows: &'a [BoardRow],
    active_loaded: bool,
    cache: &'a HashMap<Mode, CachedQueue>,
) -> Vec<(Mode, &'a [BoardRow])> {
    let mut sources = Vec::with_capacity(MAX_CACHED_QUEUES);
    if active_loaded {
        sources.push((active_mode, active_rows));
    }
    sources.extend(cache.iter().filter_map(|(mode, cached)| {
        (*mode != active_mode && cached.last_synced.is_some())
            .then_some((*mode, cached.rows.as_slice()))
    }));
    sources
}

/// "just now" / "3m ago" / "2h 15m ago" — static text, recomputed on notify.
pub fn refresh_interval(config_secs: Option<u64>) -> Duration {
    prmarmot_local::config::refresh_interval(config_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_ids_evict_in_fifo_order() {
        let mut ids = BoundedIds::<2>::new();
        assert!(ids.insert("a".into()));
        assert!(ids.insert("b".into()));
        assert!(!ids.insert("a".into()));
        assert!(ids.insert("c".into()));
        assert!(!ids.ids.contains("a"));
        assert!(ids.ids.contains("b"));
        assert!(ids.ids.contains("c"));
        assert_eq!(ids.order.len(), 2);
    }

    #[test]
    fn badge_sources_skip_stale_cache_for_active_mode() {
        let mut cache = HashMap::new();
        for mode in [Mode::Authored, Mode::Review] {
            cache.insert(
                mode,
                CachedQueue {
                    rows: Vec::new(),
                    last_synced: Some(Local::now()),
                    truncated: false,
                    pagination: BoardPagination::default(),
                },
            );
        }

        let active_rows = Vec::new();
        let sources = loaded_badge_sources(Mode::Authored, &active_rows, true, &cache);
        assert_eq!(
            sources.iter().map(|(mode, _)| *mode).collect::<Vec<_>>(),
            vec![Mode::Authored, Mode::Review]
        );
    }
}
