//! App state entity: the board data plus refresh/sync/rate-limit status.
//! All GitHub work happens on the background executor; results hop back to
//! the UI thread via `this.update`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use gpui::{Context, Subscription};
use prmarmot_core::attention::{
    semantic_needs_action, semantic_notice, should_notify, watched_status_notice, NotifyCase,
    ObservationKind, SnapshotNamespace, MAX_SNAPSHOTS,
};
use prmarmot_core::board::{
    carry_forward_conflicts, fetch_more_board_scoped, fetch_view, BoardConfig, BoardFetch,
    BoardPagination, BoardRow, BoardScope, Mode, Tracked, TrackedPr, TrackedPrStatus,
};
use prmarmot_core::github::access::AccessGaps;
use prmarmot_core::github::gh_cli::RepoDiscovery;
use prmarmot_core::github::rate_limit::{rate_limited_wait_secs, RateLimitInfo};
use prmarmot_core::github::{GhError, GithubTransport};
use prmarmot_core::search::RemoteFilter;
use prmarmot_core::status::{paused_text, PauseReason};
use prmarmot_local::config::AuthSettings;
use prmarmot_local::session;

use crate::attention_state::{
    observation, AttentionState, Marks, SnoozeCondition, TrackedStatus, MAX_SNOOZES, MAX_WATCHES,
};
use crate::platform::{Platform, PlatformEvent};

#[derive(Debug, Clone, Copy)]
pub struct AttentionPreferences {
    pub notifications: bool,
    pub notification_sound: bool,
    pub notify_all_needs_action: bool,
    pub dock_badge: bool,
}

impl AttentionPreferences {
    /// The file's choices, at launch and after Settings saves.
    pub fn from_file(file: &crate::config::FileConfig) -> Self {
        Self {
            notifications: file.notifications,
            notification_sound: file.notification_sound,
            notify_all_needs_action: file.notify_all_needs_action,
            dock_badge: file.dock_badge,
        }
    }
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
    /// Which sign-in `auto` settled on, for Settings to name.
    pub via: session::Connection,
    /// True when a pasted fine-grained token is carrying the board while `gh`
    /// is signed in to the same host, for the notice under the header.
    pub token_reach_hint: bool,
    /// True when `login` is the one remembered at sign-in rather than an
    /// answer from GitHub; [`Connector::verify_login`] asks while the first
    /// board loads.
    pub login_remembered: bool,
}

/// How the app opens that connection. Swappable so tests need no network and
/// no `gh`.
pub trait Connector: Send + Sync {
    fn connect(&self) -> Result<Connection, GhError>;
    /// Every repository this account can see, for the repo picker.
    fn list_repos(&self) -> Result<RepoDiscovery, GhError>;
    /// The signed-in account as GitHub answers now, over the connection
    /// `connect` opened.
    fn verify_login(&self) -> Result<String, GhError>;
    /// Connect to `host` from now on (the sign-in screen's Enterprise field).
    /// True when that changed the host.
    fn use_host(&self, host: &str) -> bool {
        let _ = host;
        false
    }
}

/// The real one: resolved `[auth]` settings, re-read on every attempt so a
/// sign-in in the onboarding screen takes effect on the next Retry. The host
/// can change once running, when someone signs in to an Enterprise Server
/// from that screen.
pub struct ConfiguredConnector {
    settings: Mutex<AuthSettings>,
    user_agent: String,
    /// The session the last `connect` opened. Discovery and the login check
    /// reuse it rather than resolving the sign-in again (a keychain read or a
    /// `gh` probe); a new `connect` or host replaces it.
    session: Mutex<Option<Arc<session::Session>>>,
}

impl ConfiguredConnector {
    pub fn new(settings: AuthSettings) -> Self {
        Self {
            settings: Mutex::new(settings),
            user_agent: session::user_agent("prmarmot", env!("CARGO_PKG_VERSION")),
            session: Mutex::new(None),
        }
    }

    fn settings(&self) -> AuthSettings {
        self.settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn open(&self) -> Result<session::Session, GhError> {
        session::connect(&self.settings(), &self.user_agent)
    }

    fn remembered_session(&self) -> MutexGuard<'_, Option<Arc<session::Session>>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The session `connect` opened, or a new one when it has not run.
    fn session(&self) -> Result<Arc<session::Session>, GhError> {
        if let Some(session) = self.remembered_session().clone() {
            return Ok(session);
        }
        let session = Arc::new(self.open()?);
        *self.remembered_session() = Some(session.clone());
        Ok(session)
    }
}

impl Connector for ConfiguredConnector {
    fn connect(&self) -> Result<Connection, GhError> {
        *self.remembered_session() = None;
        let session = Arc::new(self.open()?);
        // The login remembered at sign-in, or `gh`'s ten-minute cache of it,
        // so the first board need not wait for a request. A token that
        // stopped working fails on the board request and opens the sign-in
        // screen; a remembered login is checked while the board loads.
        let login = session.login_cached()?;
        // Asked only when it can matter: a fine-grained token reaches one
        // owner's repositories, and the `gh` probe is a local file read.
        let token_reach_hint = session.fine_grained_token
            && matches!(session::gh_login(&session.host), session::GhLogin::SignedIn);
        let connection = Connection {
            login,
            transport: session.transport_arc(),
            host: session.host.clone(),
            via: session.connection,
            token_reach_hint,
            login_remembered: session.stored_login.is_some(),
        };
        *self.remembered_session() = Some(session);
        Ok(connection)
    }

