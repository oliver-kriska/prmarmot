//! The data behind a view: config and scope resolution (same precedence as the
//! app), one bounded GitHub fetch, and the read-only attention overlay.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use prmarmot_core::attention::{Observation, SnapshotNamespace};
use prmarmot_core::board::{
    fetch_all_open, fetch_board_scoped, fetch_more_board_scoped, BoardConfig, BoardFetch, BoardRow,
    BoardScope, Mode,
};
use prmarmot_core::github::rate_limit::RateLimitInfo;
use prmarmot_core::github::{GhError, GithubTransport};
use prmarmot_core::layout::{layout_ordered, LayoutItem, SectionOrder, Sort};
use prmarmot_core::pickup::{is_stale, DEFAULT_STALE_AFTER_DAYS};
use prmarmot_core::search::{
    local_only_terms, matches_filter, take_filter_chips, FilterChip, Qualifier, RemoteFilter,
    StaleRule,
};
use prmarmot_local::attention_state::AttentionState;
use prmarmot_local::config::{self, AuthMode, AuthSettings, FileConfig};
use prmarmot_local::session::{self, Session};

/// Everything resolved before talking to GitHub.
pub struct Setup {
    pub file: FileConfig,
    pub scope: BoardScope,
    pub board: BoardConfig,
    pub auth: AuthSettings,
    /// `section_order` from the file: the app's order, so both show the same.
    pub sections: SectionOrder,
    pub warnings: Vec<String>,
}

pub fn setup(
    cli_scope: Option<BoardScope>,
    cli_host: Option<&str>,
    cli_auth: Option<AuthMode>,
) -> Setup {
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
    let auth = config::auth_settings(&file, cli_host, cli_auth, &mut warnings);
    let (sections, ignored) = SectionOrder::from_keys(&file.section_order);
    warnings.extend(ignored);
    Setup {
        file,
        scope,
        board,
        auth,
        sections,
        warnings,
    }
}

/// Open the GitHub connection this setup asks for: the `gh` CLI, or direct
/// HTTPS with a token this machine stored.
pub fn connect(setup: &Setup) -> Result<Session, GhError> {
    session::connect(
        &setup.auth,
        &session::user_agent("prmarmot-cli", env!("CARGO_PKG_VERSION")),
    )
}

/// The app's attention file for this account, loaded for reading only.
pub fn attention(host: &str, login: &str) -> AttentionState {
    AttentionState::load(SnapshotNamespace::new(host, login))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    pub changed: bool,
    pub watched: bool,
    /// Only PRs that have waited `stale_after_days` or longer for a reviewer.
    pub stale: bool,
    /// What stale means, also for each PR's `stale` mark.
    pub stale_after_days: u64,
    /// `--filter`: the app's search grammar, from `prmarmot_core::search`.
    /// Free words plus `label:`, `author:`, `repo:` and `is:stale`, ANDed,
    /// except that one of several `author:` or `repo:` terms is enough.
    pub query: Option<String>,
}

impl Filters {
    /// What of `--filter` GitHub answers itself, for All open: its `label:`
    /// and `author:` terms, which then cover the whole repository. The other
    /// views load everything they show, so nothing goes to GitHub for them.
    pub fn remote(&self, mode: Mode) -> RemoteFilter {
        match (mode, self.query.as_deref()) {
            (Mode::AllOpen, Some(query)) => {
                RemoteFilter::from_chips(&take_filter_chips(query, true).0)
            }
            _ => RemoteFilter::default(),
        }
    }
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            changed: false,
            watched: false,
            stale: false,
            stale_after_days: DEFAULT_STALE_AFTER_DAYS,
            query: None,
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
    /// The order inside the review queue's pickup sections (`--sort`); set by
    /// the caller.
    pub sort: Sort,
    /// The order of the sections themselves (`section_order` in config.toml);
    /// set by the caller.
    pub sections: SectionOrder,
    /// How many PRs GitHub's search matched, loaded or not (My PRs and All
    /// open; `None` for the review queue and when GitHub did not say).
    pub total: Option<u64>,
    /// What of `--filter` went to GitHub with the search (All open only).
    pub remote: RemoteFilter,
}

impl BoardView {
    pub fn all_repos(&self) -> bool {
        self.scope.is_all()
    }

