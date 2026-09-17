//! "Changed since you looked", across the boundary.
//!
//! `prmarmot-core` owns the rule for what counts as a change and which change
//! is worth telling someone about; this crate owns none of it. What the iPad
//! adds is the file: the store serializes to bytes and restores from bytes, so
//! Swift decides where that lives (its own container, iCloud later) without
//! Rust learning anything about iOS file system APIs.
//!
//! The store is namespaced by `(host, account)` on purpose. Restoring another
//! account's state is refused rather than merged — a "changed" marker from a
//! different person's view of a PR is not a fact about yours.

use std::sync::Mutex;

use prmarmot_core::attention as core_attention;

use crate::error::FfiError;
use crate::types::PullRequest;

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
}

/// The persisted "changed since you looked" state for one account.
///
/// Bounded at 1,000 PRs with FIFO eviction and at 16 MiB on disk, the same
/// bounds the desktop app runs under.
#[derive(uniffi::Object)]
pub struct AttentionStore {
    inner: Mutex<core_attention::SnapshotStore>,
}

#[uniffi::export]
impl AttentionStore {
    /// An empty store for `account` on `host`.
    #[uniffi::constructor]
    pub fn new(host: String, account: String) -> Self {
        Self {
            inner: Mutex::new(core_attention::SnapshotStore::new(
                core_attention::SnapshotNamespace::new(host, account),
            )),
        }
    }

    /// Restore what `to_bytes` wrote. Fails when the bytes are for another
    /// account, are too large, or do not validate — in which case start empty
    /// rather than half-trusting them.
    #[uniffi::constructor]
    pub fn from_bytes(host: String, account: String, bytes: Vec<u8>) -> Result<Self, FfiError> {
        let namespace = core_attention::SnapshotNamespace::new(host, account);
        let inner = core_attention::SnapshotStore::from_json_slice(namespace, &bytes)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// The whole store, for the caller to write wherever it keeps it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, FfiError> {
        Ok(self.lock().to_json_vec()?)
    }

    /// Record how a PR looks now, and say what that means.
    pub fn observe(&self, row: PullRequest) -> Result<ObserveResult, FfiError> {
        let row = row.into_row();
        let mut store = self.lock();
        let previous = store
            .snapshot(&row.id)
            .map(|snapshot| snapshot.latest.clone());
        let notice = core_attention::semantic_notice(previous.as_ref(), &row);
        let result = store
            .observe(row.id.clone(), core_attention::Observation::from_row(&row))
            .map_err(FfiError::invalid)?;
        let changes = store
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
        })
    }

    /// Mark a PR as seen. Returns whether anything changed, so the caller can
    /// skip writing an identical file.
    pub fn acknowledge(&self, pr_id: String) -> bool {
        self.lock().acknowledge(&pr_id)
    }

    /// Whether a PR is currently marked changed.
    pub fn is_changed(&self, pr_id: String) -> bool {
        self.lock()
            .snapshot(&pr_id)
            .is_some_and(|snapshot| snapshot.changed_since_acknowledgement)
    }

    /// What differs from the acknowledged observation, in short phrases.
    pub fn changes(&self, pr_id: String) -> Vec<String> {
        self.lock()
            .snapshot(&pr_id)
            .map(|snapshot| snapshot.change_summary())
            .unwrap_or_default()
    }

    /// How many PRs the store is holding.
    pub fn count(&self) -> u32 {
        self.lock().len() as u32
    }
}

impl AttentionStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, core_attention::SnapshotStore> {
        self.inner.lock().expect("attention store")
    }
}
