//! The data behind a view: config and scope resolution (same precedence as the
//! app), one bounded GitHub fetch, and the read-only attention overlay.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use prmarmot_core::attention::{Observation, SnapshotNamespace};
use prmarmot_core::board::{
    fetch_board_scoped, fetch_more_board_scoped, BoardConfig, BoardFetch, BoardRow, BoardScope,
    Mode,
};
use prmarmot_core::github::rate_limit::RateLimitInfo;
use prmarmot_core::github::{GhError, GithubTransport};
use prmarmot_core::layout::{layout, LayoutItem};
use prmarmot_core::pickup::{is_stale, DEFAULT_STALE_AFTER_DAYS};
use prmarmot_local::attention_state::AttentionState;
use prmarmot_local::config::{self, FileConfig};

/// Everything resolved before talking to GitHub.
pub struct Setup {
    pub file: FileConfig,
    pub scope: BoardScope,
    pub board: BoardConfig,
    pub warnings: Vec<String>,
}

pub fn setup(cli_scope: Option<BoardScope>) -> Setup {
    let mut warnings = Vec::new();
    let file = config::try_load().unwrap_or_else(|warning| {
        warnings.push(warning);
        FileConfig::default()
    });
    let scope = config::resolve_scope(
        cli_scope,
        std::env::var("PRMARMOT_REPO").ok(),
        std::env::var("PRMARMOT_SCOPE").ok().as_deref(),
        &file,
    );
    let (board, warning) = config::board_config(&file);
    warnings.extend(warning);
    Setup {
        file,
        scope,
        board,
        warnings,
    }
}

pub fn github_host() -> String {
    std::env::var("GH_HOST")
        .ok()
        .filter(|host| !host.trim().is_empty())
        .unwrap_or_else(|| "github.com".into())
}

/// The app's attention file for this account, loaded for reading only.
pub fn attention(host: &str, login: &str) -> AttentionState {
    AttentionState::load(SnapshotNamespace::new(host, login))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Filters {
    pub changed: bool,
    pub watched: bool,
    /// Only PRs that have waited `stale_after_days` or longer for a reviewer.
    pub stale: bool,
    /// What stale means, also for each PR's `stale` mark.
    pub stale_after_days: u64,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            changed: false,
            watched: false,
            stale: false,
            stale_after_days: DEFAULT_STALE_AFTER_DAYS,
        }
    }
}

/// Per-row attention facts as the app would show them right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Marks {
    pub watched: bool,
    /// Snooze description while the snooze is still in effect.
    pub snoozed: Option<String>,
    pub changed: bool,
    pub changes: Vec<String>,
    /// Waited `stale_after_days` or longer for a reviewer.
    pub stale: bool,
}

/// A fetched, filtered, attention-annotated board ready to render.
pub struct BoardView {
    pub mode: Mode,
    pub scope: BoardScope,
    pub viewer: String,
    pub generated_at: DateTime<Utc>,
    pub rows: Vec<BoardRow>,
    pub marks: Vec<Marks>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
    pub can_load_more: bool,
    pub attention_error: Option<String>,
    pub filters: Filters,
    /// Rows dropped by `filters`.
    pub filtered_out: usize,
    /// All-repositories My PRs searched only your PRs (`--authored`); set by
    /// the caller from the `BoardConfig` it fetched with.
    pub authored_only: bool,
}

impl BoardView {
    pub fn all_repos(&self) -> bool {
        self.scope.is_all()
    }

    pub fn snoozed_ids(&self) -> HashSet<String> {
        self.rows
            .iter()
            .zip(&self.marks)
            .filter(|(_, marks)| marks.snoozed.is_some())
            .map(|(row, _)| row.id.clone())
            .collect()
    }

    pub fn layout(&self, show_snoozed: bool) -> Vec<LayoutItem> {
        layout(
            &self.rows,
            self.mode,
            self.all_repos(),
            &self.snoozed_ids(),
            show_snoozed,
        )
    }
}

/// One request, plus up to `pages - 1` user-requested pages.
pub fn fetch(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    viewer: &str,
    config: &BoardConfig,
    pages: u8,
) -> Result<BoardFetch, GhError> {
    let mut board = fetch_board_scoped(transport, mode, scope, viewer, config)?;
    for _ in 1..pages {
        if !board.pagination.can_load_more(mode) {
            break;
        }
        board = fetch_more_board_scoped(transport, mode, scope, viewer, config, &board)?;
    }
    Ok(board)
}