    /// Rows GitHub returned, before the local filters.
    pub fn loaded(&self) -> usize {
        self.rows.len() + self.filtered_out
    }

    /// The filter terms checked only against the loaded rows, spelled as the
    /// reader wrote them: all but what went to GitHub, plus `is:stale` for
    /// `--stale`. In All open these can miss a match that is not loaded yet.
    pub fn local_only_terms(&self) -> Vec<String> {
        let mut terms = match self.filters.query.as_deref() {
            Some(query) => {
                let (chips, rest) = take_filter_chips(query, true);
                local_only_terms(&rest, &chips, &self.remote)
            }
            None => Vec::new(),
        };
        let stale = FilterChip::new(Qualifier::Is, "stale").term();
        if self.filters.stale && !terms.contains(&stale) {
            terms.push(stale);
        }
        terms
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
        layout_ordered(
            &self.rows,
            self.mode,
            self.all_repos(),
            &self.snoozed_ids(),
            show_snoozed,
            self.sort,
            &self.sections,
        )
    }
}

/// Why All open refuses to run across all repositories, and how to pick one.
pub fn all_open_needs_repository() -> String {
    format!(
        "{} Pass --repo OWNER/NAME, or set `repo` in ~/.config/prmarmot/config.toml.",
        prmarmot_core::status::all_open_needs_repository()
    )
}

/// One request, plus up to `pages - 1` user-requested pages. All open sends
/// `filter` with its search, so GitHub answers those terms for the whole
/// repository and Load more pages the same search.
pub fn fetch(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    viewer: &str,
    config: &BoardConfig,
    filter: &RemoteFilter,
    pages: u8,
) -> Result<BoardFetch, GhError> {
    let mut board = match (mode, scope.repository()) {
        (Mode::AllOpen, Some(repo)) => {
            fetch_all_open(transport, repo, viewer, config, filter, &[])?
        }
        _ => fetch_board_scoped(transport, mode, scope, viewer, config)?,
    };
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
        let query_matches = filters.query.as_deref().is_none_or(|query| {
            matches_filter(
                &row,
                query,
                StaleRule {
                    now,
                    after_days: filters.stale_after_days,
                },
            )
        });
        if (filters.changed && !row_marks.changed)
            || (filters.watched && !row_marks.watched)
            || (filters.stale && !row_marks.stale)
            || !query_matches
        {
            continue;
        }
        rows.push(row);
        marks.push(row_marks);
    }
    let can_load_more = board.pagination.can_load_more(mode);
    let remote = filters.remote(mode);
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
        sort: Sort::Wait,
        sections: SectionOrder::default(),
        total: board.total,
        remote,
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
            .map(|snooze| snooze.description_utc()),
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
            size: None,
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
            access: Default::default(),
            total: None,
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
    fn the_filter_query_uses_the_apps_search_grammar() {
        let attention = AttentionState::empty(namespace());
        let mut labelled = row(1, Category::Await);
        labelled.labels = vec!["help wanted".into()];
        let mut other_author = row(2, Category::Await);
        other_author.author = Some("bob".into());
        let plain = row(3, Category::Await);

        let filtered = |query: &str, rows: Vec<BoardRow>| {
            build(
                fetch_of(rows),
                &attention,
                Mode::Authored,
                BoardScope::AllRepositories,
                "me".into(),
                Filters {
                    query: Some(query.into()),
                    ..Filters::default()
                },
                Utc::now(),
            )
        };

        let rows = vec![labelled.clone(), other_author.clone(), plain.clone()];
        // A quoted qualifier value, the same one the app's search box takes.
        let view = filtered("label:\"help wanted\"", rows.clone());
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].number, 1);
        assert_eq!(view.filtered_out, 2);

        // Free words match the title, and every term must match.
        assert_eq!(filtered("number 2", rows.clone()).rows.len(), 1);
        assert_eq!(filtered("author:bob", rows.clone()).rows.len(), 1);
        assert_eq!(filtered("author:bob number 1", rows.clone()).rows.len(), 0);
        assert_eq!(filtered("repo:acme/widgets", rows.clone()).rows.len(), 3);
        assert_eq!(filtered("   ", rows).rows.len(), 3);
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
