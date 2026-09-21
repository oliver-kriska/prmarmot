//! Watch, snooze and "changed since you looked", across the boundary.
//!
//! None of the rules are here. `prmarmot-core` decides what counts as a change
//! and which change is worth telling someone about; `prmarmot-local` owns the
//! watch list, the snooze conditions and the file format. This module is the
//! doorway, and the one thing it adds is that the bytes go in and out instead
//! of to a path: Swift decides where the file lives without Rust learning
//! anything about iOS containers.
//!
//! The store is namespaced by `(host, account)` on purpose. Restoring another
//! account's state is refused rather than merged — a "changed" marker from a
//! different person's view of a PR is not a fact about yours.

use std::sync::Mutex;

use chrono::{DateTime, TimeZone, Utc};
use prmarmot_core::attention as core_attention;
use prmarmot_core::board as core_board;
use prmarmot_local::attention_state as local_attention;

use crate::error::FfiError;
use crate::types::{BoardSettings, Mode, PullRequest};

/// What a change means for the person, most important first. The wording is
/// each front end's own; this is the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NoticeKind {
    MergeConflict,
    ChangesRequested,
    ReviewAgain,
    CiPassed,
    Changed,
}

impl From<core_attention::NoticeKind> for NoticeKind {
    fn from(kind: core_attention::NoticeKind) -> Self {
        match kind {
            core_attention::NoticeKind::MergeConflict => Self::MergeConflict,
            core_attention::NoticeKind::ChangesRequested => Self::ChangesRequested,
            core_attention::NoticeKind::ReviewAgain => Self::ReviewAgain,
            core_attention::NoticeKind::CiPassed => Self::CiPassed,
            core_attention::NoticeKind::Changed => Self::Changed,
        }
    }
}

/// The one most important thing that changed about a PR, ready to become a
/// notification.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
}

/// What observing a PR did to the store.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum Observed {
    /// First sighting: a baseline, never a change.
    Baseline,
    /// Something differs from what was last seen.
    Changed {
        /// True when a fact changed, not only GitHub's `updatedAt`.
        semantic: bool,
    },
    Unchanged,
}

/// One PR's place in the store after observing it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ObserveResult {
    pub observed: Observed,
    /// Whether this PR is currently marked changed.
    pub changed: bool,
    /// What differs from the acknowledged observation, in short phrases:
    /// "New commits", "CI passing → failing", "alice approved".
    pub changes: Vec<String>,
    /// A PR dropped from the store to stay under the 1,000-entry bound.
    pub evicted_pr_id: Option<String>,
    /// The notice worth showing, when there is one.
    pub notice: Option<Notice>,
    /// Whether the observation *before* this one already needed the author to
    /// act. `None` on a first sighting. This is what tells "it just started
    /// needing you" from "it has needed you all along", which is the whole
    /// difference between a useful notification and a nag.
    pub needed_action_before: Option<bool>,
}

/// Whether a watched PR is still there. GitHub can close, merge or hide one
/// while it is being watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WatchStatus {
    Open,
    Closed,
    Merged,
    Inaccessible,
    Unknown,
}

impl From<local_attention::TrackedStatus> for WatchStatus {
    fn from(status: local_attention::TrackedStatus) -> Self {
        match status {
            local_attention::TrackedStatus::Open => Self::Open,
            local_attention::TrackedStatus::Closed => Self::Closed,
            local_attention::TrackedStatus::Merged => Self::Merged,
            local_attention::TrackedStatus::Inaccessible => Self::Inaccessible,
            local_attention::TrackedStatus::Unknown => Self::Unknown,
        }
    }
}

impl From<WatchStatus> for local_attention::TrackedStatus {
    fn from(status: WatchStatus) -> Self {
        match status {
            WatchStatus::Open => Self::Open,
            WatchStatus::Closed => Self::Closed,
            WatchStatus::Merged => Self::Merged,
            WatchStatus::Inaccessible => Self::Inaccessible,
            WatchStatus::Unknown => Self::Unknown,
        }
    }
}

/// A watched PR, enough of it to draw a row without having fetched the board
/// it came from.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WatchedPr {
    pub pr_id: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub status: WatchStatus,
}

impl From<&local_attention::Watch> for WatchedPr {
    fn from(watch: &local_attention::Watch) -> Self {
        Self {
            pr_id: watch.pr_id.clone(),
            repo: watch.repo.clone(),
            number: watch.number,
            url: watch.url.clone(),
            title: watch.title.clone(),
            status: watch.status.into(),
        }
    }
}