/// Annotate and filter a fetch. Change detection replays this fetch against a
/// copy of the app's snapshots, so "changed" means changed since the person
/// last acknowledged the PR in the app — and nothing is written back.
pub fn build(
    board: BoardFetch,
    attention: &AttentionState,
    mode: Mode,
    scope: BoardScope,
    viewer: String,
    filters: Filters,
    now: DateTime<Utc>,
) -> BoardView {
    let mut snapshots = attention.snapshots.clone();
    let mut rows = Vec::with_capacity(board.rows.len());
    let mut marks = Vec::with_capacity(board.rows.len());
    let total = board.rows.len();
    for row in board.rows {
        let mut row_marks = marks_for(&row, attention, &mut snapshots, now);
        row_marks.stale = is_stale(&row, now, filters.stale_after_days);
        if (filters.changed && !row_marks.changed)
            || (filters.watched && !row_marks.watched)
            || (filters.stale && !row_marks.stale)
        {
            continue;
        }
        rows.push(row);
        marks.push(row_marks);
    }
    let can_load_more = board.pagination.can_load_more(mode);
    BoardView {
        mode,
        scope,
        viewer,
        generated_at: now,
        filtered_out: total - rows.len(),
        rows,
        marks,
        rate: board.rate,
        truncated: board.truncated,
        can_load_more,
        attention_error: attention.storage_error.clone(),
        filters,
        authored_only: false,
    }
}