    fn list_repos(&self) -> Result<RepoDiscovery, GhError> {
        self.session()?.list_repos()
    }

    fn verify_login(&self) -> Result<String, GhError> {
        self.session()?.login()
    }

    fn use_host(&self, host: &str) -> bool {
        let mut settings = self.settings.lock().unwrap_or_else(PoisonError::into_inner);
        let next = settings.for_host(host);
        let changed = next.host != settings.host;
        *settings = next;
        if changed {
            *self.remembered_session() = None;
        }
        changed
    }
}

pub struct AppState {
    pub scope: BoardScope,
    pub me: Option<String>,
    /// How `me` signed in, once connected.
    pub signed_in_via: Option<session::Connection>,
    /// A pasted fine-grained token is carrying the board and `gh` has a login
    /// for the same host, so the board may be missing an organization's
    /// private repositories. Drives one notice under the header.
    pub token_reach_hint: bool,
    pub setup: SetupStatus,
    pub mode: Mode,
    pub config: BoardConfig,
    pub transport: Arc<dyn GithubTransport>,
    connector: Arc<dyn Connector>,
    pub rows: Vec<BoardRow>,
    pub last_synced: Option<DateTime<Local>>,
    pub error: Option<String>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
    /// How many PRs the search found in all, loaded or not ("60 of 759
    /// open"), when the view's search said.
    pub total: Option<u64>,
    /// All open's `label:`/`author:` chips as GitHub should answer them. The
    /// search box sets it; only All open sends it.
    remote_filter: RemoteFilter,
    /// The filter GitHub answered for the rows on screen. Differs from
    /// `remote_filter` while a changed filter waits for its request.
    pub rows_filter: RemoteFilter,
    /// The pending request for a changed filter; replacing it cancels the
    /// previous one, so quick successive chips cost one request.
    filter_task: Option<gpui::Task<()>>,
    /// What the token was not allowed to read on the last fetch (a
    /// fine-grained token cannot read checks), for the one-line notice under
    /// the header. Empty for a `gh` token.
    pub access: AccessGaps,
    pagination: BoardPagination,
    /// Bumped on every successful fetch; observers use it to detect new rows
    /// without diffing (and to gate their reactions — the PRFlow observer-loop
    /// lesson).
    pub generation: u64,
    /// The generation a Load more produced and how many rows it added, so the
    /// window can say so once.
    pub loaded_more: Option<(u64, usize)>,
    /// Do not fetch before this epoch second (set after a rate-limit hit,
    /// always clamped 60..900s ahead).
    backoff_until: Option<u64>,
    /// Why `backoff_until` is set, for the line that counts it down.
    pause_reason: PauseReason,
    /// Per view: bumped when that view's in-flight fetch can no longer be
    /// trusted — a scope, config or sign-in change (every view) or a new All
    /// open filter (that view). A result from an older epoch is dropped. A
    /// view switch bumps nothing: a fetch still running for the view left
    /// lands in its cache, so switching back costs no second request.
    epochs: [u64; VIEWS],
    /// Per view: a refresh or Load more is running.
    in_flight: [bool; VIEWS],
    /// Per view: when its last refresh started, so the timer and a view
    /// switch do not ask again right after one.
    fetch_started: [Option<Instant>; VIEWS],
    setup_epoch: u64,
    /// Last-seen rows per queue, so switching restores the previous view
    /// instantly and refreshes in the background instead of flashing empty
    /// (critique #1). Bounded by construction — one entry per `Mode` — but the
    /// cap is explicit per the PRFlow bounded-everything guardrail.
    cache: HashMap<Mode, CachedQueue>,
    /// Shared with the writer of a save in flight, so taking the snapshot to
    /// write costs nothing here; a change made while it writes copies it
    /// once (`Arc::make_mut`).
    pub attention: Option<Arc<AttentionState>>,
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
    /// `PRMARMOT_DEMO_ATTENTION` is set and the first rows have not been
    /// seeded with a change, a watch and a snooze yet (screenshots only).
    demo_attention: bool,
}

/// A queue's last-known rows and when they were fetched. Rate-limit budget is
/// deliberately NOT cached: it is account-global, not per-queue, so restoring a
/// stale budget could wrongly trip the back-off on a switch.
#[derive(Default)]
struct CachedQueue {
    rows: Vec<BoardRow>,
    last_synced: Option<DateTime<Local>>,
    truncated: bool,
    pagination: BoardPagination,
    total: Option<u64>,
    rows_filter: RemoteFilter,
}

/// One cache entry per view: My PRs, Review queue, All open. Deliberately
/// bounded on both axes: three entries, and each holds what its view can load
/// — 60 rows a page and at most five Load more pages, so 300 rows for My PRs
/// and All open and 600 for the review queue's two searches (core's
/// `MAX_BOARD_ROWS × MAX_PAGES_PER_ALIAS`). All open is the new worst case:
/// a busy repository, fully loaded, left selected.
const MAX_CACHED_QUEUES: usize = 3;
/// The number of views, for the per-view arrays.
const VIEWS: usize = 3;
/// A view synced this recently is shown from its cache on a switch without a
/// new request.
const FRESH_ON_SWITCH: Duration = Duration::from_secs(30);

fn slot(mode: Mode) -> usize {
    match mode {
        Mode::Authored => 0,
        Mode::Review => 1,
        Mode::AllOpen => 2,
    }
}
/// How long a changed All open filter waits before its request, so chips
/// clicked in quick succession cost one search.
const FILTER_DEBOUNCE: Duration = Duration::from_millis(400);
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
        // All open is one repository's view; with all repositories the app
        // opens on My PRs instead of on a view that cannot load.
        let mode = if mode == Mode::AllOpen && scope.is_all() {
            Mode::Authored
        } else {
            mode
        };
        Self {
            scope,
            me: None,
            signed_in_via: None,
            token_reach_hint: false,
            setup: SetupStatus::Checking,
            mode,
            config,
            transport,
            connector,
            rows: Vec::new(),
            last_synced: None,
            error: None,
            rate: None,
            truncated: false,
            total: None,
            remote_filter: RemoteFilter::default(),
            rows_filter: RemoteFilter::default(),
            filter_task: None,
            access: AccessGaps::default(),
            pagination: BoardPagination::default(),
            generation: 0,
            loaded_more: None,
            backoff_until: None,
            pause_reason: PauseReason::RateLimited,
            epochs: [0; VIEWS],
            in_flight: [false; VIEWS],
            fetch_started: [None; VIEWS],
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
            demo_attention: std::env::var_os("PRMARMOT_DEMO_ATTENTION").is_some(),
        }
    }

    /// Move the active queue's rows into the cache before we leave it, so
    /// returning to it restores instantly.
    fn stash_current(&mut self) {
        let queue = CachedQueue {
            rows: std::mem::take(&mut self.rows),
            last_synced: self.last_synced,
            truncated: self.truncated,
            pagination: std::mem::take(&mut self.pagination),
            total: self.total,
            rows_filter: std::mem::take(&mut self.rows_filter),
        };
        self.cache_queue(self.mode, queue);
    }

    /// Evicts an arbitrary other entry if somehow over the (currently
    /// unreachable) bound.
    fn cache_queue(&mut self, mode: Mode, queue: CachedQueue) {
        if self.cache.len() >= MAX_CACHED_QUEUES && !self.cache.contains_key(&mode) {
            if let Some(&victim) = self.cache.keys().find(|k| **k != mode) {
                self.cache.remove(&victim);
            }
        }
        self.cache.insert(mode, queue);
    }

    /// Signed in, and the first board request has answered, rows or error.
    pub fn first_fetch_done(&self) -> bool {
        self.setup == SetupStatus::Ready
            && !self.syncing()
            && (self.last_synced.is_some() || self.error.is_some())
    }

    /// The open view has a refresh or Load more running.
    pub fn syncing(&self) -> bool {
        self.in_flight[slot(self.mode)]
    }

    /// Drop whatever `mode`'s in-flight fetch brings back.
    fn invalidate(&mut self, mode: Mode) {
        let view = slot(mode);
        self.epochs[view] += 1;
        self.in_flight[view] = false;
    }

    fn invalidate_all(&mut self) {
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            self.invalidate(mode);
        }
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
        if self.mode == Mode::AllOpen && self.scope.is_all() {
            // The segment is disabled there; land on My PRs, not a dead view.
            self.mode = Mode::Authored;
        }
        self.reset_and_refetch(cx);
    }

    /// The search box's chips changed. All open asks GitHub again, once,
    /// after [`FILTER_DEBOUNCE`], if what GitHub is asked changed; the other
    /// views filter what they loaded and never send it.
    pub fn set_remote_filter(&mut self, filter: RemoteFilter, cx: &mut Context<Self>) {
        if filter == self.remote_filter {
            return;
        }
        self.remote_filter = filter;
        if self.mode != Mode::AllOpen {
            return;
        }
        self.filter_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FILTER_DEBOUNCE).await;
            let _ = this.update(cx, |state, cx| {
                if state.mode != Mode::AllOpen || state.rows_filter == state.remote_filter {
                    return;
                }
                // A new search: the in-flight one and its cursors are for
                // the old filter.
                state.invalidate(Mode::AllOpen);
                state.pagination = state.pagination.restarted();
                state.refresh(cx);
            });
        }));
    }

    pub fn validate_setup(&mut self, cx: &mut Context<Self>) {
        self.setup_epoch += 1;
        let setup_epoch = self.setup_epoch;
        self.setup = SetupStatus::Checking;
        self.error = None;
        // Results from the previous connection may be another account's.
        self.invalidate_all();
        cx.notify();
        let connector = self.connector.clone();
        cx.spawn(async move |this, cx| {
            // The attention file is read here too, off the UI thread.
            let result = cx
                .background_executor()
                .spawn(async move {
                    connector.connect().map(|connection| {
                        let attention = AttentionState::load(SnapshotNamespace::new(
                            connection.host.clone(),
                            connection.login.clone(),
                        ));
                        (connection, attention)
                    })
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.setup_epoch != setup_epoch {
                    return;
                }
                match result {
                    Ok((connection, attention)) => {
                        state.attention = Some(Arc::new(attention));
                        state.transport = connection.transport;
                        state.me = Some(connection.login);
                        state.signed_in_via = Some(connection.via);
                        state.token_reach_hint = connection.token_reach_hint;
                        state.setup = SetupStatus::Ready;
                        state.refresh(cx);
                        if connection.login_remembered {
                            state.verify_login(setup_epoch, cx);
                        }
                    }
                    Err(GhError::NotInstalled) => state.setup = SetupStatus::MissingGh,
                    Err(GhError::NotAuthenticated) => state.setup = SetupStatus::NotAuthenticated,
                    Err(
                        GhError::Network(message)
                        | GhError::Timeout(message)
                        | GhError::Http { message, .. },
                    ) => state.setup = SetupStatus::Network(message),
                    Err(error) => state.setup = SetupStatus::Failed(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Ask GitHub who is signed in while the first board loads from the
    /// remembered login. Another answer (the account was renamed) reloads
    /// the board as that login; a token GitHub refuses opens the sign-in
    /// screen. Anything else is left to the board request to report.
    fn verify_login(&mut self, setup_epoch: u64, cx: &mut Context<Self>) {
        let connector = self.connector.clone();
        let host = self
            .attention
            .as_ref()
            .map(|attention| attention.snapshots.namespace().host.clone());
        let remembered = self.me.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    connector.verify_login().map(|login| {
                        // Another account's marks, read off the UI thread.
                        let attention = (remembered.as_deref() != Some(login.as_str()))
                            .then_some(host)
                            .flatten()
                            .map(|host| {
                                AttentionState::load(SnapshotNamespace::new(host, login.clone()))
                            });
                        (login, attention)
                    })
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.setup_epoch != setup_epoch || state.setup != SetupStatus::Ready {
                    return;
                }
                match result {
                    Ok((login, attention)) if state.me.as_deref() != Some(login.as_str()) => {
                        if let Some(attention) = attention {
                            state.attention = Some(Arc::new(attention));
                        }
                        state.me = Some(login);
                        state.cache.clear();
                        state.reset_and_refetch(cx);
                    }
                    Err(GhError::NotAuthenticated) => {
                        state.setup = SetupStatus::NotAuthenticated;
                        cx.notify();
                    }
                    _ => {}
                }
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

    /// Resolves when [`Self::take_platform_event`] has something new.
    pub fn platform_event_signal(&self) -> Arc<crate::platform::EventSignal> {
        self.platform.event_signal()
    }

    pub fn check_notification_permission(&self) {
        self.platform.check_notification_permission();
    }

    pub fn request_notification_permission(&self) {
        self.platform.request_notification_permission();
    }

    /// Which PRs are changed, watched and snoozed, gathered once for a pass
    /// over the board; empty before sign-in.
    pub fn marks(&self) -> Marks<'_> {
        self.attention
            .as_ref()
            .map(|attention| attention.marks())
            .unwrap_or_default()
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
                format!("{}#{} · {}", watch.repo, watch.number, watch.status.word()),
            )
        })
    }

    pub fn snooze_description(&self, pr_id: &str) -> Option<String> {
        self.attention
            .as_ref()?
            .snooze(pr_id)
            .map(|snooze| snooze.description(crate::table::local_offset_secs()))
    }

    pub fn acknowledge(&mut self, pr_id: &str, cx: &mut Context<Self>) {
        if self
            .attention
            .as_mut()
            .map(Arc::make_mut)
            .is_some_and(|attention| attention.snapshots.acknowledge(pr_id))
        {
            self.schedule_persist(cx);
            self.generation += 1;
            cx.notify();
        }
    }

    pub fn toggle_watch(&mut self, row: &BoardRow, cx: &mut Context<Self>) -> Option<String> {
        let attention = self.attention.as_mut().map(Arc::make_mut)?;
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
        if let Some(attention) = self.attention.as_mut().map(Arc::make_mut) {
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
            .map(Arc::make_mut)
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
            .map(Arc::make_mut)
            .map(|attention| attention.wake_due(&self.rows, Utc::now()))
            .unwrap_or_default();
        if !woke.is_empty() {
            self.schedule_persist(cx);
            self.update_badge();
            self.generation += 1;
            cx.notify();
        }
    }

    /// Select one of the dashboards. The previous queue's rows are stashed
    /// and the target queue is restored from cache immediately (if seen
    /// before) so the table never flashes empty; a background refresh then
    /// updates it in place (critique #1), unless it synced in the last
    /// [`FRESH_ON_SWITCH`] or its refresh is still running.
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.stash_current();
        self.mode = mode;
        self.error = None;
        self.access = AccessGaps::default(); // the refresh says again
        let queue = self.cache.remove(&mode).unwrap_or_default();
        self.rows = queue.rows;
        self.last_synced = queue.last_synced;
        self.truncated = queue.truncated;
        self.pagination = queue.pagination;
        self.total = queue.total;
        self.rows_filter = queue.rows_filter;
        self.generation += 1; // observers push the restored (or empty) rows
        let fresh = self.last_synced.is_some_and(|synced| {
            (Local::now() - synced)
                .to_std()
                .is_ok_and(|age| age < FRESH_ON_SWITCH)
        }) && (mode != Mode::AllOpen || self.rows_filter == self.remote_filter);
        if !fresh {
            self.refresh(cx); // background-update the queue we just switched to
        }
        cx.notify();
    }

    fn reset_and_refetch(&mut self, cx: &mut Context<Self>) {
        self.rows.clear();
        self.last_synced = None;
        self.error = None;
        self.truncated = false;
        self.total = None;
        self.rows_filter = RemoteFilter::default();
        self.access = AccessGaps::default();
        self.pagination = BoardPagination::default();
        self.generation += 1; // observers push the (empty) rows to the table
        self.invalidate_all(); // in-flight fetches are for the old scope
        self.refresh(cx);
        cx.notify();
    }

    /// How long the refresh timer sleeps: until the open view's last refresh
    /// is `interval` old, or a whole `interval` when it already is (a back-off
    /// or a running fetch held the last one up).
    pub fn refresh_wait(&self, interval: Duration) -> Duration {
        match self.fetch_started[slot(self.mode)] {
            Some(started) if started.elapsed() < interval => interval - started.elapsed(),
            _ => interval,
        }
    }

    /// The timer's refresh: only when the open view's last one started at
    /// least `interval` ago (less a second for timer slack), so a view switch
    /// or a manual refresh moves the next one back instead of being followed
    /// by another.
    pub fn refresh_if_due(&mut self, interval: Duration, cx: &mut Context<Self>) {
        let due = self.fetch_started[slot(self.mode)]
            .is_none_or(|started| started.elapsed() + Duration::from_secs(1) >= interval);
        if due {
            self.refresh(cx);
        }
    }

    /// Seconds until the next allowed fetch while backing off after a
    /// rate-limit hit, or `None` if a fetch could run now. Lets the UI show a
    /// "paused — retrying in Xm" state instead of a permanent "Loading…" when a
    /// switch to an unseen queue lands inside a back-off window.
    pub fn backoff_remaining(&self) -> Option<u64> {
        let now = Local::now().timestamp().max(0) as u64;
        self.backoff_until.filter(|&u| u > now).map(|u| u - now)
    }

    /// False while backing off after a rate-limit hit, and when the budget is
    /// down to the reserve kept for the person's own `gh` and `git` use, which
    /// starts a back-off and says so.
    fn budget_allows(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Local::now().timestamp().max(0) as u64;
        if let Some(until) = self.backoff_until {
            if now < until {
                return false; // quietly wait out the backoff window
            }
            self.backoff_until = None;
        }
        let Some(rate) = &self.rate else {
            return true;
        };
        let Some(until) = rate.pause_until(now) else {
            return true;
        };
        self.backoff_until = Some(until);
        self.pause_reason = PauseReason::BudgetLow {
            remaining: rate.remaining,
        };
        cx.notify();
        false
    }

    /// The paused line, counting down, while fetching waits out a rate
    /// limit or the budget's reserve.
    pub fn pause_text(&self) -> Option<String> {
        self.backoff_remaining()
            .map(|secs| paused_text(self.pause_reason, secs))
    }

    /// Kick off one fetch unless one is in flight or we are backing off.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.setup != SetupStatus::Ready || self.syncing() {
            return;
        }
        if self.mode == Mode::AllOpen && self.scope.is_all() {
            return; // unreachable from the UI; the body says to pick one
        }
        let Some(me) = self.me.clone() else {
            return;
        };
        if !self.budget_allows(cx) {
            return;
        }
        let mode = self.mode;
        let view = slot(mode);
        self.in_flight[view] = true;
        self.fetch_started[view] = Some(Instant::now());
        self.error = None;
        cx.notify();

        let transport = self.transport.clone();
        let scope = self.scope.clone();
        let config = self.config.clone();
        let epoch = self.epochs[view];
        let filter = if mode == Mode::AllOpen {
            self.remote_filter.clone()
        } else {
            RemoteFilter::default()
        };
        let sent_filter = filter.clone();
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
        // A followed PR the board already shows needs only its state, as does
        // a watch known to be closed or gone: its full row would cost as much
        // as a search result on the request GitHub cuts off at ~10 s.
        let on_board: HashSet<&str> = self.rows.iter().map(|row| row.id.as_str()).collect();
        let (tracked_status, tracked_rows): (Vec<String>, Vec<String>) =
            tracked_ids.into_iter().partition(|id| {
                on_board.contains(id.as_str())
                    || self
                        .attention
                        .as_ref()
                        .and_then(|attention| attention.watch(id))
                        .is_some_and(|watch| {
                            matches!(
                                watch.status,
                                TrackedStatus::Closed | TrackedStatus::Inaccessible
                            )
                        })
            });
        // A view GitHub could not answer in time keeps its smaller pages; the
        // flag lives in the view's pagination, so it lasts until a scope
        // change resets it or the app quits.
        let small_pages = self.pagination.small_pages();

        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    // The `gh` subprocess blocks; that is fine on the
                    // background pool for a call made every few minutes.
                    let tracked = Tracked {
                        rows: &tracked_rows,
                        status: &tracked_status,
                    };
                    fetch_view(
                        transport.as_ref(),
                        mode,
                        &scope,
                        &me,
                        &config,
                        &filter,
                        tracked,
                        small_pages,
                    )
                })
                .await;

            let _ = this.update(cx, |state, cx| {
                if state.epochs[view] != epoch {
                    return; // the scope or filter changed; a newer fetch owns it
                }
                state.in_flight[view] = false;
                match fetched {
                    Ok(board) => state.accept_refresh(mode, board, sent_filter, cx),
                    Err(error) => state.fetch_failed(mode, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A refresh's answer: observed, then shown, or kept in the cache when
    /// the person has switched to another view since it was asked for.
    fn accept_refresh(
        &mut self,
        mode: Mode,
        mut board: BoardFetch,
        sent_filter: RemoteFilter,
        cx: &mut Context<Self>,
    ) {
        self.keep_known_conflicts(&mut board.rows, mode);
        for tracked in &mut board.tracked {
            if let Some(row) = tracked.row.as_mut() {
                // Tracked rows read as in Involving me: your PRs as
                // authored, anyone else's as another author's.
                self.keep_known_conflicts(std::slice::from_mut(row), Mode::Authored);
            }
        }
        self.tracked_loaded = board.tracked.len();
        self.process_tracked(&board.tracked, cx);
        let shown: HashSet<&str> = board.rows.iter().map(|row| row.id.as_str()).collect();
        let observed: Vec<&BoardRow> = board
            .rows
            .iter()
            .chain(
                board
                    .tracked
                    .iter()
                    .filter_map(|tracked| tracked.row.as_ref())
                    .filter(|row| !shown.contains(row.id.as_str())),
            )
            .collect();
        self.process_accepted_rows(&observed, mode, cx);
        self.rate = board.rate;
        let synced = Some(Local::now());
        if mode == self.mode {
            self.rows = board.rows;
            self.truncated = board.truncated;
            self.total = board.total;
            self.rows_filter = sent_filter;
            self.access = board.access;
            // A deliberate refresh starts again at page one; load-more
            // cursors are only preserved by queue cache restoration.
            self.pagination = board.pagination;
            self.last_synced = synced;
            self.generation += 1;
        } else {
            self.cache_queue(
                mode,
                CachedQueue {
                    rows: board.rows,
                    last_synced: synced,
                    truncated: board.truncated,
                    pagination: board.pagination,
                    total: board.total,
                    rows_filter: sent_filter,
                },
            );
        }
        self.update_badge();
    }

    /// A refresh or Load more failed. A rate limit pauses every view; a token
    /// GitHub refuses opens the sign-in screen; a timeout makes that view ask
    /// for smaller pages from now on. Other errors are shown only on the view
    /// they belong to.
    fn fetch_failed(&mut self, mode: Mode, error: GhError) {
        match error {
            GhError::RateLimited {
                reset_epoch,
                retry_after_secs,
            } => {
                let now = Local::now().timestamp().max(0) as u64;
                let wait = rate_limited_wait_secs(reset_epoch, retry_after_secs, now);
                self.backoff_until = Some(now + wait);
                self.pause_reason = PauseReason::RateLimited;
            }
            GhError::NotAuthenticated => self.setup = SetupStatus::NotAuthenticated,
            error => {
                if error.is_query_timeout() {
                    // GitHub gave up even on the small page: the next try
                    // starts small rather than full again.
                    if mode == self.mode {
                        self.pagination.mark_small_pages();
                    } else if let Some(queue) = self.cache.get_mut(&mode) {
                        queue.pagination.mark_small_pages();
                    }
                }
                if mode == self.mode {
                    self.error = Some(error.to_string());
                }
            }
        }
    }

    pub fn can_load_more(&self) -> bool {
        !self.syncing()
            && self.backoff_remaining().is_none()
            && self.pagination.can_load_more(self.mode)
    }

    /// Whether GitHub has another page for this view, whether or not a
    /// fetch is running right now.
    pub fn pagination_can_load_more(&self) -> bool {
        self.pagination.can_load_more(self.mode)
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
        let Some(me) = self.me.clone() else {
            return;
        };
        if !self.budget_allows(cx) {
            return;
        }
        let mode = self.mode;
        let view = slot(mode);
        self.in_flight[view] = true;
        self.error = None;
        cx.notify();
        let transport = self.transport.clone();
        let scope = self.scope.clone();
        let config = self.config.clone();
        let epoch = self.epochs[view];
        let current = BoardFetch {
            rows: self.rows.clone(),
            rate: self.rate.clone(),
            truncated: self.truncated,
            pagination: self.pagination.clone(),
            tracked: Vec::new(),
            access: self.access,
            total: self.total,
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
                if state.epochs[view] != epoch {
                    return;
                }
                state.in_flight[view] = false;
                match fetched {
                    Ok(board) => state.accept_more(mode, board, cx),
                    Err(error) => state.fetch_failed(mode, error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Load more's answer: the earlier rows plus the new page. Loading older
    /// pages does not refresh the earlier rows, so the sync time stays.
    fn accept_more(&mut self, mode: Mode, mut board: BoardFetch, cx: &mut Context<Self>) {
        self.keep_known_conflicts(&mut board.rows, mode);
        let observed: Vec<&BoardRow> = board.rows.iter().collect();
        self.process_accepted_rows(&observed, mode, cx);
        self.rate = board.rate;
        if mode == self.mode {
            let added = board.rows.len().saturating_sub(self.rows.len());
            self.rows = board.rows;
            self.truncated = board.truncated;
            self.total = board.total;
            self.access = board.access;
            self.pagination = board.pagination;
            self.generation += 1;
            self.loaded_more = Some((self.generation, added));
        } else if let Some(queue) = self.cache.get_mut(&mode) {
            // The person switched views while it loaded: the page joins the
            // view it was asked for.
            queue.rows = board.rows;
            queue.truncated = board.truncated;
            queue.total = board.total;
            queue.pagination = board.pagination;
        }
        self.update_badge();
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
        let Some(attention) = self.attention.as_mut().map(Arc::make_mut) else {
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
            let update = attention.apply_tracked_status(&tracked.pr_id, status);
            dirty |= update.changed();
            let first = self.status_baselines.insert(tracked.pr_id.clone());
            if first
                || update.changed_from.is_none()
                || !self.attention_preferences.notifications
                || update.was_snoozed
                || self.focused_selected_pr.as_deref() == Some(&tracked.pr_id)
            {
                continue;
            }
            let Some(watch) = attention.watch(&tracked.pr_id) else {
                continue;
            };
            if let Some((title, body)) =
                watched_status_notice(tracked.status, &watch.repo, watch.number, &watch.title)
            {
                notices.push((
                    title.to_owned(),
                    body,
                    tracked.pr_id.clone(),
                    watch.url.clone(),
                ));
            }
        }
        for (title, body, pr_id, url) in notices {
            self.platform.notify(
                title,
                body,
                pr_id,
                url,
                self.attention_preferences.notification_sound,
            );
        }
        if dirty {
            self.schedule_persist(cx);
        }
    }

    /// The demo's attention markers on the first three rows: one changed
    /// since you looked, one watched, one snoozed.
    fn seed_demo_attention(&mut self, rows: &[&BoardRow]) {
        let Some(attention) = self.attention.as_mut().map(Arc::make_mut) else {
            return;
        };
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
    }

    fn process_accepted_rows(&mut self, rows: &[&BoardRow], mode: Mode, cx: &mut Context<Self>) {
        if self.demo_attention {
            self.demo_attention = false;
            self.seed_demo_attention(rows);
        }
        let Some(attention) = self.attention.as_mut().map(Arc::make_mut) else {
            return;
        };
        // All open records only the PRs that already have a snapshot or that
        // involve you (`AttentionState::observes`, shared with the iPad).
        let observed: Vec<&BoardRow> = rows
            .iter()
            .copied()
            .filter(|row| attention.observes(mode, row))
            .collect();
        let woke = attention.wake_due(rows, Utc::now());
        let mut dirty = !woke.is_empty();
        let mut notices = Vec::new();
        for row in observed {
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
            let case = NotifyCase {
                notifications: self.attention_preferences.notifications,
                all_needs_action: self.attention_preferences.notify_all_needs_action,
                me: self.me.as_deref().unwrap_or_default(),
                watched: attention.is_watched(&row.id),
                snoozed: attention.snooze(&row.id).is_some(),
                focused: self.focused_selected_pr.as_deref() == Some(&row.id),
                needed_action_before: previous.as_ref().map(semantic_needs_action),
            };
            if !should_notify(&case, row) {
                continue;
            }
            if let Some(notice) = semantic_notice(previous.as_ref(), row) {
                notices.push((notice.title, notice.body, row.id.clone(), row.url.clone()));
            }
        }
        for (title, body, pr_id, url) in notices {
            self.platform.notify(
                title,
                body,
                pr_id,
                url,
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
                Mode::AllOpen => return,
            }
            // The header's rule, so the badge and "need you" can't drift
            // apart: your own PRs under Needs action, reviews asked of you.
            for row in rows {
                if prmarmot_core::status::row_needs_you(row) && !snoozed(&row.id) {
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
                    if let Some(attention) = state.attention.as_mut().map(Arc::make_mut) {
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

/// Where a notification's "Open pull request" goes when its PR isn't shown
/// on the board (the other queue, a filter, a collapsed group, or gone
/// since): the browser, with a line saying so. Returns the URL to open and
/// that line. `watched` is `AppState::watched_fallback`'s answer.
pub fn off_board_click(url: String, watched: Option<(String, String)>) -> (String, String) {
    match watched {
        Some((url, status)) => (url, format!("Watched PR opened · {status}")),
        None => (url, "Opened in the browser: it isn't shown here".into()),
    }
}

fn loaded_badge_sources<'a>(
    active_mode: Mode,
    active_rows: &'a [BoardRow],
    active_loaded: bool,
    cache: &'a HashMap<Mode, CachedQueue>,
) -> Vec<(Mode, &'a [BoardRow])> {
    // All open adds nothing: the badge counts your PRs and reviews asked of
    // you, which My PRs and the review queue already hold, not everyone's.
    let mut sources = Vec::with_capacity(MAX_CACHED_QUEUES);
    if active_loaded && active_mode != Mode::AllOpen {
        sources.push((active_mode, active_rows));
    }
    sources.extend(cache.iter().filter_map(|(mode, cached)| {
        (*mode != active_mode && *mode != Mode::AllOpen && cached.last_synced.is_some())
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
    fn a_notification_for_a_pr_off_the_board_still_opens_it() {
        let url = "https://github.com/acme/widgets/pull/7".to_owned();
        assert_eq!(
            off_board_click(url.clone(), None),
            (
                url.clone(),
                "Opened in the browser: it isn't shown here".into()
            ),
            "neither loaded nor watched: this used to do nothing"
        );
        assert_eq!(
            off_board_click(
                url.clone(),
                Some((url.clone(), "acme/widgets#7 · Merged".into()))
            ),
            (url, "Watched PR opened · acme/widgets#7 · Merged".into())
        );
    }

    #[test]
    fn a_host_from_the_sign_in_screen_is_where_the_app_connects() {
        let mut warnings = Vec::new();
        let file = prmarmot_local::config::FileConfig::default();
        let settings =
            prmarmot_local::config::auth_settings(&file, Some("github.com"), None, &mut warnings);
        let connector = ConfiguredConnector::new(settings);
        assert!(
            !connector.use_host("github.com"),
            "same host, nothing changes"
        );
        assert!(connector.use_host("https://ghe.acme.test/"));
        let now = connector.settings();
        assert_eq!(now.host, "ghe.acme.test");
        assert!(
            now.client_id_is_placeholder(),
            "github.com's app ID stays behind"
        );
        assert!(connector.use_host("github.com"));
        assert!(!connector.settings().client_id_is_placeholder());
    }

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
        for mode in [Mode::Authored, Mode::Review, Mode::AllOpen] {
            cache.insert(
                mode,
                CachedQueue {
                    rows: Vec::new(),
                    last_synced: Some(Local::now()),
                    truncated: false,
                    pagination: BoardPagination::default(),
                    total: None,
                    rows_filter: RemoteFilter::default(),
                },
            );
        }
        assert_eq!(cache.len(), MAX_CACHED_QUEUES);

        let active_rows = Vec::new();
        let sources = loaded_badge_sources(Mode::Authored, &active_rows, true, &cache);
        assert_eq!(
            sources.iter().map(|(mode, _)| *mode).collect::<Vec<_>>(),
            vec![Mode::Authored, Mode::Review],
            "All open adds nothing to the badge"
        );
        let sources = loaded_badge_sources(Mode::AllOpen, &active_rows, true, &cache);
        let mut modes = sources.iter().map(|(mode, _)| *mode).collect::<Vec<_>>();
        modes.sort_by_key(|mode| *mode as u8);
        assert_eq!(modes, vec![Mode::Authored, Mode::Review]);
    }
}
