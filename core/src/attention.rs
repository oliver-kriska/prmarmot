//! Pure, bounded tracking of PR observations and acknowledgements.
//!
//! This module owns no filesystem or UI behavior. Callers supply the authenticated
//! host/account namespace, persist [`SnapshotStore::to_json_vec`] wherever they
//! choose, and restore through [`SnapshotStore::from_json_slice`] so persisted
//! input is bounded and validated.

use std::collections::{HashSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::board::{BoardRow, Ci};

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 2;
pub const MAX_SNAPSHOTS: usize = 1_000;
pub const MAX_PERSISTED_BYTES: usize = 16 * 1024 * 1024;

const MAX_ID_BYTES: usize = 512;
const MAX_NAMESPACE_BYTES: usize = 255;
const MAX_FACT_BYTES: usize = 512;
const MAX_LIST_ITEMS: usize = 100;

/// Separates state belonging to different GitHub hosts or authenticated users.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotNamespace {
    pub host: String,
    pub account: String,
}

impl SnapshotNamespace {
    pub fn new(host: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            account: account.into(),
        }
    }
}

/// The structured facts used to decide whether a transition is semantic.
/// Display-only fields such as title and Note text are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticObservation {
    pub head_oid: Option<String>,
    pub draft: bool,
    pub ci: ObservedCi,
    pub conflict: bool,
    pub review_decision: Option<String>,
    pub requested: Vec<String>,
    pub reviews: Vec<ObservedReview>,
    pub unresolved: usize,
    pub reviewed_oid: Option<String>,
    pub reviewed_at: Option<String>,
}

/// A complete poll observation. `updated_at` affects the changed marker but is
/// excluded from semantic transition comparisons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub updated_at: Option<String>,
    pub semantic: SemanticObservation,
}