pub fn marks_for(
    row: &BoardRow,
    attention: &AttentionState,
    snapshots: &mut prmarmot_core::attention::SnapshotStore,
    now: DateTime<Utc>,
) -> Marks {
    // A PR this store cannot hold (validation) simply shows as unchanged.
    let _ = snapshots.observe(row.id.clone(), Observation::from_row(row));
    let snapshot = snapshots.snapshot(&row.id);
    Marks {
        watched: attention.is_watched(&row.id),
        // A snooze whose condition is already met would be woken by the app on
        // its next refresh; don't hide the PR behind it.
        snoozed: attention
            .snooze(&row.id)
            .filter(|snooze| !snooze.should_wake(Some(row), now))
            .map(|snooze| snooze.description()),
        changed: snapshot.is_some_and(|s| s.changed_since_acknowledgement),
        changes: snapshot.map(|s| s.change_summary()).unwrap_or_default(),
        stale: false,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use prmarmot_core::board::{
        BoardPagination, Category, Ci, QueueProvenance, ReviewState, ReviewSummary,
    };
    use prmarmot_local::attention_state::SnoozeCondition;

    pub fn row(number: u64, category: Category) -> BoardRow {
        BoardRow {
            id: format!("PR_{number}"),
            repo: "acme/widgets".into(),
            updated_at: Some("2026-09-11T10:00:00Z".into()),
            head_oid: Some("head-1".into()),
            reviewed_oid: None,
            reviewed_at: None,
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("Change number {number}"),
            issue: None,
            issue_url: None,
            author: Some("alice".into()),
            stack: None,
            queue_provenance: Some(QueueProvenance::Requested),
            draft: category == Category::Draft,
            category,
            bug: false,
            labels: Vec::new(),
            ci: Ci::Pass,
            conflict: false,
            mergeable_unknown: false,
            review_decision: None,
            review_state: ReviewState::Waiting,
            requested: vec!["bob".into()],
            requested_teams: Vec::new(),
            reviews: vec![ReviewSummary {
                login: Some("carol".into()),
                state: "COMMENTED".into(),
                submitted_at: Some("2026-09-10T10:00:00Z".into()),
            }],
            my_review: None,
            unresolved: 0,
            blockers: Vec::new(),
            created_at: "2026-09-01T10:00:00Z".into(),
            waiting_since: None,
            note: "🟡 waiting on bob".into(),
        }
    }

    pub fn fetch_of(rows: Vec<BoardRow>) -> BoardFetch {
        BoardFetch {
            rows,
            rate: None,
            truncated: false,
            pagination: BoardPagination::default(),
            tracked: Vec::new(),
        }
    }

    fn namespace() -> SnapshotNamespace {
        SnapshotNamespace::new("github.com", "me")
    }

    #[test]
    fn marks_reflect_the_apps_watches_snoozes_and_acknowledged_snapshots() {
        let mut attention = AttentionState::empty(namespace());
        let watched = row(1, Category::Await);
        let snoozed = row(2, Category::Await);
        let changed = row(3, Category::Action);
        attention.toggle_watch(&watched);
        attention.set_snooze(AttentionState::snooze_for(
            &snoozed,
            SnoozeCondition::Until {
                deadline: Utc::now() + chrono::Duration::hours(3),
            },
        ));
        let mut before = Observation::from_row(&changed);
        before.semantic.ci = prmarmot_core::attention::ObservedCi::Running;
        attention.snapshots.observe(&changed.id, before).unwrap();

        let view = build(
            fetch_of(vec![watched, snoozed, changed]),
            &attention,
            Mode::Authored,
            BoardScope::Repository("acme/widgets".into()),
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        assert!(view.marks[0].watched && !view.marks[0].changed);
        assert!(view.marks[1]
            .snoozed
            .as_deref()
            .is_some_and(|d| d.starts_with("Snoozed until")));
        assert!(view.marks[2].changed);
        assert_eq!(view.marks[2].changes, vec!["CI running → passing"]);
        // Read-only: the app's own snapshot still holds the old observation.
        assert!(!attention.is_changed("PR_3"));
        assert_eq!(view.snoozed_ids(), HashSet::from(["PR_2".to_string()]));
    }

    #[test]
    fn a_snooze_whose_condition_is_met_no_longer_hides_the_pr() {
        let mut attention = AttentionState::empty(namespace());
        let mut pr = row(4, Category::Await);
        pr.ci = Ci::Running;
        attention.set_snooze(AttentionState::snooze_for(
            &pr,
            AttentionState::waiting_ci(&pr),
        ));
        pr.ci = Ci::Pass;
        let view = build(
            fetch_of(vec![pr]),
            &attention,
            Mode::Authored,
            BoardScope::AllRepositories,
            "me".into(),
            Filters::default(),
            Utc::now(),
        );
        assert_eq!(view.marks[0].snoozed, None);
    }

    #[test]
    fn filters_keep_only_changed_or_watched_rows_and_count_the_rest() {
        let mut attention = AttentionState::empty(namespace());
        let watched = row(1, Category::Await);
        attention.toggle_watch(&watched);
        let view = build(
            fetch_of(vec![
                watched,
                row(2, Category::Await),
                row(3, Category::Draft),
            ]),
            &attention,
            Mode::Authored,
            BoardScope::AllRepositories,
            "me".into(),
            Filters {
                watched: true,
                ..Filters::default()
            },
            Utc::now(),
        );
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.filtered_out, 2);

        let view = build(
            fetch_of(vec![row(5, Category::Await)]),
            &attention,
            Mode::Authored,
            BoardScope::AllRepositories,
            "me".into(),
            Filters {
                changed: true,
                ..Filters::default()
            },
            Utc::now(),
        );
        assert!(view.rows.is_empty());
    }

    #[test]
    fn stale_marks_follow_the_configured_days_and_filter() {
        let now = DateTime::parse_from_rfc3339("2026-09-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let waited = |number, since: Option<&str>| {
            let mut pr = row(number, Category::Todo);
            pr.waiting_since = since.map(str::to_owned);
            pr
        };
        let rows = || {
            vec![
                waited(1, Some("2026-09-10T10:00:00Z")),
                waited(2, Some("2026-09-13T10:00:00Z")),
                waited(3, None),
            ]
        };
        let attention = AttentionState::empty(namespace());
        let view = |filters| {
            build(
                fetch_of(rows()),
                &attention,
                Mode::Review,
                BoardScope::AllRepositories,
                "me".into(),
                filters,
                now,
            )
        };
        let all = view(Filters::default());
        let stale: Vec<bool> = all.marks.iter().map(|marks| marks.stale).collect();
        assert_eq!(stale, [true, false, false]);

        let only = view(Filters {
            stale: true,
            stale_after_days: 2,
            ..Filters::default()
        });
        let numbers: Vec<u64> = only.rows.iter().map(|row| row.number).collect();
        assert_eq!(numbers, [1, 2]);
        assert_eq!(only.filtered_out, 1);
    }
}
