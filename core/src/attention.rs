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
}
