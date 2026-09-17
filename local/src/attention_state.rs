//! Durable, bounded attention state. Preferences stay in config TOML; mutable
//! snapshots, watches and snoozes live under XDG_STATE_HOME and are replaced
//! atomically. A corrupt or newer file is locked against writes for this run.
//!
//! The desktop app is the only writer. The CLI loads this file read-only and
//! never calls [`AttentionState::save`], so the two can run side by side.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use prmarmot_core::attention::{Observation, SnapshotNamespace, SnapshotStore};
use prmarmot_core::board::{BoardRow, Ci};
use serde::{Deserialize, Serialize};

pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const MAX_WATCHES: usize = 50;
pub const MAX_SNOOZES: usize = 200;
const MAX_STATE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub pr_id: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub status: TrackedStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackedStatus {
    #[default]
    Open,
    Closed,
    Merged,
    Inaccessible,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snooze {
    pub pr_id: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub condition: SnoozeCondition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnoozeCondition {
    Until {
        deadline: DateTime<Utc>,
    },
    WaitingPerson {
        login: String,
        baseline_submitted_at: Option<String>,
    },
    WaitingCi {
        baseline: ObservedCi,
    },
    ReviewAgainChanged {
        baseline: ReviewRelevant,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRelevant {
    head_oid: Option<String>,
    review_decision: Option<String>,
    requested: Vec<String>,
    reviews: Vec<(Option<String>, String, Option<String>)>,
    unresolved: usize,
    draft: bool,
}

impl ReviewRelevant {
    fn from_row(row: &BoardRow) -> Self {
        let mut requested = row.requested.clone();
        requested.sort();
        let mut reviews = row
            .reviews
            .iter()
            .map(|review| {
                (
                    review.login.clone(),
                    review.state.clone(),
                    review.submitted_at.clone(),
                )
            })
            .collect::<Vec<_>>();
        reviews.sort();
        Self {
            head_oid: row.head_oid.clone(),
            review_decision: row.review_decision.clone(),
            requested,
            reviews,
            unresolved: row.unresolved,
            draft: row.draft,
        }
    }
}

impl Snooze {
    pub fn description(&self) -> String {
        match &self.condition {
            SnoozeCondition::Until { deadline } => {
                format!("Snoozed until {} UTC", deadline.format("%Y-%m-%d %H:%M"))
            }
            SnoozeCondition::WaitingPerson { login, .. } => format!("Waiting on {login}"),
            SnoozeCondition::WaitingCi { .. } => "Waiting for CI to finish".into(),
            SnoozeCondition::ReviewAgainChanged { .. } => "Review again when changed".into(),
        }
    }

    /// Missing GitHub evidence never wakes a conditional snooze. Timed snoozes
    /// are evaluated from UTC alone, so restart and machine sleep are harmless.
    pub fn should_wake(&self, row: Option<&BoardRow>, now: DateTime<Utc>) -> bool {
        match &self.condition {
            SnoozeCondition::Until { deadline } => now >= *deadline,
            SnoozeCondition::WaitingPerson {
                login,
                baseline_submitted_at,
            } => row.is_some_and(|row| {
                row.reviews.iter().any(|review| {
                    review.login.as_deref() == Some(login)
                        && review.submitted_at.is_some()
                        && review.submitted_at > *baseline_submitted_at
                })
            }),
            SnoozeCondition::WaitingCi { baseline } => row.is_some_and(|row| {
                let current = ObservedCi::from(row.ci);
                current != *baseline && matches!(current, ObservedCi::Pass | ObservedCi::Fail)
            }),
            SnoozeCondition::ReviewAgainChanged { baseline } => {
                row.is_some_and(|row| ReviewRelevant::from_row(row) != *baseline)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AttentionState {
    pub snapshots: SnapshotStore,
    pub watches: VecDeque<Watch>,
    pub snoozes: VecDeque<Snooze>,
    pub storage_error: Option<String>,
    writable: bool,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    schema_version: u32,
    namespace: SnapshotNamespace,
    snapshots: serde_json::Value,
    #[serde(default)]
    watches: VecDeque<Watch>,
    #[serde(default)]
    snoozes: VecDeque<Snooze>,
}

impl AttentionState {
    pub fn empty(namespace: SnapshotNamespace) -> Self {
        Self {
            snapshots: SnapshotStore::new(namespace),
            watches: VecDeque::new(),
            snoozes: VecDeque::new(),
            storage_error: None,
            writable: true,
        }
    }

    pub fn load(namespace: SnapshotNamespace) -> Self {
        let path = state_path(&namespace);
        match read_bounded(&path) {
            Ok(bytes) => match Self::from_bytes(namespace.clone(), &bytes) {
                Ok(state) => state,
                Err(error) => Self {
                    storage_error: Some(format!(
                        "Attention state wasn't loaded; {} is preserved: {error}",
                        path.display()
                    )),
                    writable: false,
                    ..Self::empty(namespace)
                },
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::empty(namespace),
            Err(error) => Self {
                storage_error: Some(format!("Couldn't read {}: {error}", path.display())),
                writable: false,
                ..Self::empty(namespace)
            },
        }
    }

    /// Restore what `to_bytes` wrote. Public because a front end that keeps
    /// this file somewhere of its own (the iPad, in its app container) needs
    /// the same validation the desktop's `load` gets.
    pub fn from_bytes(namespace: SnapshotNamespace, bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(format!("file is too large ({} bytes)", bytes.len()));
        }
        let envelope: Envelope = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if envelope.schema_version != STATE_SCHEMA_VERSION {
            return Err(format!("unsupported schema {}", envelope.schema_version));
        }
        if envelope.namespace != namespace {
            return Err("account/host namespace mismatch".into());
        }
        if envelope.watches.len() > MAX_WATCHES || envelope.snoozes.len() > MAX_SNOOZES {
            return Err("persisted collection exceeds its bound".into());
        }
        let snapshot_bytes = serde_json::to_vec(&envelope.snapshots).map_err(|e| e.to_string())?;
        let snapshots = SnapshotStore::from_json_slice(namespace, &snapshot_bytes)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            snapshots,
            watches: envelope.watches,
            snoozes: envelope.snoozes,
            storage_error: None,
            writable: true,
        })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let snapshot_bytes = self.snapshots.to_json_vec().map_err(|e| e.to_string())?;
        let snapshots = serde_json::from_slice(&snapshot_bytes).map_err(|e| e.to_string())?;
        serde_json::to_vec_pretty(&Envelope {
            schema_version: STATE_SCHEMA_VERSION,
            namespace: self.snapshots.namespace().clone(),
            snapshots,
            watches: self.watches.clone(),
            snoozes: self.snoozes.clone(),
        })
        .map_err(|e| e.to_string())
    }

    pub fn save(&self) -> Result<(), String> {
        if !self.writable {
            return Err(self
                .storage_error
                .clone()
                .unwrap_or_else(|| "attention state is read-only".into()));
        }
        let path = state_path(self.snapshots.namespace());
        atomic_write(&path, &self.to_bytes()?).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn is_changed(&self, pr_id: &str) -> bool {
        self.snapshots
            .snapshot(pr_id)
            .is_some_and(|snapshot| snapshot.changed_since_acknowledgement)
    }

    /// What changed since the PR was last acknowledged; empty if unchanged.
    pub fn change_summary(&self, pr_id: &str) -> Vec<String> {
        self.snapshots
            .snapshot(pr_id)
            .map(|snapshot| snapshot.change_summary())
            .unwrap_or_default()
    }

    pub fn is_watched(&self, pr_id: &str) -> bool {
        self.watches.iter().any(|watch| watch.pr_id == pr_id)
    }

    pub fn watch(&self, pr_id: &str) -> Option<&Watch> {
        self.watches.iter().find(|watch| watch.pr_id == pr_id)
    }

    pub fn tracked_ids(&self, limit: usize, offset: usize) -> (Vec<String>, usize) {
        let mut ids = Vec::new();
        for id in self
            .watches
            .iter()
            .map(|watch| &watch.pr_id)
            .chain(self.snoozes.iter().map(|snooze| &snooze.pr_id))
        {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let total = ids.len();
        if total <= limit {
            return (ids, total);
        }
        let selected = (0..limit)
            .map(|index| ids[(offset + index) % total].clone())
            .collect();
        (selected, total)
    }

    pub fn update_watch_status(
        &mut self,
        pr_id: &str,
        status: TrackedStatus,
    ) -> Option<TrackedStatus> {
        let watch = self.watches.iter_mut().find(|watch| watch.pr_id == pr_id)?;
        let previous = watch.status;
        watch.status = status;
        (previous != status).then_some(previous)
    }

    pub fn toggle_watch(&mut self, row: &BoardRow) -> Option<Watch> {
        if let Some(index) = self.watches.iter().position(|watch| watch.pr_id == row.id) {
            self.watches.remove(index);
            return None;
        }
        let evicted = (self.watches.len() == MAX_WATCHES)
            .then(|| self.watches.pop_front())
            .flatten();
        self.watches.push_back(Watch {
            pr_id: row.id.clone(),
            repo: row.repo.clone(),
            number: row.number,
            url: row.url.clone(),
            title: row.title.clone(),
            status: TrackedStatus::Open,
        });
        evicted
    }

    pub fn snooze(&self, pr_id: &str) -> Option<&Snooze> {
        self.snoozes.iter().find(|snooze| snooze.pr_id == pr_id)
    }

    pub fn set_snooze(&mut self, snooze: Snooze) {
        if let Some(index) = self
            .snoozes
            .iter()
            .position(|entry| entry.pr_id == snooze.pr_id)
        {
            self.snoozes.remove(index);
        } else if self.snoozes.len() == MAX_SNOOZES {
            self.snoozes.pop_front();
        }
        self.snoozes.push_back(snooze);
    }

    pub fn cancel_snooze(&mut self, pr_id: &str) -> bool {
        self.snoozes
            .iter()
            .position(|entry| entry.pr_id == pr_id)
            .and_then(|index| self.snoozes.remove(index))
            .is_some()
    }

    pub fn wake_due(&mut self, rows: &[BoardRow], now: DateTime<Utc>) -> Vec<String> {
        let mut woke = Vec::new();
        self.snoozes.retain(|snooze| {
            let row = rows.iter().find(|row| row.id == snooze.pr_id);
            if snooze.should_wake(row, now) {
                woke.push(snooze.pr_id.clone());
                false
            } else {
                true
            }
        });
        woke
    }

    pub fn snooze_for(row: &BoardRow, condition: SnoozeCondition) -> Snooze {
        Snooze {
            pr_id: row.id.clone(),
            repo: row.repo.clone(),
            number: row.number,
            title: row.title.clone(),
            created_at: Utc::now(),
            condition,
        }
    }

    pub fn waiting_ci(row: &BoardRow) -> SnoozeCondition {
        SnoozeCondition::WaitingCi {
            baseline: row.ci.into(),
        }
    }

    pub fn waiting_person(row: &BoardRow, login: String) -> SnoozeCondition {
        let baseline_submitted_at = row
            .reviews
            .iter()
            .filter(|review| review.login.as_deref() == Some(&login))
            .filter_map(|review| review.submitted_at.clone())
            .max();
        SnoozeCondition::WaitingPerson {
            login,
            baseline_submitted_at,
        }
    }

    pub fn review_again(row: &BoardRow) -> SnoozeCondition {
        SnoozeCondition::ReviewAgainChanged {
            baseline: ReviewRelevant::from_row(row),
        }
    }
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_STATE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file is too large (more than {MAX_STATE_BYTES} bytes)"),
        ));
    }
    Ok(bytes)
}

pub fn state_path(namespace: &SnapshotNamespace) -> PathBuf {
    let safe = |value: &str| {
        value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    crate::config::state_root().join(format!(
        "attention-{}-{}.json",
        safe(&namespace.host),
        safe(&namespace.account)
    ))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

pub fn observation(row: &BoardRow) -> Observation {
    Observation::from_row(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prmarmot_core::board::{Category, QueueProvenance, ReviewState, ReviewSummary};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn row(number: u64) -> BoardRow {
        BoardRow {
            id: format!("PR_{number}"),
            repo: "acme/widgets".into(),
            updated_at: Some("2026-09-11T10:00:00Z".into()),
            head_oid: Some("head-1".into()),
            reviewed_oid: None,
            reviewed_at: None,
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("PR {number}"),
            issue: None,
            issue_url: None,
            author: Some("alice".into()),
            stack: None,
            queue_provenance: Some(QueueProvenance::Requested),
            draft: false,
            category: Category::Todo,
            bug: false,
            labels: Vec::new(),
            ci: Ci::Running,
            conflict: false,
            mergeable_unknown: false,
            review_decision: None,
            review_state: ReviewState::Waiting,
            requested: vec!["me".into()],
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: String::new(),
            waiting_since: None,
            size: None,
            note: "needs your review".into(),
        }
    }

    #[test]
    fn newer_or_corrupt_state_is_read_only_and_preserved() {
        let namespace = SnapshotNamespace::new("github.com", "me");
        assert!(AttentionState::from_bytes(namespace.clone(), b"not json").is_err());
        let bytes = br#"{"schema_version":99,"namespace":{"host":"github.com","account":"me"},"snapshots":{},"watches":[],"snoozes":[]}"#;
        assert!(AttentionState::from_bytes(namespace, bytes)
            .unwrap_err()
            .contains("unsupported schema"));
    }

    #[test]
    fn empty_state_round_trips() {
        let namespace = SnapshotNamespace::new("github.com", "me");
        let state = AttentionState::empty(namespace.clone());
        let restored = AttentionState::from_bytes(namespace, &state.to_bytes().unwrap()).unwrap();
        assert!(restored.snapshots.is_empty());
        assert!(restored.watches.is_empty());
        assert!(restored.snoozes.is_empty());
    }

    #[test]
    fn bounded_read_rejects_oversized_files_without_reading_the_remainder() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "prmarmot-attention-state-{}-{unique}.json",
            std::process::id()
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_STATE_BYTES + 2]).unwrap();
        drop(file);

        let error = read_bounded(&path).unwrap_err();
        let _ = fs::remove_file(path);
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("file is too large"));
    }

    #[test]
    fn watches_are_fifo_bounded_and_tracked_ids_are_deduplicated() {
        let mut state = AttentionState::empty(SnapshotNamespace::new("github.com", "me"));
        for number in 0..MAX_WATCHES as u64 {
            assert!(state.toggle_watch(&row(number)).is_none());
        }
        let evicted = state.toggle_watch(&row(999)).unwrap();
        assert_eq!(evicted.pr_id, "PR_0");
        assert_eq!(state.watches.len(), MAX_WATCHES);
        state.set_snooze(AttentionState::snooze_for(
            &row(1000),
            AttentionState::review_again(&row(1000)),
        ));
        let (ids, total) = state.tracked_ids(MAX_WATCHES, 0);
        assert_eq!(ids.len(), MAX_WATCHES);
        assert_eq!(total, MAX_WATCHES + 1);
        let (rotated, _) = state.tracked_ids(MAX_WATCHES, MAX_WATCHES);
        assert_eq!(rotated.first().map(String::as_str), Some("PR_1000"));
    }

    #[test]
    fn conditional_snoozes_require_positive_evidence() {
        let base = row(1);
        let ci = AttentionState::snooze_for(&base, AttentionState::waiting_ci(&base));
        assert!(!ci.should_wake(None, Utc::now()));
        assert!(!ci.should_wake(Some(&base), Utc::now()));
        let mut passed = base.clone();
        passed.ci = Ci::Pass;
        assert!(ci.should_wake(Some(&passed), Utc::now()));

        let person =
            AttentionState::snooze_for(&base, AttentionState::waiting_person(&base, "bob".into()));
        let mut unrelated = base.clone();
        unrelated.reviews.push(ReviewSummary {
            login: Some("carol".into()),
            state: "APPROVED".into(),
            submitted_at: Some("2026-09-11T11:00:00Z".into()),
        });
        assert!(!person.should_wake(Some(&unrelated), Utc::now()));
        unrelated.reviews.push(ReviewSummary {
            login: Some("bob".into()),
            state: "COMMENTED".into(),
            submitted_at: Some("2026-09-11T11:00:00Z".into()),
        });
        assert!(person.should_wake(Some(&unrelated), Utc::now()));
    }

    #[test]
    fn timed_snooze_wakes_at_utc_deadline_without_a_row() {
        let deadline = Utc::now();
        let snooze = AttentionState::snooze_for(&row(1), SnoozeCondition::Until { deadline });
        assert!(snooze.should_wake(None, deadline));
        assert!(!snooze.should_wake(None, deadline - chrono::Duration::seconds(1)));
    }
}