/// What the user picked from the snooze menu. The deadline arithmetic for the
/// timed ones is here rather than in Swift so both front ends mean the same
/// thing by "until tomorrow".
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SnoozeChoice {
    /// The desktop's "One hour".
    OneHour,
    /// The desktop's "Until tomorrow": twenty-four hours, not midnight.
    UntilTomorrow,
    /// Wake when this person submits a review newer than the one they had.
    WaitingPerson { login: String },
    /// Wake when CI reaches a terminal state different from today's.
    WaitingCi,
    /// Wake when anything a reviewer would care about changes.
    ReviewAgainWhenChanged,
}

/// What the snooze menu says for `choice`: the desktop's words, from the one
/// place both front ends take them.
#[uniffi::export]
pub fn snooze_choice_label(choice: SnoozeChoice) -> String {
    use local_attention::{
        waiting_on, SNOOZE_ONE_HOUR, SNOOZE_REVIEW_AGAIN, SNOOZE_UNTIL_TOMORROW, SNOOZE_WAITING_CI,
    };
    match choice {
        SnoozeChoice::OneHour => SNOOZE_ONE_HOUR.into(),
        SnoozeChoice::UntilTomorrow => SNOOZE_UNTIL_TOMORROW.into(),
        SnoozeChoice::WaitingPerson { login } => waiting_on(&login),
        SnoozeChoice::WaitingCi => SNOOZE_WAITING_CI.into(),
        SnoozeChoice::ReviewAgainWhenChanged => SNOOZE_REVIEW_AGAIN.into(),
    }
}

/// "Cancel snooze", as the desktop's menu says it.
#[uniffi::export]
pub fn cancel_snooze_label() -> String {
    local_attention::SNOOZE_CANCEL.into()
}

/// A snoozed PR and the sentence describing why.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SnoozedPr {
    pub pr_id: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub created_at_epoch: i64,
    /// The desktop's wording: "Snoozed until 2026-09-18 11:00" (in the
    /// reader's time zone), "Waiting on alice", "Waiting for CI to finish",
    /// "Review again when changed".
    pub description: String,
}

impl SnoozedPr {
    fn new(snooze: &local_attention::Snooze, tz_offset_secs: i32) -> Self {
        Self {
            pr_id: snooze.pr_id.clone(),
            repo: snooze.repo.clone(),
            number: snooze.number,
            title: snooze.title.clone(),
            created_at_epoch: snooze.created_at.timestamp(),
            description: snooze.description(tz_offset_secs),
        }
    }
}

/// The result of pressing `w`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WatchToggle {
    /// True when the PR is now watched, false when the press removed it.
    pub watching: bool,
    /// The oldest watch, dropped to stay under the fifty-watch bound.
    pub evicted: Option<WatchedPr>,
}

/// Which PRs to refresh outside the board, and how many there are in total.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TrackedIds {
    pub ids: Vec<String>,
    pub total: u32,
}

/// The persisted attention state for one account: what changed since you
/// looked, what you are watching, and what is snoozed.
///
/// Bounded exactly as the desktop bounds it — 1,000 snapshots, 50 watches,
/// 200 snoozes, 20 MiB on disk — because it is the desktop's code.
#[derive(uniffi::Object)]
pub struct AttentionStore {
    inner: Mutex<local_attention::AttentionState>,
}

#[uniffi::export]
impl AttentionStore {
    /// An empty store for `account` on `host`.
    #[uniffi::constructor]
    pub fn new(host: String, account: String) -> Self {
        Self {
            inner: Mutex::new(local_attention::AttentionState::empty(namespace(
                host, account,
            ))),
        }
    }

    /// Restore what `to_bytes` wrote. Fails when the bytes are for another
    /// account, are too large, or do not validate — in which case start empty
    /// rather than half-trusting them, and tell the person: the desktop
    /// preserves the file and says so in the footer, and so must this.
    #[uniffi::constructor]
    pub fn from_bytes(host: String, account: String, bytes: Vec<u8>) -> Result<Self, FfiError> {
        let inner = local_attention::AttentionState::from_bytes(namespace(host, account), &bytes)
            .map_err(FfiError::invalid)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// The whole store, for the caller to write wherever it keeps it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, FfiError> {
        self.lock().to_bytes().map_err(FfiError::invalid)
    }