impl Observation {
    pub fn from_row(row: &BoardRow) -> Self {
        let mut requested = row.requested.clone();
        requested.sort();
        requested.dedup();

        let mut reviews: Vec<_> = row
            .reviews
            .iter()
            .map(|review| ObservedReview {
                login: review.login.clone(),
                state: review.state.clone(),
            })
            .collect();
        reviews.sort_by(|a, b| (&a.login, &a.state).cmp(&(&b.login, &b.state)));
        reviews.dedup();

        Self {
            updated_at: row.updated_at.clone(),
            semantic: SemanticObservation {
                head_oid: row.head_oid.clone(),
                draft: row.draft,
                ci: row.ci.into(),
                conflict: row.conflict,
                review_decision: row.review_decision.clone(),
                requested,
                reviews,
                unresolved: row.unresolved,
                reviewed_oid: row.reviewed_oid.clone(),
                reviewed_at: row.reviewed_at.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedCi {
    Pass,
    Fail,
    None,
    Running,
}

impl From<Ci> for ObservedCi {
    fn from(value: Ci) -> Self {
        match value {
            Ci::Pass => Self::Pass,
            Ci::Fail => Self::Fail,
            Ci::None => Self::None,
            Ci::Running => Self::Running,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservedReview {
    pub login: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub pr_id: String,
    pub latest: Observation,
    pub acknowledged: Observation,
    /// Latched so a change followed by a revert remains unseen until acknowledged.
    pub changed_since_acknowledgement: bool,
    /// Latched independently from updatedAt-only changes.
    pub semantic_change_since_acknowledgement: bool,
}

impl Snapshot {
    /// What differs from the acknowledged observation, as short phrases for
    /// the changed marker ("New commits", "CI passing → failing", "alice
    /// approved"). Empty unless the PR is marked changed. Compares the two
    /// endpoints only; the latched flags cover a change that was reverted.
    pub fn change_summary(&self) -> Vec<String> {
        if !self.changed_since_acknowledgement {
            return Vec::new();
        }
        let (old, new) = (&self.acknowledged.semantic, &self.latest.semantic);
        let mut changes = Vec::new();
        if old.head_oid != new.head_oid {
            changes.push("New commits".to_string());
        }
        if old.draft != new.draft {
            changes.push(if new.draft {
                "Converted to draft".to_string()
            } else {
                "Marked ready for review".to_string()
            });
        }
        if old.ci != new.ci {
            changes.push(format!("CI {} → {}", old.ci.word(), new.ci.word()));
        }
        if old.conflict != new.conflict {
            changes.push(if new.conflict {
                "Merge conflict".to_string()
            } else {
                "Merge conflict resolved".to_string()
            });
        }
        let new_reviews: Vec<&ObservedReview> = new
            .reviews
            .iter()
            .filter(|review| !old.reviews.contains(review))
            .collect();
        for review in &new_reviews {
            let login = review.login.as_deref().unwrap_or("deleted user");
            changes.push(match review.state.as_str() {
                "APPROVED" => format!("{login} approved"),
                "CHANGES_REQUESTED" => format!("{login} requested changes"),
                "COMMENTED" => format!("{login} commented"),
                "DISMISSED" => format!("{login}'s review was dismissed"),
                _ => format!("{login} reviewed"),
            });
        }
        // A new review usually explains a changed decision on its own.
        if new_reviews.is_empty() && old.review_decision != new.review_decision {
            changes.push(format!(
                "Review decision {} → {}",
                decision_word(old.review_decision.as_deref()),
                decision_word(new.review_decision.as_deref())
            ));
        }
        let added: Vec<&str> = new
            .requested
            .iter()
            .filter(|login| !old.requested.contains(login))
            .map(String::as_str)
            .collect();
        if !added.is_empty() {
            changes.push(format!("Review requested from {}", added.join(", ")));
        }
        // A request that disappears because the reviewer reviewed is already
        // described by their review.
        let removed: Vec<&str> = old
            .requested
            .iter()
            .filter(|login| {
                !new.requested.contains(login)
                    && !new_reviews
                        .iter()
                        .any(|review| review.login.as_deref() == Some(login.as_str()))
            })
            .map(String::as_str)
            .collect();
        if !removed.is_empty() {
            changes.push(format!("Review request removed: {}", removed.join(", ")));
        }
        if old.unresolved != new.unresolved {
            changes.push(format!(
                "Unresolved threads {} → {}",
                old.unresolved, new.unresolved
            ));
        }
        if new_reviews.is_empty() && new.reviewed_at.is_some() && old.reviewed_at != new.reviewed_at
        {
            changes.push("You reviewed".to_string());
        }
        if changes.is_empty() {
            changes.push(if self.semantic_change_since_acknowledgement {
                "Changed on GitHub, then changed back".to_string()
            } else {
                "Updated on GitHub (a comment, edit, or label)".to_string()
            });
        }
        changes
    }
}

impl ObservedCi {
    fn word(self) -> &'static str {
        match self {
            Self::Pass => "passing",
            Self::Fail => "failing",
            Self::Running => "running",
            Self::None => "no checks",
        }
    }
}

fn decision_word(decision: Option<&str>) -> String {
    match decision {
        None => "none".to_string(),
        Some("APPROVED") => "approved".to_string(),
        Some("CHANGES_REQUESTED") => "changes requested".to_string(),
        Some("REVIEW_REQUIRED") => "review required".to_string(),
        Some(other) => other.to_lowercase().replace('_', " "),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationKind {
    Baseline,
    Unchanged,
    Changed { semantic_transition: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserveResult {
    pub kind: ObservationKind,
    pub changed_since_acknowledgement: bool,
    pub semantic_change_since_acknowledgement: bool,
    pub evicted_pr_id: Option<String>,
}

/// FIFO-bounded state for one authenticated host/account namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotStore {
    schema_version: u32,
    namespace: SnapshotNamespace,
    snapshots: VecDeque<Snapshot>,
}

impl SnapshotStore {
    pub fn new(namespace: SnapshotNamespace) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            namespace,
            snapshots: VecDeque::new(),
        }
    }

    pub fn namespace(&self) -> &SnapshotNamespace {
        &self.namespace
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    pub fn snapshot(&self, pr_id: &str) -> Option<&Snapshot> {
        self.snapshots.iter().find(|entry| entry.pr_id == pr_id)
    }

    /// The merge-conflict state last observed for a PR, for
    /// [`crate::board::carry_forward_conflicts`].
    pub fn last_conflict(&self, pr_id: &str) -> Option<bool> {
        self.snapshot(pr_id)
            .map(|snapshot| snapshot.latest.semantic.conflict)
    }

    pub fn observe(
        &mut self,
        pr_id: impl Into<String>,
        observation: Observation,
    ) -> Result<ObserveResult, ValidationError> {
        let pr_id = pr_id.into();
        validate_string("pr_id", &pr_id, MAX_ID_BYTES)?;
        validate_observation(&observation)?;

        if let Some(snapshot) = self.snapshots.iter_mut().find(|entry| entry.pr_id == pr_id) {
            if snapshot.latest == observation {
                return Ok(result(ObservationKind::Unchanged, snapshot, None));
            }

            let semantic_transition = snapshot.latest.semantic != observation.semantic;
            snapshot.latest = observation;
            snapshot.changed_since_acknowledgement = true;
            snapshot.semantic_change_since_acknowledgement |= semantic_transition;

            return Ok(result(
                ObservationKind::Changed {
                    semantic_transition,
                },
                snapshot,
                None,
            ));
        }

        let evicted_pr_id = if self.snapshots.len() == MAX_SNAPSHOTS {
            self.snapshots.pop_front().map(|entry| entry.pr_id)
        } else {
            None
        };

        self.snapshots.push_back(Snapshot {
            pr_id,
            latest: observation.clone(),
            acknowledged: observation,
            changed_since_acknowledgement: false,
            semantic_change_since_acknowledgement: false,
        });

        let snapshot = self.snapshots.back().expect("snapshot was just inserted");
        Ok(result(ObservationKind::Baseline, snapshot, evicted_pr_id))
    }

    /// Acknowledges the latest observation without changing FIFO order. Returns
    /// whether persisted state changed, so callers can skip redundant writes.
    pub fn acknowledge(&mut self, pr_id: &str) -> bool {
        let Some(snapshot) = self.snapshots.iter_mut().find(|entry| entry.pr_id == pr_id) else {
            return false;
        };

        if snapshot.latest == snapshot.acknowledged
            && !snapshot.changed_since_acknowledgement
            && !snapshot.semantic_change_since_acknowledgement
        {
            return false;
        }

        snapshot.acknowledged = snapshot.latest.clone();
        snapshot.changed_since_acknowledgement = false;
        snapshot.semantic_change_since_acknowledgement = false;
        true
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchema(self.schema_version));
        }
        validate_namespace(&self.namespace)?;
        if self.snapshots.len() > MAX_SNAPSHOTS {
            return Err(ValidationError::TooManySnapshots(self.snapshots.len()));
        }

        let mut ids = HashSet::with_capacity(self.snapshots.len());
        for snapshot in &self.snapshots {
            validate_string("pr_id", &snapshot.pr_id, MAX_ID_BYTES)?;
            if !ids.insert(snapshot.pr_id.as_str()) {
                return Err(ValidationError::DuplicatePrId(snapshot.pr_id.clone()));
            }
            validate_observation(&snapshot.latest)?;
            validate_observation(&snapshot.acknowledged)?;
            if snapshot.semantic_change_since_acknowledgement
                && !snapshot.changed_since_acknowledgement
            {
                return Err(ValidationError::InconsistentSnapshot(
                    snapshot.pr_id.clone(),
                ));
            }
            if snapshot.latest != snapshot.acknowledged && !snapshot.changed_since_acknowledgement {
                return Err(ValidationError::InconsistentSnapshot(
                    snapshot.pr_id.clone(),
                ));
            }
        }
        Ok(())
    }

    pub fn to_json_vec(&self) -> Result<Vec<u8>, PersistError> {
        self.validate().map_err(PersistError::Validation)?;
        let bytes = serde_json::to_vec(self).map_err(PersistError::Json)?;
        if bytes.len() > MAX_PERSISTED_BYTES {
            return Err(PersistError::OutputTooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    /// Restores only bounded, validated state for the expected authenticated
    /// namespace. The byte limit is checked before serde allocates nested values.
    pub fn from_json_slice(
        expected_namespace: SnapshotNamespace,
        bytes: &[u8],
    ) -> Result<Self, RestoreError> {
        if bytes.len() > MAX_PERSISTED_BYTES {
            return Err(RestoreError::InputTooLarge(bytes.len()));
        }

        let wire: PersistedStore = serde_json::from_slice(bytes).map_err(RestoreError::Json)?;
        let store = Self {
            schema_version: wire.schema_version,
            namespace: wire.namespace,
            snapshots: wire.snapshots,
        };
        store.validate().map_err(RestoreError::Validation)?;
        if store.namespace != expected_namespace {
            return Err(RestoreError::NamespaceMismatch {
                expected: expected_namespace,
                actual: store.namespace,
            });
        }
        Ok(store)
    }
}

#[derive(Deserialize)]
struct PersistedStore {
    schema_version: u32,
    namespace: SnapshotNamespace,
    snapshots: VecDeque<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    UnsupportedSchema(u32),
    TooManySnapshots(usize),
    TooManyItems { field: &'static str, count: usize },
    StringTooLong { field: &'static str, bytes: usize },
    DuplicatePrId(String),
    InconsistentSnapshot(String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid snapshot state: {self:?}")
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug)]
pub enum RestoreError {
    InputTooLarge(usize),
    Json(serde_json::Error),
    Validation(ValidationError),
    NamespaceMismatch {
        expected: SnapshotNamespace,
        actual: SnapshotNamespace,
    },
}

#[derive(Debug)]
pub enum PersistError {
    OutputTooLarge(usize),
    Json(serde_json::Error),
    Validation(ValidationError),
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputTooLarge(bytes) => {
                write!(f, "serialized snapshot state is too large ({bytes} bytes)")
            }
            Self::Json(error) => write!(f, "could not serialize snapshot state: {error}"),
            Self::Validation(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Validation(error) => Some(error),
            Self::OutputTooLarge(_) => None,
        }
    }
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge(bytes) => write!(f, "snapshot state is too large ({bytes} bytes)"),
            Self::Json(error) => write!(f, "invalid snapshot JSON: {error}"),
            Self::Validation(error) => error.fmt(f),
            Self::NamespaceMismatch { expected, actual } => write!(
                f,
                "snapshot namespace mismatch: expected {expected:?}, found {actual:?}"
            ),
        }
    }
}

impl std::error::Error for RestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Validation(error) => Some(error),
            Self::InputTooLarge(_) | Self::NamespaceMismatch { .. } => None,
        }
    }
}

fn result(
    kind: ObservationKind,
    snapshot: &Snapshot,
    evicted_pr_id: Option<String>,
) -> ObserveResult {
    ObserveResult {
        kind,
        changed_since_acknowledgement: snapshot.changed_since_acknowledgement,
        semantic_change_since_acknowledgement: snapshot.semantic_change_since_acknowledgement,
        evicted_pr_id,
    }
}

fn validate_namespace(namespace: &SnapshotNamespace) -> Result<(), ValidationError> {
    validate_string("namespace.host", &namespace.host, MAX_NAMESPACE_BYTES)?;
    validate_string("namespace.account", &namespace.account, MAX_NAMESPACE_BYTES)
}

fn validate_observation(observation: &Observation) -> Result<(), ValidationError> {
    validate_optional_string("updated_at", observation.updated_at.as_deref())?;
    let semantic = &observation.semantic;
    validate_optional_string("head_oid", semantic.head_oid.as_deref())?;
    validate_optional_string("review_decision", semantic.review_decision.as_deref())?;
    validate_optional_string("reviewed_oid", semantic.reviewed_oid.as_deref())?;
    validate_optional_string("reviewed_at", semantic.reviewed_at.as_deref())?;
    validate_list("requested", &semantic.requested, |value| {
        validate_string("requested[]", value, MAX_FACT_BYTES)
    })?;
    if semantic.reviews.len() > MAX_LIST_ITEMS {
        return Err(ValidationError::TooManyItems {
            field: "reviews",
            count: semantic.reviews.len(),
        });
    }
    for review in &semantic.reviews {
        validate_optional_string("reviews[].login", review.login.as_deref())?;
        validate_string("reviews[].state", &review.state, MAX_FACT_BYTES)?;
    }
    Ok(())
}

fn validate_optional_string(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), ValidationError> {
    if let Some(value) = value {
        validate_string(field, value, MAX_FACT_BYTES)?;
    }
    Ok(())
}

fn validate_string(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ValidationError> {
    if value.len() > max_bytes {
        return Err(ValidationError::StringTooLong {
            field,
            bytes: value.len(),
        });
    }
    Ok(())
}

fn validate_list<T>(
    field: &'static str,
    values: &[T],
    mut validate_item: impl FnMut(&T) -> Result<(), ValidationError>,
) -> Result<(), ValidationError> {
    if values.len() > MAX_LIST_ITEMS {
        return Err(ValidationError::TooManyItems {
            field,
            count: values.len(),
        });
    }
    for value in values {
        validate_item(value)?;
    }
    Ok(())
}

/// What a semantic transition means for the person, in the order the app has
/// always prioritized them. Shared by desktop notifications and CLI events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    MergeConflict,
    ChangesRequested,
    ReviewAgain,
    CiPassed,
    Changed,
}

impl NoticeKind {
    /// Stable machine key for JSON output.
    pub fn key(&self) -> &'static str {
        match self {
            Self::MergeConflict => "merge_conflict",
            Self::ChangesRequested => "changes_requested",
            Self::ReviewAgain => "review_again",
            Self::CiPassed => "ci_passed",
            Self::Changed => "changed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
}

/// True when an observation carries something the author must act on.
pub fn semantic_needs_action(observation: &Observation) -> bool {
    let semantic = &observation.semantic;
    semantic.conflict
        || semantic.ci == ObservedCi::Fail
        || semantic.review_decision.as_deref() == Some("CHANGES_REQUESTED")
        || semantic.unresolved > 0
}

/// The one most important thing that changed between `previous` and `row`.
/// `None` without a previous observation: a first sighting is a baseline.
pub fn semantic_notice(previous: Option<&Observation>, row: &BoardRow) -> Option<Notice> {
    let old = &previous?.semantic;
    let notice = |kind, title: &str, body: String| {
        Some(Notice {
            kind,
            title: title.to_owned(),
            body,
        })
    };
    if !old.conflict && row.conflict {
        return notice(
            NoticeKind::MergeConflict,
            "Merge conflict",
            format!(
                "{} #{} now conflicts with its base branch",
                row.repo, row.number
            ),
        );
    }
    if old.review_decision.as_deref() != Some("CHANGES_REQUESTED")
        && row.review_decision.as_deref() == Some("CHANGES_REQUESTED")
    {
        return notice(
            NoticeKind::ChangesRequested,
            "Needs you — changes requested",
            format!("{} #{} · {}", row.repo, row.number, row.title),
        );
    }
    if old.head_oid != row.head_oid
        && row.reviewed_oid.is_some()
        && row.reviewed_oid != row.head_oid
    {
        return notice(
            NoticeKind::ReviewAgain,
            "Review again — new commits",
            format!("{} #{} changed since your review", row.repo, row.number),
        );
    }
    if old.ci != ObservedCi::Pass && row.ci == Ci::Pass {
        let approval = if row.review_decision.as_deref() == Some("APPROVED") {
            " and is approved"
        } else {
            ""
        };
        return notice(
            NoticeKind::CiPassed,
            "Ready for you — CI passed",
            format!("{} #{} passed CI{approval}", row.repo, row.number),
        );
    }
    notice(
        NoticeKind::Changed,
        "Pull request changed",
        format!("{} #{} · {}", row.repo, row.number, row.title),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace(account: &str) -> SnapshotNamespace {
        SnapshotNamespace::new("github.com", account)
    }

    fn observation(updated_at: &str, ci: ObservedCi) -> Observation {
        Observation {
            updated_at: Some(updated_at.into()),
            semantic: SemanticObservation {
                head_oid: Some("oid-1".into()),
                draft: false,
                ci,
                conflict: false,
                review_decision: None,
                requested: vec!["reviewer".into()],
                reviews: Vec::new(),
                unresolved: 0,
                reviewed_oid: None,
                reviewed_at: None,
            },
        }
    }

    #[test]
    fn first_and_repeated_polls_are_not_changes() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        let first = observation("2026-09-11T10:00:00Z", ObservedCi::Running);

        let baseline = store.observe("PR_1", first.clone()).unwrap();
        assert_eq!(baseline.kind, ObservationKind::Baseline);
        assert!(!baseline.changed_since_acknowledgement);

        let repeat = store.observe("PR_1", first).unwrap();
        assert_eq!(repeat.kind, ObservationKind::Unchanged);
        assert!(!repeat.changed_since_acknowledgement);
    }

    #[test]
    fn updated_at_only_change_is_not_semantic_and_acknowledge_clears_it() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        let first = observation("2026-09-11T10:00:00Z", ObservedCi::Running);
        store.observe("PR_1", first.clone()).unwrap();

        let mut next = first;
        next.updated_at = Some("2026-09-11T10:01:00Z".into());
        let changed = store.observe("PR_1", next.clone()).unwrap();
        assert_eq!(
            changed.kind,
            ObservationKind::Changed {
                semantic_transition: false
            }
        );
        assert!(changed.changed_since_acknowledgement);
        assert!(!changed.semantic_change_since_acknowledgement);

        assert!(store.acknowledge("PR_1"));
        let snapshot = store.snapshot("PR_1").unwrap();
        assert_eq!(snapshot.acknowledged, next);
        assert!(!snapshot.changed_since_acknowledgement);
        assert!(!store.acknowledge("PR_1"));
        assert!(!store.acknowledge("missing"));
    }

    #[test]
    fn display_derived_facts_are_not_part_of_the_semantic_snapshot() {
        let serialized =
            serde_json::to_value(observation("2026-09-11T10:00:00Z", ObservedCi::Running)).unwrap();
        let semantic = serialized.get("semantic").unwrap();
        for derived in ["category", "review_state", "my_review", "blockers"] {
            assert!(
                semantic.get(derived).is_none(),
                "{derived} must be scope independent"
            );
        }
    }

    #[test]
    fn two_changes_compare_each_poll_but_remain_latched_until_acknowledged() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:00:00Z", ObservedCi::Running),
            )
            .unwrap();

        let first_change = store
            .observe(
                "PR_1",
                observation("2026-09-11T10:01:00Z", ObservedCi::Pass),
            )
            .unwrap();
        assert_eq!(
            first_change.kind,
            ObservationKind::Changed {
                semantic_transition: true
            }
        );

        let mut second = observation("2026-09-11T10:02:00Z", ObservedCi::Pass);
        second.semantic.unresolved = 2;
        let second_change = store.observe("PR_1", second).unwrap();
        assert_eq!(
            second_change.kind,
            ObservationKind::Changed {
                semantic_transition: true
            }
        );
        assert!(second_change.semantic_change_since_acknowledgement);
    }

    #[test]
    fn change_then_revert_stays_changed_until_acknowledged() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        let baseline = observation("2026-09-11T10:00:00Z", ObservedCi::Running);
        store.observe("PR_1", baseline.clone()).unwrap();
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:01:00Z", ObservedCi::Fail),
            )
            .unwrap();

        let reverted = store.observe("PR_1", baseline).unwrap();
        assert!(reverted.changed_since_acknowledgement);
        assert!(reverted.semantic_change_since_acknowledgement);
        assert!(store.acknowledge("PR_1"));
        assert!(
            !store
                .snapshot("PR_1")
                .unwrap()
                .changed_since_acknowledgement
        );
    }

    #[test]
    fn insertion_evicts_the_oldest_snapshot() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        for index in 0..MAX_SNAPSHOTS {
            store
                .observe(
                    format!("PR_{index}"),
                    observation("2026-09-11T10:00:00Z", ObservedCi::Running),
                )
                .unwrap();
        }

        // Updating an old entry does not promote it in FIFO order.
        store
            .observe(
                "PR_0",
                observation("2026-09-11T10:01:00Z", ObservedCi::Pass),
            )
            .unwrap();
        let inserted = store
            .observe(
                "PR_new",
                observation("2026-09-11T10:00:00Z", ObservedCi::Running),
            )
            .unwrap();

        assert_eq!(inserted.evicted_pr_id.as_deref(), Some("PR_0"));
        assert_eq!(store.len(), MAX_SNAPSHOTS);
        assert!(store.snapshot("PR_0").is_none());
        assert!(store.snapshot("PR_new").is_some());
    }