    /// Record how a PR looks now, and say what that means.
    pub fn observe(&self, row: PullRequest) -> Result<ObserveResult, FfiError> {
        let row = row.into_row();
        let mut state = self.lock();
        let previous = state
            .snapshots
            .snapshot(&row.id)
            .map(|snapshot| snapshot.latest.clone());
        let notice = core_attention::semantic_notice(previous.as_ref(), &row);
        let needed_action_before = previous.as_ref().map(core_attention::semantic_needs_action);
        let result = state
            .snapshots
            .observe(row.id.clone(), core_attention::Observation::from_row(&row))
            .map_err(FfiError::invalid)?;
        let changes = state
            .snapshots
            .snapshot(&row.id)
            .map(|snapshot| snapshot.change_summary())
            .unwrap_or_default();
        Ok(ObserveResult {
            observed: match result.kind {
                core_attention::ObservationKind::Baseline => Observed::Baseline,
                core_attention::ObservationKind::Changed {
                    semantic_transition,
                } => Observed::Changed {
                    semantic: semantic_transition,
                },
                core_attention::ObservationKind::Unchanged => Observed::Unchanged,
            },
            changed: result.changed_since_acknowledgement,
            changes,
            evicted_pr_id: result.evicted_pr_id,
            notice: notice.map(|notice| Notice {
                kind: notice.kind.into(),
                title: notice.title,
                body: notice.body,
            }),
            needed_action_before,
        })
    }

    /// A PR this store last saw conflicting stays conflicting while GitHub
    /// reports its mergeability as unknown, with the Note and the category
    /// that go with a conflict. GitHub reports unknown for a while after a
    /// push to the base branch; without this the marker flickers off and on
    /// between refreshes, and `observe` records a conflict resolved that
    /// never was. The desktop's `keep_known_conflicts`, through core's
    /// `carry_forward_conflicts` (FEATURES.md F-note-11).
    ///
    /// Call it on every fetched board, before `observe`, with the `now_epoch`
    /// and settings of the fetch. `mode` is the board's; tracked rows read as
    /// in Involving me, so pass `Mode::Authored` for them, as the desktop
    /// does. Whose PR a row is comes from the store's account.
    pub fn keep_known_conflicts(
        &self,
        rows: Vec<PullRequest>,
        mode: Mode,
        settings: BoardSettings,
        now_epoch: i64,
    ) -> Result<Vec<PullRequest>, FfiError> {
        let cfg = settings.to_core()?;
        let now = crate::types::instant(now_epoch)?;
        let mut rows = crate::types::into_rows(rows);
        let state = self.lock();
        let me = state.snapshots.namespace().account.clone();
        core_board::carry_forward_conflicts(
            &mut rows,
            |id| state.snapshots.last_conflict(id),
            mode.into(),
            &me,
            &cfg,
        );
        Ok(rows
            .iter()
            .map(|row| PullRequest::from_row(row, now, settings.stale_after_days))
            .collect())
    }

    /// Mark a PR as seen. Returns whether anything changed, so the caller can
    /// skip writing an identical file.
    pub fn acknowledge(&self, pr_id: String) -> bool {
        self.lock().snapshots.acknowledge(&pr_id)
    }

    /// Whether a PR is currently marked changed.
    pub fn is_changed(&self, pr_id: String) -> bool {
        self.lock().is_changed(&pr_id)
    }

    /// What differs from the acknowledged observation, in short phrases.
    pub fn changes(&self, pr_id: String) -> Vec<String> {
        self.lock().change_summary(&pr_id)
    }

    /// How many PRs the snapshot store is holding.
    pub fn count(&self) -> u32 {
        self.lock().snapshots.len() as u32
    }

    // -- Watch ------------------------------------------------------------

    pub fn is_watched(&self, pr_id: String) -> bool {
        self.lock().is_watched(&pr_id)
    }

    /// Press `w`: watch an unwatched PR, unwatch a watched one.
    pub fn toggle_watch(&self, row: PullRequest) -> WatchToggle {
        let row = row.into_row();
        let mut state = self.lock();
        let was_watched = state.is_watched(&row.id);
        let evicted = state.toggle_watch(&row);
        WatchToggle {
            watching: !was_watched,
            evicted: evicted.as_ref().map(WatchedPr::from),
        }
    }

    /// Stop watching a PR from the list of watches, where there may be no row
    /// to toggle: it merged, closed, or is outside the tracked rotation.
    /// False when it was not being watched.
    pub fn unwatch(&self, pr_id: String) -> bool {
        self.lock().unwatch(&pr_id).is_some()
    }

    /// Everything being watched, oldest first — the order the bound evicts in.
    pub fn watches(&self) -> Vec<WatchedPr> {
        self.lock().watches.iter().map(WatchedPr::from).collect()
    }

    /// Record that a watched PR closed, merged or became invisible. Returns
    /// true when this is news.
    pub fn update_watch_status(&self, pr_id: String, status: WatchStatus) -> bool {
        self.lock()
            .update_watch_status(&pr_id, status.into())
            .is_some()
    }

    // -- Snooze -----------------------------------------------------------

    pub fn is_snoozed(&self, pr_id: String) -> bool {
        self.lock().snooze(&pr_id).is_some()
    }

    /// The sentence for a snoozed PR, or nothing when it is awake.
    /// `tz_offset_secs` is the reader's offset from UTC, as for
    /// `detail_lines`: a deadline reads in their time zone.
    pub fn snooze_description(&self, pr_id: String, tz_offset_secs: i32) -> Option<String> {
        self.lock()
            .snooze(&pr_id)
            .map(|snooze| snooze.description(tz_offset_secs))
    }

    /// Who "Waiting on …" would wait for: the row's author, or `None` on your
    /// own PR or one with no author, because your own reviews never reach a
    /// row and that snooze could never wake. Offer
    /// `SnoozeChoice::WaitingPerson` only when this is `Some`, as the desktop
    /// does.
    pub fn waiting_on(&self, row: PullRequest) -> Option<String> {
        let row = row.into_row();
        let me = self.lock().snapshots.namespace().account.clone();
        local_attention::AttentionState::waiting_on_author(&row, &me).and(row.author)
    }

    /// Snooze a row. `now_epoch` is the caller's clock: core owns none.
    pub fn snooze(&self, row: PullRequest, choice: SnoozeChoice, now_epoch: i64) {
        let row = row.into_row();
        let now = instant(now_epoch);
        let condition = match choice {
            SnoozeChoice::OneHour => local_attention::SnoozeCondition::Until {
                deadline: now + chrono::Duration::hours(1),
            },
            SnoozeChoice::UntilTomorrow => local_attention::SnoozeCondition::Until {
                deadline: now + chrono::Duration::hours(24),
            },
            SnoozeChoice::WaitingPerson { login } => {
                local_attention::AttentionState::waiting_person(&row, login)
            }
            SnoozeChoice::WaitingCi => local_attention::AttentionState::waiting_ci(&row),
            SnoozeChoice::ReviewAgainWhenChanged => {
                local_attention::AttentionState::review_again(&row)
            }
        };
        let mut snooze = local_attention::AttentionState::snooze_for(&row, condition);
        // `snooze_for` stamps `Utc::now()`; the caller's clock wins so a test
        // (and a fixed-clock UI test) is deterministic.
        snooze.created_at = now;
        self.lock().set_snooze(snooze);
    }

    /// Wake a snoozed PR by hand.
    pub fn cancel_snooze(&self, pr_id: String) -> bool {
        self.lock().cancel_snooze(&pr_id)
    }

    /// Everything snoozed, oldest first, each described in the reader's time
    /// zone (`tz_offset_secs` east of UTC).
    pub fn snoozes(&self, tz_offset_secs: i32) -> Vec<SnoozedPr> {
        self.lock()
            .snoozes
            .iter()
            .map(|snooze| SnoozedPr::new(snooze, tz_offset_secs))
            .collect()
    }

    /// Wake whatever this refresh's rows say should wake. Returns the PR ids
    /// that woke, for the caller to mention.
    pub fn wake_due(&self, rows: Vec<PullRequest>, now_epoch: i64) -> Vec<String> {
        let rows: Vec<_> = rows.into_iter().map(PullRequest::into_row).collect();
        self.lock().wake_due(&rows, instant(now_epoch))
    }

    // -- Refreshing what is not on the board -------------------------------

    /// The watched and snoozed PRs to fetch alongside the board, round-robin
    /// through `offset` so a long list is covered over several refreshes.
    pub fn tracked_ids(&self, limit: u32, offset: u32) -> TrackedIds {
        let (ids, total) = self.lock().tracked_ids(limit as usize, offset as usize);
        TrackedIds {
            ids,
            total: total as u32,
        }
    }

    /// Set when the state file could not be read and is being preserved. The
    /// footer says this; nothing overwrites the file after it.
    pub fn storage_error(&self) -> Option<String> {
        self.lock().storage_error.clone()
    }
}

impl AttentionStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, local_attention::AttentionState> {
        self.inner.lock().expect("attention store")
    }
}

fn namespace(host: String, account: String) -> core_attention::SnapshotNamespace {
    core_attention::SnapshotNamespace::new(host, account)
}

fn instant(epoch: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(epoch, 0)
        .single()
        .unwrap_or_else(Utc::now)
}