    #[test]
    fn restoring_snapshots_preserves_unacknowledged_state_and_fifo_order() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:00:00Z", ObservedCi::Running),
            )
            .unwrap();
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:01:00Z", ObservedCi::Pass),
            )
            .unwrap();
        store
            .observe(
                "PR_2",
                observation("2026-09-11T10:00:00Z", ObservedCi::Running),
            )
            .unwrap();

        let json = store.to_json_vec().unwrap();
        let restored = SnapshotStore::from_json_slice(namespace("octocat"), &json).unwrap();

        assert_eq!(restored, store);
        assert!(
            restored
                .snapshot("PR_1")
                .unwrap()
                .semantic_change_since_acknowledgement
        );
    }

    #[test]
    fn restore_rejects_another_account_and_oversized_state() {
        let store = SnapshotStore::new(namespace("octocat"));
        let json = store.to_json_vec().unwrap();
        assert!(matches!(
            SnapshotStore::from_json_slice(namespace("hubot"), &json),
            Err(RestoreError::NamespaceMismatch { .. })
        ));

        let oversized = vec![b' '; MAX_PERSISTED_BYTES + 1];
        assert!(matches!(
            SnapshotStore::from_json_slice(namespace("octocat"), &oversized),
            Err(RestoreError::InputTooLarge(_))
        ));
    }

    #[test]
    fn restore_rejects_more_than_the_snapshot_bound() {
        let snapshot = Snapshot {
            pr_id: "PR_1".into(),
            latest: observation("2026-09-11T10:00:00Z", ObservedCi::Running),
            acknowledged: observation("2026-09-11T10:00:00Z", ObservedCi::Running),
            changed_since_acknowledgement: false,
            semantic_change_since_acknowledgement: false,
        };
        let value = serde_json::json!({
            "schema_version": SNAPSHOT_SCHEMA_VERSION,
            "namespace": namespace("octocat"),
            "snapshots": vec![snapshot; MAX_SNAPSHOTS + 1],
        });
        let json = serde_json::to_vec(&value).unwrap();

        assert!(matches!(
            SnapshotStore::from_json_slice(namespace("octocat"), &json),
            Err(RestoreError::Validation(ValidationError::TooManySnapshots(
                count
            ))) if count == MAX_SNAPSHOTS + 1
        ));
    }

    fn changed_snapshot(latest: Observation) -> Snapshot {
        let mut store = SnapshotStore::new(namespace("octocat"));
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:00:00Z", ObservedCi::Pass),
            )
            .unwrap();
        store.observe("PR_1", latest).unwrap();
        store.snapshot("PR_1").unwrap().clone()
    }

    #[test]
    fn change_summary_is_empty_until_marked_changed() {
        let mut store = SnapshotStore::new(namespace("octocat"));
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:00:00Z", ObservedCi::Pass),
            )
            .unwrap();
        assert!(store.snapshot("PR_1").unwrap().change_summary().is_empty());
    }

    #[test]
    fn change_summary_names_each_semantic_change() {
        let mut latest = observation("2026-09-11T10:05:00Z", ObservedCi::Fail);
        latest.semantic.head_oid = Some("oid-2".into());
        latest.semantic.conflict = true;
        latest.semantic.requested = vec!["bob".into()];
        latest.semantic.reviews = vec![ObservedReview {
            login: Some("reviewer".into()),
            state: "APPROVED".into(),
        }];
        latest.semantic.review_decision = Some("APPROVED".into());
        latest.semantic.unresolved = 2;
        assert_eq!(
            changed_snapshot(latest).change_summary(),
            vec![
                "New commits",
                "CI passing → failing",
                "Merge conflict",
                "reviewer approved",
                "Review requested from bob",
                "Unresolved threads 0 → 2",
            ]
        );
    }

    #[test]
    fn change_summary_reports_decisions_removed_requests_and_draft_flips() {
        let mut latest = observation("2026-09-11T10:05:00Z", ObservedCi::Pass);
        latest.semantic.draft = true;
        latest.semantic.requested = Vec::new();
        latest.semantic.review_decision = Some("REVIEW_REQUIRED".into());
        latest.semantic.reviewed_at = Some("2026-09-11T10:04:00Z".into());
        assert_eq!(
            changed_snapshot(latest).change_summary(),
            vec![
                "Converted to draft",
                "Review decision none → review required",
                "Review request removed: reviewer",
                "You reviewed",
            ]
        );
    }

    #[test]
    fn change_summary_falls_back_for_activity_and_reverts() {
        let activity = changed_snapshot(observation("2026-09-11T10:05:00Z", ObservedCi::Pass));
        assert_eq!(
            activity.change_summary(),
            vec!["Updated on GitHub (a comment, edit, or label)"]
        );

        let mut store = SnapshotStore::new(namespace("octocat"));
        let baseline = observation("2026-09-11T10:00:00Z", ObservedCi::Pass);
        store.observe("PR_1", baseline).unwrap();
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:01:00Z", ObservedCi::Fail),
            )
            .unwrap();
        store
            .observe(
                "PR_1",
                observation("2026-09-11T10:02:00Z", ObservedCi::Pass),
            )
            .unwrap();
        assert_eq!(
            store.snapshot("PR_1").unwrap().change_summary(),
            vec!["Changed on GitHub, then changed back"]
        );
    }

    fn board_row() -> BoardRow {
        BoardRow {
            id: "PR_7".into(),
            repo: "acme/widgets".into(),
            updated_at: Some("2026-09-11T10:00:00Z".into()),
            head_oid: Some("head-1".into()),
            reviewed_oid: None,
            reviewed_at: None,
            number: 7,
            url: "https://github.com/acme/widgets/pull/7".into(),
            title: "Tidy things".into(),
            issue: None,
            issue_url: None,
            author: Some("octocat".into()),
            stack: None,
            queue_provenance: None,
            draft: false,
            category: crate::board::Category::Await,
            bug: false,
            labels: Vec::new(),
            ci: Ci::Running,
            conflict: false,
            mergeable_unknown: false,
            review_decision: None,
            review_state: crate::board::ReviewState::Waiting,
            requested: Vec::new(),
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: "2026-09-01T10:00:00Z".into(),
            waiting_since: None,
            size: None,
            note: String::new(),
        }
    }

    #[test]
    fn notices_pick_the_single_most_important_transition() {
        let before = board_row();
        let previous = Observation::from_row(&before);
        assert_eq!(semantic_notice(None, &before), None, "first sighting");

        let mut after = before.clone();
        after.ci = Ci::Pass;
        after.review_decision = Some("APPROVED".into());
        let notice = semantic_notice(Some(&previous), &after).unwrap();
        assert_eq!(notice.kind, NoticeKind::CiPassed);
        assert_eq!(notice.kind.key(), "ci_passed");
        assert_eq!(notice.body, "acme/widgets #7 passed CI and is approved");

        after.conflict = true;
        after.review_decision = Some("CHANGES_REQUESTED".into());
        let notice = semantic_notice(Some(&previous), &after).unwrap();
        assert_eq!(notice.kind, NoticeKind::MergeConflict);
        assert!(semantic_needs_action(&Observation::from_row(&after)));

        after.conflict = false;
        assert_eq!(
            semantic_notice(Some(&previous), &after).unwrap().title,
            "Needs you — changes requested"
        );

        let mut rereview = before.clone();
        rereview.reviewed_oid = Some("head-1".into());
        let reviewed = Observation::from_row(&rereview);
        rereview.head_oid = Some("head-2".into());
        assert_eq!(
            semantic_notice(Some(&reviewed), &rereview).unwrap().kind,
            NoticeKind::ReviewAgain
        );

        let mut other = before.clone();
        other.unresolved = 2;
        assert_eq!(
            semantic_notice(Some(&previous), &other).unwrap().kind,
            NoticeKind::Changed
        );
        assert!(!semantic_needs_action(&previous));
    }
}
