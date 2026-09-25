//! The board view-model: `RawPr` → `BoardRow` (category, CI, review state,
//! unresolved count, Note). Ported from the shell prototype's `jq` programs
//! (categorization) and `SKILL.md` (Note composition); the golden tests in
//! `tests/parity.rs` pin this module to the prototype's actual output.

// Exactly one regex engine, chosen by feature. See `small-regex` in
// `core/Cargo.toml` for why iOS gets the other one.
#[cfg(not(feature = "small-regex"))]
use regex::Regex;
#[cfg(feature = "small-regex")]
use regex_lite::Regex;

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::github::access::{tolerate_access_errors, AccessGaps};
use crate::github::query::{
    all_open_search_string, available_search_string, global_authored_search_string,
    global_available_search_string, global_search_string, issue_count, page_info,
    parse_alias_response, parse_pull_request_id, parse_review_response, parse_search_response,
    parse_tracked_response, pull_request_id_query, requested_ids, scope_repository_error,
    search_string, with_requested_ids, with_scope_repository, with_tracked_nodes, RawPr,
    ReviewNode, PR_SEARCH_PAGE_QUERY, PR_SEARCH_QUERY, REVIEW_AVAILABLE_PAGE_QUERY,
    REVIEW_BOTH_PAGE_QUERY, REVIEW_REQUESTED_PAGE_QUERY, REVIEW_SEARCH_QUERY, TRACKED_ONLY_QUERY,
};
use crate::github::rate_limit::RateLimitInfo;
use crate::github::{GhError, GithubTransport};
use crate::pickup::pickup_since;
use crate::search::RemoteFilter;
use crate::size::ChangeSize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// PRs the user authored — the outgoing queue.
    Authored,
    /// PRs awaiting the user's review — the incoming queue.
    Review,
    /// Every open PR in one repository, whoever wrote it: the way to find a PR
    /// that does not involve you yet. One repository only; see
    /// [`fetch_all_open`].
    AllOpen,
}

/// Repository coverage is independent from the queue mode.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BoardScope {
    AllRepositories,
    Repository(String),
}

impl BoardScope {
    pub fn repository(&self) -> Option<&str> {
        match self {
            Self::AllRepositories => None,
            Self::Repository(repo) => Some(repo),
        }
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Self::AllRepositories)
    }

    fn fallback_repo(&self) -> &str {
        self.repository().unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    // Authored mode
    Action,
    Await,
    // Review mode
    Todo,
    /// No reviewer is currently requested; available to pick up.
    Available,
    Done,
    // Both
    Draft,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Action => "action",
            Category::Await => "await",
            Category::Todo => "todo",
            Category::Available => "available",
            Category::Done => "done",
            Category::Draft => "draft",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Category::Action | Category::Todo => 0,
            Category::Await | Category::Available => 1,
            Category::Done => 2,
            Category::Draft => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    Pass,
    Fail,
    None,
    Running,
    /// The token may not read the checks (a fine-grained personal access
    /// token never can), so nothing is known: not the same as no checks.
    Hidden,
}

impl Ci {
    pub fn as_str(&self) -> &'static str {
        match self {
            Ci::Pass => "pass",
            Ci::Fail => "fail",
            Ci::None => "none",
            Ci::Running => "running",
            Ci::Hidden => "hidden",
        }
    }
}

/// Aggregate of completed human reviews on an authored PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Changes,
    Approved,
    Commented,
    Waiting,
    None,
}

impl ReviewState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewState::Changes => "changes",
            ReviewState::Approved => "approved",
            ReviewState::Commented => "commented",
            ReviewState::Waiting => "waiting",
            ReviewState::None => "none",
        }
    }
}

/// Latest review per author. `login` is `None` for reviews whose author no
/// longer exists (the prototype keeps them too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSummary {
    pub login: Option<String>,
    pub state: String,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackInfo {
    pub number: u64,
    pub size: u64,
    pub base_ref_name: String,
    pub position: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueProvenance {
    Requested,
    Available,
}

/// A single thing keeping an authored PR out of the merge queue, in the
/// prototype's canonical (most-blocking-first) order. This is the structured
/// twin of the human `note`: core owns the *facts*, the UI (`src/table.rs`)
/// owns *severity, wording, and color*. Never add tone or colors here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// No human has been asked to review. `suggested` is the configured
    /// default-reviewer list (may be empty when none is configured).
    NoReviewers {
        suggested: Vec<String>,
    },
    MergeConflict,
    CiFailing,
    ChangesRequested,
    UnresolvedComments(usize),
}

/// Why a configured issue-link pattern could not be compiled. Which engine
/// produced it depends on the `small-regex` feature; both print the offending
/// pattern and what is wrong with it.
#[cfg(not(feature = "small-regex"))]
pub type PatternError = regex::Error;
#[cfg(feature = "small-regex")]
pub type PatternError = regex_lite::Error;

/// Optional "linked ticket" extraction: a pattern matched against the PR
/// title and a URL template with an `{id}` placeholder. Project prefixes and
/// tracker URLs always come from user config.
#[derive(Debug, Clone)]
pub struct IssueLinkRule {
    pattern: Regex,
    url_template: String,
    /// Strips a leading `[<match>] ` from the displayed title.
    strip_prefix: Regex,
}

impl PartialEq for IssueLinkRule {
    fn eq(&self, other: &Self) -> bool {
        self.pattern.as_str() == other.pattern.as_str() && self.url_template == other.url_template
    }
}

impl Eq for IssueLinkRule {}

impl IssueLinkRule {
    pub fn new(pattern: &str, url_template: &str) -> Result<Self, PatternError> {
        Ok(Self {
            pattern: Regex::new(pattern)?,
            url_template: url_template.to_string(),
            strip_prefix: Regex::new(&format!(r"^\[(?:{pattern})\]\s*"))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardConfig {
    /// Review authors that never count as human review (prototype default).
    pub bots: Vec<String>,
    /// Suggested reviewers for the "no reviewers — assign …" note wherever no
    /// `repo_reviewers` entry applies.
    pub default_reviewers: Vec<String>,
    /// Suggestions for one owner (`acme`) or one repository (`acme/api`),
    /// keyed in lowercase. See [`BoardConfig::suggested_reviewers`].
    pub repo_reviewers: BTreeMap<String, Vec<String>>,
    pub issue_link: Option<IssueLinkRule>,
    /// Across all repositories, My PRs searches only PRs you authored instead
    /// of every PR involving you. The app keeps it off; `prmarmot-cli
    /// --authored` turns it on. A single repository is always authored-only.
    pub authored_only: bool,
    /// A PR that has waited this many days for a reviewer is stale.
    pub stale_after_days: u64,
}

impl BoardConfig {
    /// Who the "no reviewers" note suggests for a PR in `repo` (`owner/name`):
    /// the repository's entry, else its owner's, else `default_reviewers`. An
    /// empty entry deliberately suggests nobody there.
    pub fn suggested_reviewers(&self, repo: &str) -> &[String] {
        let repo = repo.to_ascii_lowercase();
        let owner = repo.split('/').next().unwrap_or_default();
        self.repo_reviewers
            .get(&repo)
            .or_else(|| self.repo_reviewers.get(owner))
            .unwrap_or(&self.default_reviewers)
    }
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            bots: vec!["chatgpt-codex-connector".into(), "github-actions".into()],
            default_reviewers: Vec::new(),
            repo_reviewers: BTreeMap::new(),
            issue_link: None,
            authored_only: false,
            stale_after_days: crate::pickup::DEFAULT_STALE_AFTER_DAYS,
        }
    }
}

/// One row of the dashboard. `review_state` is authored-mode only (`None` in
/// the review queue); everything else is set in both modes.
#[derive(Debug, Clone)]
pub struct BoardRow {
    /// GitHub node id (canonical URL for legacy prototype fixtures).
    pub id: String,
    pub repo: String,
    pub updated_at: Option<String>,
    pub head_oid: Option<String>,
    pub reviewed_oid: Option<String>,
    pub reviewed_at: Option<String>,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub issue: Option<String>,
    pub issue_url: Option<String>,
    pub author: Option<String>,
    pub stack: Option<StackInfo>,
    pub queue_provenance: Option<QueueProvenance>,
    pub draft: bool,
    pub category: Category,
    pub bug: bool,
    /// All label names, GitHub order. `bug` stays a derived flag ("bug" ∈ labels).
    pub labels: Vec<String>,
    pub ci: Ci,
    pub conflict: bool,
    /// GitHub had not finished computing mergeability (`UNKNOWN` or absent),
    /// so `conflict == false` says nothing. It recomputes lazily, e.g. after
    /// the base branch moves; see [`carry_forward_conflicts`].
    pub mergeable_unknown: bool,
    pub review_decision: Option<String>,
    pub review_state: ReviewState,
    /// Requested reviewers: logins and team slugs.
    pub requested: Vec<String>,
    /// The team slugs among `requested`.
    pub requested_teams: Vec<String>,
    pub reviews: Vec<ReviewSummary>,
    /// The viewer's standing review state (`APPROVED`, `COMMENTED`, …), or
    /// `NONE`; see [`standing_review`].
    pub my_review: Option<String>,
    pub unresolved: usize,
    /// Structured blockers behind the action `note`, most-blocking-first
    /// (authored mode only; empty for await/review rows). The UI reorders and
    /// colors these; the `note` string is generated from exactly this list.
    pub blockers: Vec<Blocker>,
    pub created_at: String,
    /// When the PR started waiting for a reviewer, or `None` when it is not
    /// waiting; see [`crate::pickup`].
    pub waiting_since: Option<String>,
    /// Change counts, `None` when GitHub did not report them; see
    /// [`crate::size`].
    pub size: Option<ChangeSize>,
    pub note: String,
}

/// Defensive bound on the board size. The query already caps at `first:60`;
/// this keeps the bound explicit at the data boundary (bounded-everything
/// guardrail from the PRFlow post-mortem).
pub const MAX_BOARD_ROWS: usize = 60;
pub const MAX_EXPANDED_BOARD_ROWS: usize = 120;
pub const MAX_PAGES_PER_ALIAS: u8 = 5;

#[derive(Debug, Clone, Default)]
struct AliasCursor {
    end_cursor: Option<String>,
    has_next: bool,
    pages: u8,
    blocked: bool,
}

impl AliasCursor {
    fn from_page(page: crate::github::query::PageInfo) -> Self {
        let blocked = page.has_next_page && page.end_cursor.is_none();
        Self {
            end_cursor: page.end_cursor,
            has_next: page.has_next_page,
            pages: 1,
            blocked,
        }
    }

    fn can_load(&self) -> bool {
        self.has_next
            && !self.blocked
            && self.end_cursor.is_some()
            && self.pages < MAX_PAGES_PER_ALIAS
    }

    fn update(&mut self, page: crate::github::query::PageInfo) {
        let advances = page.end_cursor.is_some() && page.end_cursor != self.end_cursor;
        self.pages += 1;
        self.end_cursor = page.end_cursor;
        // A malformed/non-advancing cursor is terminal, even if GitHub says more.
        self.has_next = page.has_next_page;
        self.blocked = page.has_next_page && !advances;
    }
}

#[derive(Debug, Clone, Default)]
pub struct BoardPagination {
    /// The single search of My PRs and All open.
    authored: AliasCursor,
    requested: AliasCursor,
    available: AliasCursor,
    /// All open's search as page one ran it, filters included. Load more
    /// repeats it exactly: a cursor only pages the search that produced it.
    search: Option<String>,
    /// All open: the PRs page one found requesting your review, so a Load
    /// more page reads them the same way. At most
    /// [`crate::github::query::MAX_ALL_OPEN_REQUESTED`].
    requested_ids: Vec<String>,
}

impl BoardPagination {
    pub fn can_load_more(&self, mode: Mode) -> bool {
        match mode {
            Mode::Authored => self.authored.can_load(),
            Mode::Review => self.requested.can_load() || self.available.can_load(),
            Mode::AllOpen => self.search.is_some() && self.authored.can_load(),
        }
    }

    pub fn page_limit_reached(&self, mode: Mode) -> bool {
        let limited = |c: &AliasCursor| c.has_next && c.pages >= MAX_PAGES_PER_ALIAS;
        match mode {
            Mode::Authored | Mode::AllOpen => limited(&self.authored),
            Mode::Review => limited(&self.requested) || limited(&self.available),
        }
    }

    fn truncated(&self, mode: Mode) -> bool {
        match mode {
            Mode::Authored | Mode::AllOpen => self.authored.has_next,
            Mode::Review => self.requested.has_next || self.available.has_next,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BoardFetch {
    pub rows: Vec<BoardRow>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
    pub pagination: BoardPagination,
    pub tracked: Vec<TrackedPr>,
    /// What the token was refused while these rows were fetched, for
    /// [`crate::status::access_notice`].
    pub access: AccessGaps,
    /// How many PRs the search matched in all, loaded or not, for the
    /// single-search views (My PRs, All open). `None` for the review queue,
    /// whose two searches overlap, and when GitHub did not say.
    pub total: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TrackedPr {
    pub pr_id: String,
    pub status: TrackedPrStatus,
    pub row: Option<BoardRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedPrStatus {
    Open,
    Closed,
    Merged,
    Inaccessible,
}

/// Tracked PRs on their own, without a board search.
#[derive(Debug, Clone)]
pub struct TrackedFetch {
    pub tracked: Vec<TrackedPr>,
    pub rate: Option<RateLimitInfo>,
    pub access: AccessGaps,
}

/// One bounded `nodes(ids:)` request for specific PRs, in any repository: what
/// became of PRs that left a view, or one PR followed on its own.
pub fn fetch_tracked(
    transport: &dyn GithubTransport,
    ids: &[String],
    me: &str,
    cfg: &BoardConfig,
) -> Result<TrackedFetch, GhError> {
    if ids.is_empty() {
        return Ok(TrackedFetch {
            tracked: Vec::new(),
            rate: None,
            access: AccessGaps::default(),
        });
    }
    let query = with_tracked_nodes(TRACKED_ONLY_QUERY)?;
    let mut body = transport.graphql_with_ids(&query, &[("who", me)], ids)?;
    let access = tolerate_access_errors(&mut body);
    let rate = parse_tracked_response(&body)?;
    Ok(TrackedFetch {
        tracked: derive_tracked(&body, ids, "", me, cfg),
        rate,
        access,
    })
}

/// The node id of `owner/name#number`, for [`fetch_tracked`].
pub fn resolve_pull_request_id(
    transport: &dyn GithubTransport,
    repo: &str,
    number: u64,
) -> Result<String, GhError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(GhError::RepositoryNotFound(repo.to_owned()));
    };
    let body = transport.graphql(
        &pull_request_id_query(number),
        &[("owner", owner), ("name", name)],
    )?;
    parse_pull_request_id(&body, repo, number)
}

/// Fetch a complete board flow. Authored mode preserves the legacy single
/// search; review mode combines requested and unrequested candidates in one
/// GraphQL request, with requested rows first and deduplicated.
pub fn fetch_board(
    transport: &dyn GithubTransport,
    mode: Mode,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) -> Result<BoardFetch, GhError> {
    fetch_board_scoped(
        transport,
        mode,
        &BoardScope::Repository(repo.to_owned()),
        me,
        cfg,
    )
}

/// Fetch a queue for either one repository or every repository involving the
/// authenticated user. Both variants retain the same one-operation contract.
pub fn fetch_board_scoped(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
) -> Result<BoardFetch, GhError> {
    fetch_board_scoped_with_tracked(transport, mode, scope, me, cfg, &[])
}

pub fn fetch_board_scoped_with_tracked(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    tracked_ids: &[String],
) -> Result<BoardFetch, GhError> {
    fetch_scoped(
        transport,
        mode,
        scope,
        me,
        cfg,
        tracked_ids,
        &RemoteFilter::default(),
    )
}

/// All open for one repository, with `filter`'s labels and authors matched by
/// GitHub rather than among the loaded rows, so the count, the first page, and
/// Load more all cover the whole repository. Same one-operation contract and
/// bounds as the other views: 60 rows a page, user-invoked Load more, at most
/// [`MAX_PAGES_PER_ALIAS`] pages.
pub fn fetch_all_open(
    transport: &dyn GithubTransport,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    filter: &RemoteFilter,
    tracked_ids: &[String],
) -> Result<BoardFetch, GhError> {
    fetch_scoped(
        transport,
        Mode::AllOpen,
        &BoardScope::Repository(repo.to_owned()),
        me,
        cfg,
        tracked_ids,
        filter,
    )
}

fn fetch_scoped(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    tracked_ids: &[String],
    filter: &RemoteFilter,
) -> Result<BoardFetch, GhError> {
    if mode == Mode::AllOpen && scope.is_all() {
        return Err(GhError::NeedsRepository);
    }
    let repo = scope.fallback_repo();
    let scope_repository = scope.repository().and_then(|repo| repo.split_once('/'));
    let initial_operation = |base: &str| -> Result<String, GhError> {
        let mut query = base.to_owned();
        if scope_repository.is_some() {
            query = with_scope_repository(&query)?;
        }
        if !tracked_ids.is_empty() {
            query = with_tracked_nodes(&query)?;
        }
        Ok(query)
    };
    let with_scope_variables = |mut variables: Vec<(&'static str, String)>| {
        if let Some((owner, name)) = scope_repository {
            variables.push(("scopeOwner", owner.to_owned()));
            variables.push(("scopeName", name.to_owned()));
        }
        variables
    };
    let request = |query: &str, variables: &[(&'static str, String)]| {
        let variables: Vec<(&str, &str)> = variables
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let mut body = transport.graphql_with_ids(query, &variables, tracked_ids)?;
        if let Some(error) = scope_repository_error(&body, repo) {
            return Err(error);
        }
        let access = tolerate_access_errors(&mut body);
        Ok((body, access))
    };
    match mode {
        Mode::Authored => {
            let search = scope_search_string(scope, mode, me, cfg);
            let (body, access) = request(
                &initial_operation(PR_SEARCH_QUERY)?,
                &with_scope_variables(vec![("q", search), ("who", me.to_owned())]),
            )?;
            let truncated = body
                .pointer("/data/search/pageInfo/hasNextPage")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let (prs, rate) = parse_search_response(&body)?;
            let pagination = BoardPagination {
                authored: AliasCursor::from_page(page_info(&body, "search")),
                ..Default::default()
            };
            Ok(BoardFetch {
                rows: if scope.is_all() {
                    derive_involving_rows(&prs, repo, me, cfg)
                } else {
                    derive_rows(&prs, mode, repo, me, cfg)
                },
                rate,
                truncated,
                pagination,
                tracked: derive_tracked(&body, tracked_ids, repo, me, cfg),
                access,
                total: issue_count(&body, "search"),
            })
        }
        Mode::AllOpen => {
            let search = all_open_search_string(repo, &filter.qualifiers());
            let (body, access) = request(
                &initial_operation(&with_requested_ids(PR_SEARCH_QUERY)?)?,
                &with_scope_variables(vec![
                    ("q", search.clone()),
                    // The review queue's own search, unfiltered: a review
                    // asked of you stays one whatever the chips say.
                    ("requested", search_string(Mode::Review, repo, me)),
                    ("who", me.to_owned()),
                ]),
            )?;
            let (prs, rate) = parse_search_response(&body)?;
            let page = page_info(&body, "search");
            let requested = requested_ids(&body);
            Ok(BoardFetch {
                rows: derive_all_open_rows(&prs, repo, me, cfg, &requested),
                rate,
                truncated: page.has_next_page,
                pagination: BoardPagination {
                    authored: AliasCursor::from_page(page),
                    search: Some(search),
                    requested_ids: requested,
                    ..Default::default()
                },
                tracked: derive_tracked(&body, tracked_ids, repo, me, cfg),
                access,
                total: issue_count(&body, "search"),
            })
        }
        Mode::Review => {
            let requested_search = scope_search_string(scope, mode, me, cfg);
            let available_search = scope_available_search_string(scope, me);
            let (body, access) = request(
                &initial_operation(REVIEW_SEARCH_QUERY)?,
                &with_scope_variables(vec![
                    ("requested", requested_search),
                    ("available", available_search),
                    ("who", me.to_owned()),
                ]),
            )?;
            let parsed = parse_review_response(&body)?;
            let mut seen = HashSet::new();
            let mut rows = Vec::new();

            for pr in parsed.requested.iter().filter(|pr| !is_own_pr(pr, me)) {
                if seen.insert(pr_identity(pr, repo)) {
                    rows.push(derive_review_row(
                        pr,
                        repo,
                        me,
                        cfg,
                        QueueProvenance::Requested,
                    ));
                }
            }
            for pr in parsed
                .available
                .iter()
                .filter(|pr| !is_own_pr(pr, me) && pr.review_requests.total_count == 0)
            {
                if seen.insert(pr_identity(pr, repo)) {
                    rows.push(derive_review_row(
                        pr,
                        repo,
                        me,
                        cfg,
                        QueueProvenance::Available,
                    ));
                }
            }
            let overflow = rows.len() > MAX_EXPANDED_BOARD_ROWS;
            rows.truncate(MAX_EXPANDED_BOARD_ROWS);
            rows.sort_by_key(|r| (r.category.rank(), r.number));
            Ok(BoardFetch {
                rows,
                rate: parsed.rate,
                truncated: parsed.truncated || overflow,
                pagination: BoardPagination {
                    requested: AliasCursor::from_page(parsed.requested_page),
                    available: AliasCursor::from_page(parsed.available_page),
                    ..Default::default()
                },
                tracked: derive_tracked(&body, tracked_ids, repo, me, cfg),
                access,
                total: None,
            })
        }
    }
}

fn derive_tracked(
    body: &serde_json::Value,
    requested_ids: &[String],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) -> Vec<TrackedPr> {
    let nodes = body
        .pointer("/data/tracked")
        .and_then(serde_json::Value::as_array);
    requested_ids
        .iter()
        .enumerate()
        .map(|(index, requested_id)| {
            let raw = nodes
                .and_then(|nodes| nodes.get(index))
                .filter(|value| !value.is_null())
                .and_then(|value| serde_json::from_value::<RawPr>(value.clone()).ok());
            match raw {
                None => TrackedPr {
                    pr_id: requested_id.clone(),
                    status: TrackedPrStatus::Inaccessible,
                    row: None,
                },
                Some(raw) => {
                    let status = if raw.merged {
                        TrackedPrStatus::Merged
                    } else if raw.state.as_deref() == Some("CLOSED") {
                        TrackedPrStatus::Closed
                    } else {
                        TrackedPrStatus::Open
                    };
                    TrackedPr {
                        pr_id: pr_identity(&raw, repo),
                        status,
                        // Someone else's PR reads as it does in Involving me.
                        row: Some(derive_involving_row(&raw, repo, me, cfg)),
                    }
                }
            }
        })
        .collect()
}

/// Fetch one user-requested page for each alias that still has a usable cursor.
/// Exhausted aliases are omitted from the GraphQL operation. The returned value
/// is independent, so callers can retain all prior rows/cursors on any error.
pub fn fetch_more_board(
    transport: &dyn GithubTransport,
    mode: Mode,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    current: &BoardFetch,
) -> Result<BoardFetch, GhError> {
    fetch_more_board_scoped(
        transport,
        mode,
        &BoardScope::Repository(repo.to_owned()),
        me,
        cfg,
        current,
    )
}

pub fn fetch_more_board_scoped(
    transport: &dyn GithubTransport,
    mode: Mode,
    scope: &BoardScope,
    me: &str,
    cfg: &BoardConfig,
    current: &BoardFetch,
) -> Result<BoardFetch, GhError> {
    let repo = scope.fallback_repo();
    let mut next = current.clone();
    match mode {
        Mode::Authored => {
            if !next.pagination.authored.can_load() {
                return Ok(next);
            }
            let search = scope_search_string(scope, mode, me, cfg);
            let cursor = next.pagination.authored.end_cursor.clone().unwrap();
            let mut body = transport.graphql(
                PR_SEARCH_PAGE_QUERY,
                &[("q", &search), ("after", &cursor), ("who", me)],
            )?;
            next.access = next.access.plus(tolerate_access_errors(&mut body));
            let (prs, rate) = parse_search_response(&body)?;
            next.pagination.authored.update(page_info(&body, "search"));
            next.total = issue_count(&body, "search").or(next.total);
            if scope.is_all() {
                merge_involving(&mut next.rows, &prs, repo, me, cfg);
            } else {
                merge_authored(&mut next.rows, &prs, repo, me, cfg);
            }
            next.rate = rate;
        }
        Mode::AllOpen => {
            let Some(search) = next.pagination.search.clone() else {
                return Ok(next);
            };
            if !next.pagination.authored.can_load() {
                return Ok(next);
            }
            let cursor = next.pagination.authored.end_cursor.clone().unwrap();
            let mut body = transport.graphql(
                PR_SEARCH_PAGE_QUERY,
                &[("q", &search), ("after", &cursor), ("who", me)],
            )?;
            next.access = next.access.plus(tolerate_access_errors(&mut body));
            let (prs, rate) = parse_search_response(&body)?;
            next.pagination.authored.update(page_info(&body, "search"));
            next.total = issue_count(&body, "search").or(next.total);
            let requested = std::mem::take(&mut next.pagination.requested_ids);
            merge_all_open(&mut next.rows, &prs, repo, me, cfg, &requested);
            next.pagination.requested_ids = requested;
            next.rate = rate;
        }
        Mode::Review => {
            let requested = next.pagination.requested.can_load();
            let available = next.pagination.available.can_load();
            if !requested && !available {
                return Ok(next);
            }
            let requested_search = scope_search_string(scope, mode, me, cfg);
            let available_search = scope_available_search_string(scope, me);
            let mut requested_prs = Vec::new();
            let mut available_prs = Vec::new();
            if requested && available {
                let rc = next.pagination.requested.end_cursor.clone().unwrap();
                let ac = next.pagination.available.end_cursor.clone().unwrap();
                let mut body = transport.graphql(
                    REVIEW_BOTH_PAGE_QUERY,
                    &[
                        ("requested", &requested_search),
                        ("requestedAfter", &rc),
                        ("available", &available_search),
                        ("availableAfter", &ac),
                        ("who", me),
                    ],
                )?;
                next.access = next.access.plus(tolerate_access_errors(&mut body));
                let parsed = parse_review_response(&body)?;
                requested_prs = parsed.requested;
                available_prs = parsed.available;
                next.pagination.requested.update(parsed.requested_page);
                next.pagination.available.update(parsed.available_page);
                next.rate = parsed.rate;
            } else {
                let (query, alias, search, cursor) = if requested {
                    (
                        REVIEW_REQUESTED_PAGE_QUERY,
                        "requested",
                        &requested_search,
                        next.pagination.requested.end_cursor.clone().unwrap(),
                    )
                } else {
                    (
                        REVIEW_AVAILABLE_PAGE_QUERY,
                        "available",
                        &available_search,
                        next.pagination.available.end_cursor.clone().unwrap(),
                    )
                };
                let mut body = transport
                    .graphql(query, &[(alias, search), ("after", &cursor), ("who", me)])?;
                next.access = next.access.plus(tolerate_access_errors(&mut body));
                let (prs, page, rate) = parse_alias_response(&body, alias)?;
                if requested {
                    requested_prs = prs;
                    next.pagination.requested.update(page);
                } else {
                    available_prs = prs;
                    next.pagination.available.update(page);
                }
                next.rate = rate;
            }
            merge_review(
                &mut next.rows,
                &requested_prs,
                &available_prs,
                repo,
                me,
                cfg,
            );
        }
    }
    next.truncated = next.pagination.truncated(mode);
    Ok(next)
}

fn scope_search_string(scope: &BoardScope, mode: Mode, me: &str, cfg: &BoardConfig) -> String {
    match scope {
        BoardScope::AllRepositories if mode == Mode::Authored && cfg.authored_only => {
            global_authored_search_string(me)
        }
        BoardScope::AllRepositories => global_search_string(mode, me),
        BoardScope::Repository(repo) => crate::github::query::search_string(mode, repo, me),
    }
}

fn scope_available_search_string(scope: &BoardScope, me: &str) -> String {
    match scope {
        BoardScope::AllRepositories => global_available_search_string(me),
        BoardScope::Repository(repo) => available_search_string(repo, me),
    }
}

fn merge_authored(
    rows: &mut Vec<BoardRow>,
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) {
    let mut by_id: HashMap<String, BoardRow> = rows.drain(..).map(|r| (r.id.clone(), r)).collect();
    for pr in prs {
        by_id.insert(
            pr_identity(pr, repo),
            derive_row(pr, Mode::Authored, repo, me, cfg),
        );
    }
    *rows = by_id.into_values().collect();
    rows.sort_by(|a, b| {
        (a.category.rank(), std::cmp::Reverse(a.number), &a.id).cmp(&(
            b.category.rank(),
            std::cmp::Reverse(b.number),
            &b.id,
        ))
    });
    rows.truncate(MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize);
}

fn merge_involving(
    rows: &mut Vec<BoardRow>,
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) {
    let mut by_id: HashMap<String, BoardRow> = rows.drain(..).map(|r| (r.id.clone(), r)).collect();
    for pr in prs {
        by_id.insert(
            pr_identity(pr, repo),
            derive_involving_row(pr, repo, me, cfg),
        );
    }
    *rows = by_id.into_values().collect();
    rows.sort_by(|a, b| {
        (a.category.rank(), std::cmp::Reverse(&a.updated_at), &a.id).cmp(&(
            b.category.rank(),
            std::cmp::Reverse(&b.updated_at),
            &b.id,
        ))
    });
    rows.truncate(MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize);
}

fn merge_all_open(
    rows: &mut Vec<BoardRow>,
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    requested: &[String],
) {
    let mut by_id: HashMap<String, BoardRow> = rows.drain(..).map(|r| (r.id.clone(), r)).collect();
    for pr in prs {
        by_id.insert(
            pr_identity(pr, repo),
            derive_all_open_row(pr, repo, me, cfg, requested),
        );
    }
    *rows = by_id.into_values().collect();
    sort_most_recently_updated(rows);
    rows.truncate(MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize);
}

/// All open's order: the search's own, most recently updated first, so a Load
/// more page lands below what is already on screen.
fn sort_most_recently_updated(rows: &mut [BoardRow]) {
    rows.sort_by(|a, b| {
        (
            std::cmp::Reverse(&a.updated_at),
            std::cmp::Reverse(a.number),
            &a.id,
        )
            .cmp(&(
                std::cmp::Reverse(&b.updated_at),
                std::cmp::Reverse(b.number),
                &b.id,
            ))
    });
}

fn merge_review(
    rows: &mut Vec<BoardRow>,
    requested: &[RawPr],
    available: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) {
    let mut by_id: HashMap<String, BoardRow> = rows.drain(..).map(|r| (r.id.clone(), r)).collect();
    for pr in available
        .iter()
        .filter(|pr| !is_own_pr(pr, me) && pr.review_requests.total_count == 0)
    {
        by_id
            .entry(pr_identity(pr, repo))
            .or_insert_with(|| derive_review_row(pr, repo, me, cfg, QueueProvenance::Available));
    }
    // Requested provenance wins even when the broad alias produced it earlier.
    for pr in requested.iter().filter(|pr| !is_own_pr(pr, me)) {
        by_id.insert(
            pr_identity(pr, repo),
            derive_review_row(pr, repo, me, cfg, QueueProvenance::Requested),
        );
    }
    *rows = by_id.into_values().collect();
    rows.sort_by(|a, b| {
        (a.category.rank(), a.number, &a.id).cmp(&(b.category.rank(), b.number, &b.id))
    });
    rows.truncate(MAX_BOARD_ROWS * MAX_PAGES_PER_ALIAS as usize * 2);
}

fn is_own_pr(pr: &RawPr, me: &str) -> bool {
    pr.author.as_ref().and_then(|a| a.login.as_deref()) == Some(me)
}

fn pr_identity(pr: &RawPr, repo: &str) -> String {
    pr.id.clone().unwrap_or_else(|| pr.canonical_url(repo))
}

/// Derive and sort the full board. `me` must be the resolved login.
pub fn derive_rows(
    prs: &[RawPr],
    mode: Mode,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
) -> Vec<BoardRow> {
    let mut rows: Vec<BoardRow> = prs
        .iter()
        .take(MAX_BOARD_ROWS)
        .map(|pr| match mode {
            Mode::AllOpen => derive_all_open_row(pr, repo, me, cfg, &[]),
            Mode::Authored | Mode::Review => derive_row(pr, mode, repo, me, cfg),
        })
        .collect();
    match mode {
        // action → await → draft, newest first within each.
        Mode::Authored => rows.sort_by_key(|r| (r.category.rank(), std::cmp::Reverse(r.number))),
        // todo → done → draft, oldest first — clear the backlog.
        Mode::Review => rows.sort_by_key(|r| (r.category.rank(), r.number)),
        Mode::AllOpen => sort_most_recently_updated(&mut rows),
    }
    rows
}

fn derive_involving_rows(prs: &[RawPr], repo: &str, me: &str, cfg: &BoardConfig) -> Vec<BoardRow> {
    let mut rows: Vec<_> = prs
        .iter()
        .take(MAX_BOARD_ROWS)
        .map(|pr| derive_involving_row(pr, repo, me, cfg))
        .collect();
    rows.sort_by(|a, b| {
        (a.category.rank(), std::cmp::Reverse(&a.updated_at), &a.id).cmp(&(
            b.category.rank(),
            std::cmp::Reverse(&b.updated_at),
            &b.id,
        ))
    });
    rows
}

/// All open's rows, most recently updated first. `requested` is what the
/// review queue's search found requesting your review.
fn derive_all_open_rows(
    prs: &[RawPr],
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    requested: &[String],
) -> Vec<BoardRow> {
    let mut rows: Vec<BoardRow> = prs
        .iter()
        .take(MAX_BOARD_ROWS)
        .map(|pr| derive_all_open_row(pr, repo, me, cfg, requested))
        .collect();
    sort_most_recently_updated(&mut rows);
    rows
}

/// One row of All open. Your own PR reads as in My PRs. A PR that asks for
/// your review — the review queue's search found it (`requested`, which
/// counts your teams), or it names you — reads as in the review queue,
/// because in a list of everything that is the row that needs you. Anyone
/// else's reads as in Involving me: the facts, never as your blockers.
fn derive_all_open_row(
    pr: &RawPr,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    requested: &[String],
) -> BoardRow {
    let asks_me =
        pr.id.as_ref().is_some_and(|id| requested.contains(id)) || asks_me_to_review(pr, me);
    if !is_own_pr(pr, me) && !pr.is_draft && asks_me {
        let row = derive_review_row(pr, repo, me, cfg, QueueProvenance::Requested);
        if row.category == Category::Todo {
            return row;
        }
    }
    let mut row = derive_involving_row(pr, repo, me, cfg);
    if !is_own_pr(pr, me) {
        classify_other_author(&mut row, Named::No);
    }
    row
}

/// GitHub names the viewer among the requested reviewers. A request that
/// reached you only through a team does not count: team membership is not in
/// the query, and guessing would put someone else's review at the top.
fn asks_me_to_review(pr: &RawPr, me: &str) -> bool {
    pr.review_requests.nodes.iter().any(|node| {
        node.requested_reviewer
            .as_ref()
            .and_then(|reviewer| reviewer.login.as_deref())
            .is_some_and(|login| login.eq_ignore_ascii_case(me))
    })
}

fn derive_involving_row(pr: &RawPr, repo: &str, me: &str, cfg: &BoardConfig) -> BoardRow {
    if is_own_pr(pr, me) {
        return derive_row(pr, Mode::Authored, repo, me, cfg);
    }

    let mut row = derive_row(pr, Mode::Authored, repo, me, cfg);
    classify_other_author(&mut row, Named::Yes);
    row
}

/// Whether someone else's Note starts with whose PR it is. Involving me has
/// no Author column, so the Note names them; All open has one, and repeating
/// the name in every row would only push the facts out of view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Named {
    Yes,
    No,
}

/// Status-only Note for someone else's PR in the involving view.
fn classify_other_author(row: &mut BoardRow, named: Named) {
    row.blockers.clear();
    let author = row.author.as_deref().unwrap_or("Unknown author").to_owned();
    if row.draft {
        row.category = Category::Draft;
        row.note = match named {
            Named::Yes => format!("draft by {author}"),
            Named::No => "draft".to_owned(),
        };
        return;
    }
    let mut facts = Vec::new();
    if row.conflict {
        facts.push("merge conflict".to_owned());
    }
    if row.ci == Ci::Fail {
        facts.push("CI failing".to_owned());
    }
    if row.review_decision.as_deref() == Some("CHANGES_REQUESTED") {
        facts.push("changes requested".to_owned());
    }
    if row.unresolved > 0 {
        facts.push(unresolved_comments(row.unresolved));
    }
    row.category = if facts.is_empty() {
        Category::Await
    } else {
        Category::Action
    };
    let state = if facts.is_empty() {
        match row.review_state {
            ReviewState::Approved => "approved".to_owned(),
            ReviewState::Commented => "review comments received".to_owned(),
            ReviewState::Waiting if only_teams_asked(row) => {
                "team requested, nobody responded".to_owned()
            }
            ReviewState::Waiting => "awaiting review".to_owned(),
            ReviewState::None | ReviewState::Changes => "open".to_owned(),
        }
    } else {
        facts.join(" · ")
    };
    row.note = match named {
        Named::Yes => format!("{author}'s PR · {state}"),
        Named::No => state,
    };
}

/// Category, blockers, and Note for one of your own PRs, from row facts only
/// (so a carried-forward fact can re-derive them).
fn classify_authored(row: &mut BoardRow, cfg: &BoardConfig) {
    row.category = if row.draft {
        Category::Draft
    } else if row.ci == Ci::Fail
        || row.conflict
        || row.review_decision.as_deref() == Some("CHANGES_REQUESTED")
        || row.unresolved > 0
        || row.review_state == ReviewState::None
    {
        Category::Action
    } else {
        Category::Await
    };
    // Structured blockers first (one source of truth), then the exact
    // legacy note generated from them.
    row.blockers = authored_blockers(row, cfg);
    row.note = authored_note(row);
}

/// Keep a merge conflict GitHub reported earlier while it has not recomputed
/// mergeability (`last_conflict(id)` is the last *known* value, e.g. from the
/// attention snapshots). Without this, every base-branch push briefly turns
/// conflicted PRs into non-conflicted ones: the Note and category flicker and
/// the conflict "changes" and notifies again when GitHub catches up.
/// Returns how many rows were adjusted.
pub fn carry_forward_conflicts(
    rows: &mut [BoardRow],
    last_conflict: impl Fn(&str) -> Option<bool>,
    mode: Mode,
    me: &str,
    cfg: &BoardConfig,
) -> usize {
    let mut adjusted = 0;
    for row in rows.iter_mut() {
        if !row.mergeable_unknown || row.conflict || last_conflict(&row.id) != Some(true) {
            continue;
        }
        row.conflict = true;
        match mode {
            Mode::Review => row.note = review_note(row, me),
            Mode::AllOpen if row.category == Category::Todo => row.note = review_note(row, me),
            Mode::Authored | Mode::AllOpen if row.author.as_deref() == Some(me) => {
                classify_authored(row, cfg)
            }
            Mode::Authored => classify_other_author(row, Named::Yes),
            Mode::AllOpen => classify_other_author(row, Named::No),
        }
        adjusted += 1;
    }
    adjusted
}

fn derive_row(pr: &RawPr, mode: Mode, repo: &str, me: &str, cfg: &BoardConfig) -> BoardRow {
    let labels: Vec<String> = pr.labels.nodes.iter().map(|l| l.name.clone()).collect();
    let bug = labels.iter().any(|l| l == "bug");
    let unresolved = pr
        .review_threads
        .nodes
        .iter()
        .filter(|t| !t.is_resolved)
        .count();
    let ci = derive_ci(pr);
    let conflict = pr.mergeable.as_deref() == Some("CONFLICTING");
    let mergeable_unknown = !matches!(pr.mergeable.as_deref(), Some("MERGEABLE" | "CONFLICTING"));
    let (issue, issue_url, title) = derive_title(&pr.title, cfg);
    let url = pr.canonical_url(repo);

    let mut row = BoardRow {
        id: pr_identity(pr, repo),
        repo: pr.repository_name(repo).to_owned(),
        updated_at: pr.updated_at.clone(),
        head_oid: pr.head_ref_oid.clone(),
        reviewed_oid: latest_review_evidence(pr)
            .and_then(|review| review.commit.as_ref())
            .map(|commit| commit.oid.clone()),
        reviewed_at: latest_review_evidence(pr).and_then(|review| review.submitted_at.clone()),
        number: pr.number,
        url,
        title,
        issue,
        issue_url,
        author: pr.author.as_ref().and_then(|a| a.login.clone()),
        stack: pr.stack.as_ref().map(|stack| StackInfo {
            number: stack.number,
            size: stack.size,
            base_ref_name: stack.base_ref_name.clone(),
            position: pr.stack_entry.as_ref().map(|entry| entry.position),
        }),
        queue_provenance: None,
        draft: pr.is_draft,
        category: Category::Draft, // set below
        bug,
        labels,
        ci,
        conflict,
        mergeable_unknown,
        review_decision: pr.review_decision.clone(),
        review_state: ReviewState::None,
        requested: requested_reviewers(pr),
        requested_teams: requested_teams(pr),
        reviews: latest_reviews_excluding(pr, me, &cfg.bots),
        my_review: Some(my_latest_review(pr, me)),
        unresolved,
        blockers: Vec::new(),
        created_at: pr.created_at.clone(),
        waiting_since: None,
        size: ChangeSize::from_counts(pr.additions, pr.deletions, pr.changed_files),
        note: String::new(),
    };

    match mode {
        // All open rows come from `derive_all_open_row`, which asks for one of
        // the other two; asked directly, a row reads as its author's.
        Mode::Authored | Mode::AllOpen => {
            let appr = row.reviews.iter().filter(|r| r.state == "APPROVED").count();
            let cmt = row.reviews.iter().any(|r| r.state == "COMMENTED");
            let chg = row.reviews.iter().any(|r| r.state == "CHANGES_REQUESTED");
            row.review_state = if chg {
                ReviewState::Changes
            } else if appr > 0 {
                ReviewState::Approved
            } else if cmt {
                ReviewState::Commented
            } else if !row.requested.is_empty() {
                ReviewState::Waiting
            } else {
                ReviewState::None
            };
            classify_authored(&mut row, cfg);
        }
        Mode::Review => {
            row.category = if pr.is_draft {
                Category::Draft
            } else if matches!(
                row.my_review.as_deref(),
                Some("APPROVED" | "COMMENTED" | "CHANGES_REQUESTED")
            ) {
                Category::Done
            } else {
                Category::Todo
            };
            row.note = review_note(&row, me);
        }
    }
    row.waiting_since = pickup_since(pr, &row, me);
    row
}

fn derive_review_row(
    pr: &RawPr,
    repo: &str,
    me: &str,
    cfg: &BoardConfig,
    provenance: QueueProvenance,
) -> BoardRow {
    let mut row = derive_row(pr, Mode::Review, repo, me, cfg);
    row.queue_provenance = Some(provenance);
    if provenance == QueueProvenance::Available
        && !matches!(row.category, Category::Done | Category::Draft)
    {
        row.category = Category::Available;
        row.note = review_note(&row, me);
        row.waiting_since = pickup_since(pr, &row, me);
    }
    row
}

fn derive_ci(pr: &RawPr) -> Ci {
    let rollup = pr
        .commits
        .nodes
        .first()
        .and_then(|c| c.commit.status_check_rollup.as_ref());
    if rollup.is_some_and(|r| r.hidden) {
        return Ci::Hidden;
    }
    let state = rollup.and_then(|r| r.state.as_deref()).unwrap_or("NONE");
    match state {
        "SUCCESS" => Ci::Pass,
        "FAILURE" | "ERROR" => Ci::Fail,
        "NONE" => Ci::None,
        _ => Ci::Running, // PENDING / EXPECTED
    }
}

fn requested_teams(pr: &RawPr) -> Vec<String> {
    pr.review_requests
        .nodes
        .iter()
        .filter_map(|n| n.requested_reviewer.as_ref())
        .filter(|r| r.login.is_none())
        .filter_map(|r| r.slug.clone())
        .collect()
}

/// Everyone requested is a team, and nobody has reviewed: a request that
/// names no person can sit unnoticed.
fn only_teams_asked(row: &BoardRow) -> bool {
    !row.requested.is_empty()
        && row.requested.len() == row.requested_teams.len()
        && row.reviews.is_empty()
}

fn requested_reviewers(pr: &RawPr) -> Vec<String> {
    pr.review_requests
        .nodes
        .iter()
        .filter_map(|n| n.requested_reviewer.as_ref())
        .filter_map(|r| r.login.clone().or_else(|| r.slug.clone()))
        .collect()
}

/// The review that stands for one reviewer: their latest, except that a
/// comment does not erase an earlier approval or change request. A later
/// change request or dismissal still does. `reviews` are that reviewer's, in
/// response order; ties on `submittedAt` go to the later one, like jq's
/// `max_by`. The prototype jq applies the same rule.
fn standing_review<'a>(reviews: &[&'a ReviewNode]) -> Option<&'a ReviewNode> {
    let latest = |keep: fn(&ReviewNode) -> bool| {
        reviews
            .iter()
            .copied()
            .filter(|review| keep(review))
            .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
    };
    let last = latest(|_| true)?;
    if last.state == "COMMENTED" {
        if let Some(held) = latest(|review| review.state != "COMMENTED") {
            if matches!(held.state.as_str(), "APPROVED" | "CHANGES_REQUESTED") {
                return Some(held);
            }
        }
    }
    Some(last)
}

/// Standing review state per author, excluding the PR author and bots — the
/// prototype's `$rv`. Ordered by login (`group_by` sorts; null first).
fn latest_reviews_excluding(pr: &RawPr, me: &str, bots: &[String]) -> Vec<ReviewSummary> {
    let mut by_author: Vec<(Option<String>, Vec<&ReviewNode>)> = Vec::new();
    for review in &pr.reviews.nodes {
        let login = review.author.as_ref().and_then(|a| a.login.clone());
        if let Some(l) = &login {
            if l == me || bots.iter().any(|b| b == l) {
                continue;
            }
        }
        match by_author.iter_mut().find(|(l, _)| *l == login) {
            Some((_, reviews)) => reviews.push(review),
            None => by_author.push((login, vec![review])),
        }
    }
    by_author.sort_by(|a, b| a.0.cmp(&b.0));
    by_author
        .into_iter()
        .filter_map(|(login, reviews)| {
            // The standing review's own time, so a comment after an approval
            // is not reported as a new approval.
            standing_review(&reviews).map(|review| ReviewSummary {
                login,
                state: review.state.clone(),
                submitted_at: review.submitted_at.clone(),
            })
        })
        .collect()
}

fn latest_review_evidence(pr: &RawPr) -> Option<&crate::github::query::LatestReview> {
    pr.latest_review
        .nodes
        .first()
        .filter(|review| review.state != "DISMISSED")
}

/// The user's own standing review state, or "NONE" — the prototype's `$mine`.
fn my_latest_review(pr: &RawPr, me: &str) -> String {
    let mine: Vec<&ReviewNode> = pr
        .reviews
        .nodes
        .iter()
        .filter(|r| r.author.as_ref().and_then(|a| a.login.as_deref()) == Some(me))
        .collect();
    standing_review(&mine)
        .map(|r| r.state.clone())
        .unwrap_or_else(|| "NONE".to_string())
}

/// `text` as one URL path or query component: the characters a URL never
/// needs to escape stay as they are, so an ordinary `PROJ-123` reads the same,
/// and every other byte is percent-encoded — `/`, `#`, `?` and a space
/// included, so a matched ID can never end the path or start a query. The
/// template around it is the user's own URL and stays untouched.
fn url_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn derive_title(raw_title: &str, cfg: &BoardConfig) -> (Option<String>, Option<String>, String) {
    let (issue, issue_url) = match &cfg.issue_link {
        Some(rule) => match rule.pattern.find(raw_title) {
            Some(m) => {
                let id = m.as_str().to_string();
                let url = rule.url_template.replace("{id}", &url_component(&id));
                (Some(id), Some(url))
            }
            None => (None, None),
        },
        None => (None, None),
    };

    let mut title = raw_title.to_string();
    if let Some(rest) = title.strip_prefix("WIP") {
        title = rest.trim_start().to_string();
    }
    if let Some(rule) = &cfg.issue_link {
        title = rule.strip_prefix.replace(&title, "").to_string();
    }
    (issue, issue_url, title)
}

/// The action-note conditions as structured facts, in the prototype's
/// canonical most-blocking-first order (no reviewers, merge conflict, CI
/// failure, changes requested, unresolved comments). A non-draft authored row
/// is `Action` iff this list is non-empty and `Await` iff it is empty, so
/// await rows carry no blockers.
fn authored_blockers(row: &BoardRow, cfg: &BoardConfig) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    if row.review_state == ReviewState::None {
        blockers.push(Blocker::NoReviewers {
            suggested: cfg.suggested_reviewers(&row.repo).to_vec(),
        });
    }
    if row.conflict {
        blockers.push(Blocker::MergeConflict);
    }
    if row.ci == Ci::Fail {
        blockers.push(Blocker::CiFailing);
    }
    if row.review_decision.as_deref() == Some("CHANGES_REQUESTED") {
        blockers.push(Blocker::ChangesRequested);
    }
    if row.unresolved > 0 {
        blockers.push(Blocker::UnresolvedComments(row.unresolved));
    }
    blockers
}

/// The exact SKILL.md note fragment for one blocker — the prototype's wording
/// and emoji, verbatim. The joined fragments reproduce the legacy `note`
/// byte-for-byte (pinned by the golden tests).
fn blocker_note(blocker: &Blocker) -> String {
    match blocker {
        Blocker::NoReviewers { suggested } => {
            if suggested.is_empty() {
                "⚠️ no reviewers".to_string()
            } else {
                format!("⚠️ no reviewers — assign {}", suggested.join(" + "))
            }
        }
        Blocker::MergeConflict => "🔴 merge conflict — rebase".to_string(),
        Blocker::CiFailing => "❌ CI failing".to_string(),
        Blocker::ChangesRequested => "✋ changes requested".to_string(),
        Blocker::UnresolvedComments(n) => format!("🟡 {}", unresolved_comments(*n)),
    }
}

/// "1 unresolved comment", "2 unresolved comments". SKILL.md writes the rule
/// as "<n> unresolved comments"; the count decides the plural.
fn unresolved_comments(n: usize) -> String {
    format!("{n} unresolved comment{}", if n == 1 { "" } else { "s" })
}

/// Notes keep the prototype's emoji language (the SKILL spec). Surfaces that
/// carry the signal another way — the board's themed status dots, shared
/// plain text — strip the glyphs for display; the note itself never changes.
pub fn strip_note_glyphs(note: &str) -> String {
    const GLYPHS: &[&str] = &[
        "⚠️ ", "🔴 ", "❌ ", "✋ ", "🟡 ", "🟢 ", "✅ ", "💬 ", "🔵 ",
    ];
    let mut s = note.to_string();
    for g in GLYPHS {
        s = s.replace(g, "");
    }
    // "·" is the prototype's neutral glyph ("· draft"). Leading, it is
    // decoration like the emoji; between facts (" · ") it is a separator and
    // stays, collapsed where a glyph-led note was appended to another.
    let s = s.replace(" · · ", " · ");
    match s.strip_prefix("· ") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

/// Mode A Note (SKILL.md): action rows combine every applicable blocker,
/// most-blocking first; await/draft rows are single-state. Action rows render
/// straight from `row.blockers`, so the note and the structured list can never
/// disagree.
fn authored_note(row: &BoardRow) -> String {
    match row.category {
        Category::Action => row
            .blockers
            .iter()
            .map(blocker_note)
            .collect::<Vec<_>>()
            .join(" · "),
        Category::Await => match row.review_state {
            ReviewState::Approved => "🟢 approved — mergeable".to_string(),
            ReviewState::Commented => "🟢 commented — awaiting approval".to_string(),
            ReviewState::Waiting if only_teams_asked(row) => {
                "✅ awaiting review — team requested, nobody responded".to_string()
            }
            _ => "✅ awaiting review".to_string(),
        },
        Category::Draft => {
            if row.conflict {
                "🔴 draft · merge conflict".to_string()
            } else if row.unresolved > 0 {
                format!("🟡 draft · {}", unresolved_comments(row.unresolved))
            } else if row.ci == Ci::Fail {
                "🔴 draft · CI failing".to_string()
            } else {
                "· draft".to_string()
            }
        }
        _ => String::new(),
    }
}

/// Mode B Note (SKILL.md). A request that reached you only through a team,
/// with no review yet, says so.
fn review_note(row: &BoardRow, me: &str) -> String {
    let note = match row.category {
        Category::Todo | Category::Available => {
            if row.ci == Ci::Fail {
                "⚠️ CI red — maybe wait for green".to_string()
            } else if row.conflict {
                "⚠️ has conflicts".to_string()
            } else if row.category == Category::Available {
                "available for review".to_string()
            } else if !row.requested_teams.is_empty()
                && row.reviews.is_empty()
                && !row
                    .requested
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(me) && !row.requested_teams.contains(r))
            {
                "🔵 team requested, nobody responded".to_string()
            } else {
                "🔵 needs your review".to_string()
            }
        }
        Category::Done => match row.my_review.as_deref() {
            Some("APPROVED") => "✅ you approved".to_string(),
            Some("CHANGES_REQUESTED") => "✋ you requested changes — on the author now".to_string(),
            Some("COMMENTED") => "💬 you commented".to_string(),
            _ => String::new(),
        },
        Category::Draft => "· draft (not ready)".to_string(),
        _ => String::new(),
    };
    if row.reviewed_oid.is_some()
        && row.head_oid.is_some()
        && row.reviewed_oid != row.head_oid
        && !note.is_empty()
    {
        format!("new commits since your review · {note}")
    } else {
        note
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct FakeTransport(serde_json::Value);

    impl GithubTransport for FakeTransport {
        fn graphql(
            &self,
            query: &str,
            variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            assert_eq!(query, with_scope_repository(REVIEW_SEARCH_QUERY).unwrap());
            assert_eq!(variables.len(), 5);
            assert!(variables.contains(&("who", "me")));
            assert!(variables.iter().any(|(key, _)| *key == "scopeOwner"));
            assert!(variables.iter().any(|(key, _)| *key == "scopeName"));
            Ok(self.0.clone())
        }
    }

    struct SequenceTransport(Mutex<VecDeque<Result<serde_json::Value, GhError>>>);

    impl SequenceTransport {
        fn new(values: Vec<Result<serde_json::Value, GhError>>) -> Self {
            Self(Mutex::new(values.into()))
        }
    }

    impl GithubTransport for SequenceTransport {
        fn graphql(
            &self,
            _query: &str,
            _variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            self.0.lock().unwrap().pop_front().unwrap()
        }
    }

    struct GlobalReviewTransport;

    impl GithubTransport for GlobalReviewTransport {
        fn graphql(
            &self,
            query: &str,
            variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            assert_eq!(query, REVIEW_SEARCH_QUERY);
            assert!(
                variables.contains(&("requested", "is:pr is:open review-requested:me -author:me"))
            );
            assert!(variables.contains(&("available", "is:pr is:open involves:me -author:me")));
            Ok(json!({"data": {
                "requested": {"pageInfo":{"hasNextPage":false}, "nodes":[]},
                "available": {"pageInfo":{"hasNextPage":false}, "nodes":[]},
                "rateLimit": null
            }}))
        }
    }

    fn cfg() -> BoardConfig {
        BoardConfig {
            default_reviewers: vec!["alice".into(), "bob".into()],
            issue_link: Some(
                IssueLinkRule::new("PROJ-[0-9]+", "https://tracker.example.test/issues/{id}")
                    .unwrap(),
            ),
            ..Default::default()
        }
    }

    fn pr(v: serde_json::Value) -> RawPr {
        serde_json::from_value(v).unwrap()
    }

    fn base(number: u64) -> serde_json::Value {
        json!({
            "number": number,
            "title": "Some change",
            "isDraft": false,
            "reviewDecision": null,
            "mergeable": "MERGEABLE",
            "createdAt": "2026-07-20T10:00:00Z",
            "author": {"login": "me"},
            "labels": {"nodes": []},
            "reviewRequests": {"nodes": []},
            "reviews": {"nodes": []},
            "reviewThreads": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}
        })
    }

    fn derive_one(v: serde_json::Value, mode: Mode) -> BoardRow {
        derive_rows(&[pr(v)], mode, "acme/widgets", "me", &cfg())
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn a_token_refused_checks_and_teams_still_gets_its_board() {
        let refused = |path: serde_json::Value| {
            json!({"type": "FORBIDDEN", "path": path,
                   "message": "Resource not accessible by personal access token"})
        };
        // The shape GitHub returned to a fine-grained token on 2026-09-18: the
        // head commit node refused and null, once per pull request.
        let mut first = base(1);
        first["commits"] = json!({"nodes": [null]});
        first["reviewRequests"] = json!({"totalCount": 1, "nodes": [{"requestedReviewer": null}]});
        let mut second = base(2);
        second["commits"] = json!({"nodes": [null]});
        let rollup = |i: u64| json!(["search", "nodes", i, "commits", "nodes", 0]);
        let body = json!({
            "data": {
                "search": {"pageInfo": {"hasNextPage": false}, "nodes": [first, second]},
                "rateLimit": null
            },
            "errors": [
                refused(rollup(0)),
                refused(rollup(1)),
                refused(json!(["search", "nodes", 0, "reviewRequests", "nodes", 0, "requestedReviewer"])),
            ]
        });

        let fetch = fetch_board_scoped(
            &SequenceTransport::new(vec![Ok(body)]),
            Mode::Authored,
            &BoardScope::AllRepositories,
            "me",
            &cfg(),
        )
        .expect("refused fields are not a failed refresh");

        assert_eq!(fetch.rows.len(), 2);
        assert!(fetch.rows.iter().all(|row| row.ci == Ci::Hidden));
        let asked_a_team = fetch.rows.iter().find(|row| row.number == 1).unwrap();
        assert_eq!(
            asked_a_team.requested,
            vec![crate::github::access::HIDDEN_TEAM]
        );
        assert!(
            !asked_a_team.note.contains("No reviewers"),
            "a team the token cannot see was still asked: {}",
            asked_a_team.note
        );
        assert_eq!(
            fetch.access,
            AccessGaps {
                ci: 2,
                teams: 1,
                other: 0,
                pull_requests: 0
            }
        );
        let notice = crate::status::access_notice(&fetch.access).unwrap();
        assert!(notice.starts_with("This token can't read CI on 2 pull requests"));
        assert!(!notice.contains('\n'));
    }

    #[test]
    fn a_refused_search_still_fails_and_says_so_once() {
        let refused = json!({"type": "FORBIDDEN", "path": ["search"],
                             "message": "Resource not accessible by personal access token"});
        let body = json!({"data": null, "errors": [refused.clone(), refused]});
        let error = fetch_board_scoped(
            &SequenceTransport::new(vec![Ok(body)]),
            Mode::Authored,
            &BoardScope::AllRepositories,
            "me",
            &cfg(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "GraphQL errors: Resource not accessible by personal access token"
        );
    }

    #[test]
    fn stripping_note_glyphs_keeps_separators_but_drops_the_neutral_lead() {
        assert_eq!(strip_note_glyphs("· draft"), "draft");
        assert_eq!(
            strip_note_glyphs("· draft (not ready)"),
            "draft (not ready)"
        );
        assert_eq!(
            strip_note_glyphs("🔴 draft · merge conflict"),
            "draft · merge conflict"
        );
        assert_eq!(
            strip_note_glyphs("new commits since your review · · draft (not ready)"),
            "new commits since your review · draft (not ready)"
        );
        assert_eq!(
            strip_note_glyphs("🔴 merge conflict — rebase · 🔴 CI failing"),
            "merge conflict — rebase · CI failing"
        );
    }

    #[test]
    fn no_reviewers_is_action_with_assign_note() {
        let row = derive_one(base(1), Mode::Authored);
        assert_eq!(row.category, Category::Action);
        assert_eq!(row.review_state, ReviewState::None);
        assert_eq!(row.note, "⚠️ no reviewers — assign alice + bob");
    }

    #[test]
    fn defaults_are_organization_and_tracker_agnostic() {
        let mut v = base(14);
        v["title"] = json!("[PROJ-1234] Fix the crash");
        let row = derive_rows(
            &[pr(v)],
            Mode::Authored,
            "acme/widgets",
            "me",
            &BoardConfig::default(),
        )
        .into_iter()
        .next()
        .unwrap();

        assert!(row.issue.is_none());
        assert!(row.issue_url.is_none());
        assert_eq!(row.title, "[PROJ-1234] Fix the crash");
        assert_eq!(row.note, "⚠️ no reviewers");
    }

    #[test]
    fn labels_carry_through_and_bug_is_derived() {
        let mut v = base(9);
        v["labels"]["nodes"] = json!([
            {"name": "bug"}, {"name": "backend"}, {"name": "P1"}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.labels, vec!["bug", "backend", "P1"]);
        assert!(row.bug);
        let no_labels = derive_one(base(10), Mode::Authored);
        assert!(no_labels.labels.is_empty());
        assert!(!no_labels.bug);
    }

    #[test]
    fn approved_is_await_mergeable() {
        let mut v = base(2);
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.category, Category::Await);
        assert_eq!(row.review_state, ReviewState::Approved);
        assert_eq!(row.note, "🟢 approved — mergeable");
    }

    #[test]
    fn action_note_combines_in_blocking_order() {
        let mut v = base(3);
        v["mergeable"] = json!("CONFLICTING");
        v["reviewDecision"] = json!("CHANGES_REQUESTED");
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
        v["reviewThreads"]["nodes"] = json!([
            {"isResolved": false}, {"isResolved": false}, {"isResolved": true}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.category, Category::Action);
        assert_eq!(
            row.note,
            "⚠️ no reviewers — assign alice + bob · 🔴 merge conflict — rebase · \
             ❌ CI failing · ✋ changes requested · 🟡 2 unresolved comments"
        );
    }

    #[test]
    fn no_reviewer_blocker_carries_configured_reviewers() {
        let row = derive_one(base(1), Mode::Authored);
        assert_eq!(
            row.blockers,
            vec![Blocker::NoReviewers {
                suggested: vec!["alice".into(), "bob".into()]
            }]
        );
    }

    #[test]
    fn reviewer_suggestions_prefer_the_repository_then_its_owner() {
        let mut cfg = cfg();
        cfg.repo_reviewers
            .insert("acme".into(), vec!["olga".into(), "oscar".into()]);
        cfg.repo_reviewers
            .insert("acme/widgets".into(), vec!["rita".into()]);
        cfg.repo_reviewers.insert("quiet/repo".into(), Vec::new());

        assert_eq!(cfg.suggested_reviewers("Acme/Widgets"), ["rita"]);
        assert_eq!(cfg.suggested_reviewers("acme/api"), ["olga", "oscar"]);
        assert_eq!(cfg.suggested_reviewers("other/repo"), ["alice", "bob"]);
        assert!(cfg.suggested_reviewers("quiet/repo").is_empty());

        let row = derive_rows(&[pr(base(1))], Mode::Authored, "acme/widgets", "me", &cfg)
            .into_iter()
            .next()
            .unwrap();
        assert!(row.note.contains("assign rita"), "{}", row.note);
    }

    #[test]
    fn no_reviewer_blocker_is_empty_without_config() {
        let row = derive_rows(
            &[pr(base(1))],
            Mode::Authored,
            "acme/widgets",
            "me",
            &BoardConfig::default(),
        )
        .into_iter()
        .next()
        .unwrap();
        assert_eq!(
            row.blockers,
            vec![Blocker::NoReviewers { suggested: vec![] }]
        );
    }

    #[test]
    fn kitchen_sink_row_lists_every_blocker_in_prototype_order() {
        let mut v = base(3);
        v["mergeable"] = json!("CONFLICTING");
        v["reviewDecision"] = json!("CHANGES_REQUESTED");
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
        v["reviewThreads"]["nodes"] = json!([
            {"isResolved": false}, {"isResolved": false}, {"isResolved": true}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(
            row.blockers,
            vec![
                Blocker::NoReviewers {
                    suggested: vec!["alice".into(), "bob".into()]
                },
                Blocker::MergeConflict,
                Blocker::CiFailing,
                Blocker::ChangesRequested,
                Blocker::UnresolvedComments(2),
            ]
        );
        // And the generated note is byte-identical to the legacy wording.
        assert_eq!(
            row.note,
            "⚠️ no reviewers — assign alice + bob · 🔴 merge conflict — rebase · \
             ❌ CI failing · ✋ changes requested · 🟡 2 unresolved comments"
        );
    }

    #[test]
    fn await_rows_have_no_blockers() {
        let mut v = base(2);
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.category, Category::Await);
        assert!(row.blockers.is_empty());
    }

    #[test]
    fn approved_with_unresolved_is_still_action() {
        let mut v = base(4);
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "grace"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        v["reviewThreads"]["nodes"] = json!([{"isResolved": false}, {"isResolved": false}]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.category, Category::Action);
        assert_eq!(row.review_state, ReviewState::Approved);
        assert_eq!(row.note, "🟡 2 unresolved comments");

        let mut one = base(5);
        one["reviews"]["nodes"] = json!([
            {"author": {"login": "grace"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        one["reviewThreads"]["nodes"] = json!([{"isResolved": false}]);
        assert_eq!(
            derive_one(one, Mode::Authored).note,
            "🟡 1 unresolved comment"
        );
    }

    #[test]
    fn bot_and_own_reviews_are_excluded() {
        let mut v = base(5);
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "github-actions"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:00:00Z"},
            {"author": {"login": "chatgpt-codex-connector"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:05:00Z"},
            {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:10:00Z"}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert!(row.reviews.is_empty());
        assert_eq!(row.review_state, ReviewState::None);
        assert_eq!(row.category, Category::Action);
    }

    #[test]
    fn latest_review_per_author_wins() {
        let mut v = base(6);
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "eve"}, "state": "COMMENTED", "submittedAt": "2026-07-21T09:00:00Z"},
            {"author": {"login": "eve"}, "state": "APPROVED", "submittedAt": "2026-07-22T09:00:00Z"}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(
            row.reviews,
            vec![ReviewSummary {
                login: Some("eve".into()),
                state: "APPROVED".into(),
                submitted_at: Some("2026-07-22T09:00:00Z".into()),
            }]
        );
        assert_eq!(row.review_state, ReviewState::Approved);
    }

    #[test]
    fn a_comment_does_not_erase_an_approval_or_a_change_request() {
        let mut v = base(60);
        let (t1, t2) = ("2026-07-21T09:00:00Z", "2026-07-22T09:00:00Z");
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "eve"}, "state": "APPROVED", "submittedAt": t1},
            {"author": {"login": "eve"}, "state": "COMMENTED", "submittedAt": t2},
            {"author": {"login": "fay"}, "state": "APPROVED", "submittedAt": t1},
            {"author": {"login": "fay"}, "state": "DISMISSED", "submittedAt": t2},
            {"author": {"login": "gus"}, "state": "DISMISSED", "submittedAt": t1},
            {"author": {"login": "gus"}, "state": "COMMENTED", "submittedAt": t2},
            {"author": {"login": "hal"}, "state": "CHANGES_REQUESTED", "submittedAt": t1},
            {"author": {"login": "hal"}, "state": "COMMENTED", "submittedAt": t2},
            {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": t1},
            {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": t2}
        ]);
        let row = derive_one(v.clone(), Mode::Authored);
        let standing: Vec<_> = row
            .reviews
            .iter()
            .map(|r| {
                (
                    r.login.as_deref().unwrap(),
                    r.state.as_str(),
                    r.submitted_at.as_deref().unwrap(),
                )
            })
            .collect();
        // The standing review keeps its own time, so a later comment is not
        // reported as a new approval.
        assert_eq!(
            standing,
            vec![
                ("eve", "APPROVED", t1),
                ("fay", "DISMISSED", t2),
                ("gus", "COMMENTED", t2),
                ("hal", "CHANGES_REQUESTED", t1),
            ]
        );
        assert_eq!(row.review_state, ReviewState::Changes);
        // The viewer's own standing review is known in every mode.
        assert_eq!(row.my_review.as_deref(), Some("APPROVED"));
        let review = derive_one(v, Mode::Review);
        assert_eq!(review.my_review.as_deref(), Some("APPROVED"));
        assert_eq!(review.category, Category::Done);
        assert_eq!(review.note, "✅ you approved");
    }

    #[test]
    fn team_slug_counts_as_requested_reviewer() {
        let mut v = base(7);
        v["reviewRequests"]["nodes"] = json!([
            {"requestedReviewer": {"__typename": "Team", "slug": "platform"}},
            {"requestedReviewer": null}
        ]);
        let row = derive_one(v.clone(), Mode::Authored);
        assert_eq!(row.requested, vec!["platform"]);
        assert_eq!(row.requested_teams, vec!["platform"]);
        assert_eq!(row.review_state, ReviewState::Waiting);
        assert_eq!(row.category, Category::Await);
        assert_eq!(
            row.note,
            "✅ awaiting review — team requested, nobody responded"
        );

        // A person asked by name is on the hook.
        let mut named = v.clone();
        named["reviewRequests"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"requestedReviewer": {"__typename": "User", "login": "alice"}}));
        let row = derive_one(named, Mode::Authored);
        assert_eq!(row.requested_teams, vec!["platform"]);
        assert_eq!(row.note, "✅ awaiting review");

        // Someone else's PR in the involving view says the same.
        let mut theirs = v.clone();
        theirs["author"] = json!({"login": "alice"});
        let rows = derive_involving_rows(&[pr(theirs)], "acme/widgets", "me", &cfg());
        assert_eq!(
            rows[0].note,
            "alice's PR · team requested, nobody responded"
        );

        // The review queue: asked only through the team, and nobody reviewed.
        v["author"] = json!({"login": "alice"});
        let row = derive_one(v.clone(), Mode::Review);
        assert_eq!(row.category, Category::Todo);
        assert_eq!(row.note, "🔵 team requested, nobody responded");
        let mut by_name = v.clone();
        by_name["reviewRequests"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"requestedReviewer": {"__typename": "User", "login": "Me"}}));
        assert_eq!(
            derive_one(by_name, Mode::Review).note,
            "🔵 needs your review"
        );
        let mut reviewed = v.clone();
        reviewed["reviews"]["nodes"] = json!([
            {"author": {"login": "bob"}, "state": "COMMENTED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        assert_eq!(
            derive_one(reviewed, Mode::Review).note,
            "🔵 needs your review"
        );
    }

    #[test]
    fn pickup_age_follows_the_row_not_the_prototype_view() {
        let mut v = base(12);
        v["author"] = json!({"login": "alice"});
        v["timelineItems"] = json!({"nodes": [
            {"__typename": "ReadyForReviewEvent", "createdAt": "2026-07-21T09:00:00Z"}
        ]});
        // Available to review: since opened, moved up to ready for review.
        let available = derive_review_row(
            &pr(v.clone()),
            "acme/widgets",
            "me",
            &cfg(),
            QueueProvenance::Available,
        );
        assert_eq!(available.category, Category::Available);
        assert_eq!(
            available.waiting_since.as_deref(),
            Some("2026-07-21T09:00:00Z")
        );
        // Once you reviewed it, it is not waiting on you.
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "me"}, "state": "COMMENTED", "submittedAt": "2026-07-22T10:00:00Z"}
        ]);
        let done = derive_review_row(
            &pr(v.clone()),
            "acme/widgets",
            "me",
            &cfg(),
            QueueProvenance::Available,
        );
        assert_eq!(done.category, Category::Done);
        assert_eq!(done.waiting_since, None);
        // Someone else's PR you reviewed has been picked up, although your
        // review does not count toward its review state.
        let rows = derive_involving_rows(&[pr(v.clone())], "acme/widgets", "me", &cfg());
        assert_eq!(rows[0].review_state, ReviewState::None);
        assert_eq!(rows[0].waiting_since, None);
        // Your own comment on your PR is not a pickup.
        v["author"] = json!({"login": "me"});
        let mine = derive_one(v, Mode::Authored);
        assert_eq!(mine.waiting_since.as_deref(), Some("2026-07-21T09:00:00Z"));

        let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T08:59:59Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            crate::pickup::waiting_secs(&mine, now),
            Some(3 * 86_400 - 1)
        );
        assert!(!crate::pickup::is_stale(&mine, now, 3));
        assert!(crate::pickup::is_stale(
            &mine,
            now + chrono::Duration::seconds(1),
            3
        ));
        assert!(crate::pickup::is_stale(&mine, now, 2));
        assert!(!crate::pickup::is_stale(&done, now, 0));
        // A clock behind GitHub's reads as no wait yet.
        let early = now - chrono::Duration::days(30);
        assert_eq!(crate::pickup::waiting_secs(&mine, early), Some(0));
    }

    #[test]
    fn title_issue_extraction_and_stripping() {
        let mut v = base(8);
        v["title"] = json!("WIP [PROJ-1234] Fix the crash");
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.issue.as_deref(), Some("PROJ-1234"));
        assert_eq!(
            row.issue_url.as_deref(),
            Some("https://tracker.example.test/issues/PROJ-1234")
        );
        assert_eq!(row.title, "Fix the crash");
    }

    #[test]
    fn a_matched_issue_id_is_percent_encoded_into_the_link() {
        let rule = IssueLinkRule::new(
            r"T-[a-z #?/]+[0-9]",
            "https://tracker.test/browse/{id}?view=1",
        )
        .unwrap();
        let cfg = BoardConfig {
            issue_link: Some(rule),
            ..cfg()
        };
        let mut v = base(9);
        v["title"] = json!("[T-a b#c?d/1] Odd ticket ids");
        let row = derive_rows(&[pr(v)], Mode::Authored, "acme/widgets", "me", &cfg)
            .into_iter()
            .next()
            .unwrap();
        // The ID reads as written; only the link escapes it.
        assert_eq!(row.issue.as_deref(), Some("T-a b#c?d/1"));
        assert_eq!(
            row.issue_url.as_deref(),
            Some("https://tracker.test/browse/T-a%20b%23c%3Fd%2F1?view=1")
        );
        assert_eq!(row.title, "Odd ticket ids");
        // Unreserved characters and non-ASCII (as UTF-8 bytes).
        assert_eq!(url_component("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(url_component("é"), "%C3%A9");
    }

    #[test]
    fn draft_notes_first_match_wins() {
        let mut v = base(9);
        v["isDraft"] = json!(true);
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]);
        assert_eq!(
            derive_one(v.clone(), Mode::Authored).note,
            "🔴 draft · CI failing"
        );
        v["mergeable"] = json!("CONFLICTING");
        assert_eq!(
            derive_one(v.clone(), Mode::Authored).note,
            "🔴 draft · merge conflict"
        );
        v["mergeable"] = json!("MERGEABLE");
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]);
        v["reviewThreads"]["nodes"] = json!([{"isResolved": false}]);
        assert_eq!(
            derive_one(v, Mode::Authored).note,
            "🟡 draft · 1 unresolved comment"
        );
    }

    #[test]
    fn review_mode_categories_and_notes() {
        // Not yet reviewed, green.
        let mut v = base(20);
        v["author"] = json!({"login": "alice"});
        let row = derive_one(v.clone(), Mode::Review);
        assert_eq!(row.category, Category::Todo);
        assert_eq!(row.my_review.as_deref(), Some("NONE"));
        assert_eq!(row.note, "🔵 needs your review");

        // CI red beats conflicts.
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "ERROR"}}}]);
        v["mergeable"] = json!("CONFLICTING");
        assert_eq!(
            derive_one(v.clone(), Mode::Review).note,
            "⚠️ CI red — maybe wait for green"
        );

        // I approved → done.
        v["commits"]["nodes"] = json!([{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]);
        v["mergeable"] = json!("MERGEABLE");
        v["reviews"]["nodes"] = json!([
            {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        let row = derive_one(v.clone(), Mode::Review);
        assert_eq!(row.category, Category::Done);
        assert_eq!(row.note, "✅ you approved");

        // Draft trumps everything.
        v["isDraft"] = json!(true);
        assert_eq!(derive_one(v, Mode::Review).note, "· draft (not ready)");
    }

    #[test]
    fn sort_orders_differ_by_mode() {
        let mk = |n: u64, draft: bool, reviewed: bool| {
            let mut v = base(n);
            v["isDraft"] = json!(draft);
            if reviewed {
                v["reviews"]["nodes"] = json!([
                    {"author": {"login": "alice"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
                ]);
            }
            pr(v)
        };
        // 10=action (no reviewers), 11=await (approved), 12=draft, 13=action
        let prs = vec![
            mk(10, false, false),
            mk(11, false, true),
            mk(12, true, false),
            mk(13, false, false),
        ];
        let authored = derive_rows(&prs, Mode::Authored, "acme/widgets", "me", &cfg());
        let order: Vec<u64> = authored.iter().map(|r| r.number).collect();
        assert_eq!(order, vec![13, 10, 11, 12]); // action desc, then await, then draft

        // Review mode: todo rows sort ascending, followed by rows I reviewed.
        // — for review-mode sorting use my own reviews instead:
        let mk_r = |n: u64, mine: bool| {
            let mut v = base(n);
            v["author"] = json!({"login": "alice"});
            if mine {
                v["reviews"]["nodes"] = json!([
                    {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-07-21T10:00:00Z"}
                ]);
            }
            pr(v)
        };
        let prs = vec![mk_r(31, true), mk_r(30, false), mk_r(28, false)];
        let review = derive_rows(&prs, Mode::Review, "acme/widgets", "me", &cfg());
        let order: Vec<u64> = review.iter().map(|r| r.number).collect();
        assert_eq!(order, vec![28, 30, 31]); // todo asc, then done
    }

    #[test]
    fn rows_are_bounded() {
        let prs: Vec<RawPr> = (0..100).map(|n| pr(base(n))).collect();
        let rows = derive_rows(&prs, Mode::Authored, "acme/widgets", "me", &cfg());
        assert_eq!(rows.len(), MAX_BOARD_ROWS);
    }

    #[test]
    fn expanded_review_queue_filters_deduplicates_and_preserves_states() {
        let mut requested = base(10);
        requested["author"] = json!({"login": "alice"});
        requested["reviewRequests"] = json!({"totalCount": 1, "nodes": [
            {"requestedReviewer": {"__typename": "User", "login": "me"}}
        ]});
        requested["reviews"]["nodes"] = json!([
            {"author": {"login": "bob"}, "state": "COMMENTED", "submittedAt": "2026-07-21T10:00:00Z"}
        ]);
        requested["labels"] = json!({"nodes": [{"name": "bug"}, {"name": "backend"}]});
        requested["stack"] = json!({"number": 70, "size": 3, "baseRefName": "main"});
        requested["stackEntry"] = json!({"position": 2});

        let mut duplicate = requested.clone();
        duplicate["title"] = json!("broad duplicate must lose");

        let mut available = base(11);
        available["author"] = json!({"login": "bob"});
        available["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
        available["labels"] = json!({"nodes": [{"name": "frontend"}]});
        available["stack"] = json!({"number": 70, "size": 3, "baseRefName": "main"});
        available["stackEntry"] = json!({"position": 3});

        let mut completed = base(12);
        completed["author"] = json!({"login": "carol"});
        completed["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
        completed["reviews"]["nodes"] = json!([{
            "author": {"login": "me"}, "state": "APPROVED",
            "submittedAt": "2026-07-21T10:00:00Z"
        }]);

        let mut draft = base(13);
        draft["author"] = json!({"login": "dave"});
        draft["isDraft"] = json!(true);
        draft["reviewRequests"] = json!({"totalCount": 0, "nodes": []});

        let mut own = base(14);
        own["reviewRequests"] = json!({"totalCount": 0, "nodes": []});

        let mut assigned_other = base(15);
        assigned_other["author"] = json!({"login": "eve"});
        assigned_other["reviewRequests"] = json!({
            "totalCount": 1,
            "nodes": [{"requestedReviewer": {"__typename": "User", "login": "other"}}]
        });

        let mut team_requested = base(16);
        team_requested["author"] = json!({"login": "frank"});
        team_requested["reviewRequests"] = json!({
            "totalCount": 1,
            "nodes": [{"requestedReviewer": {"__typename": "Team", "slug": "platform"}}]
        });

        let body = json!({
            "data": {
                "requested": {"pageInfo": {"hasNextPage": false}, "nodes": [requested]},
                "available": {"pageInfo": {"hasNextPage": false}, "nodes": [
                    duplicate, available, completed, draft, own, assigned_other, team_requested
                ]},
                "rateLimit": null
            }
        });
        let fetched = fetch_board(
            &FakeTransport(body),
            Mode::Review,
            "acme/widgets",
            "me",
            &cfg(),
        )
        .unwrap();

        assert_eq!(
            fetched.rows.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![10, 11, 12, 13]
        );
        assert_eq!(fetched.rows[0].category, Category::Todo);
        assert_eq!(
            fetched.rows[0].queue_provenance,
            Some(QueueProvenance::Requested)
        );
        assert_eq!(fetched.rows[0].requested, vec!["me"]);
        assert_eq!(fetched.rows[0].reviews.len(), 1);
        assert_eq!(fetched.rows[0].reviews[0].login.as_deref(), Some("bob"));
        assert_eq!(fetched.rows[0].reviews[0].state, "COMMENTED");
        assert_eq!(fetched.rows[0].labels, vec!["bug", "backend"]);
        assert!(fetched.rows[0].bug);
        assert_eq!(fetched.rows[0].stack.as_ref().unwrap().position, Some(2));
        assert_eq!(fetched.rows[1].category, Category::Available);
        assert_eq!(fetched.rows[1].labels, vec!["frontend"]);
        assert!(!fetched.rows[1].bug);
        assert_eq!(fetched.rows[1].stack.as_ref().unwrap().number, 70);
        assert_eq!(fetched.rows[1].stack.as_ref().unwrap().position, Some(3));
        assert_eq!(fetched.rows[2].category, Category::Done);
        assert_eq!(fetched.rows[3].category, Category::Draft);
        assert!(!fetched.truncated);
    }

    #[test]
    fn expanded_review_queue_surfaces_alias_truncation() {
        let body = json!({"data": {
            "requested": {"pageInfo": {"hasNextPage": true}, "nodes": []},
            "available": {"pageInfo": {"hasNextPage": false}, "nodes": []},
            "rateLimit": null
        }});
        let fetched = fetch_board(
            &FakeTransport(body),
            Mode::Review,
            "acme/widgets",
            "me",
            &cfg(),
        )
        .unwrap();
        assert!(fetched.truncated);
    }

    #[test]
    fn global_review_keeps_available_candidates_involvement_scoped() {
        let fetched = fetch_board_scoped(
            &GlobalReviewTransport,
            Mode::Review,
            &BoardScope::AllRepositories,
            "me",
            &cfg(),
        )
        .unwrap();
        assert!(fetched.rows.is_empty());
        assert!(!fetched.truncated);
    }

    #[test]
    fn paginated_review_deduplicates_with_requested_priority_and_skips_exhausted_alias() {
        let mut available_duplicate = base(20);
        available_duplicate["author"] = json!({"login": "alice"});
        available_duplicate["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
        let first = json!({"data": {
            "requested": {"pageInfo": {"hasNextPage": false, "endCursor": "r1"}, "nodes": []},
            "available": {"pageInfo": {"hasNextPage": true, "endCursor": "a1"}, "nodes": [available_duplicate]},
            "rateLimit": null
        }});
        let mut requested_duplicate = base(20);
        requested_duplicate["author"] = json!({"login": "alice"});
        requested_duplicate["reviewRequests"] = json!({"totalCount": 1, "nodes": []});
        // The only live alias is available; a requested-looking result there is
        // filtered, proving an exhausted requested alias was not refetched.
        let second = json!({"data": {
            "available": {"pageInfo": {"hasNextPage": false, "endCursor": "a2"}, "nodes": [requested_duplicate]},
            "rateLimit": null
        }});
        let transport = SequenceTransport::new(vec![Ok(first), Ok(second)]);
        let initial = fetch_board(&transport, Mode::Review, "acme/widgets", "me", &cfg()).unwrap();
        let fetched = fetch_more_board(
            &transport,
            Mode::Review,
            "acme/widgets",
            "me",
            &cfg(),
            &initial,
        )
        .unwrap();
        assert_eq!(fetched.rows.len(), 1);
        assert_eq!(
            fetched.rows[0].queue_provenance,
            Some(QueueProvenance::Available)
        );
        assert!(!fetched.pagination.can_load_more(Mode::Review));
    }

    #[test]
    fn requested_page_replaces_an_available_duplicate() {
        let mut broad = base(30);
        broad["author"] = json!({"login": "alice"});
        broad["reviewRequests"] = json!({"totalCount": 0, "nodes": []});
        let first = json!({"data": {
            "requested": {"pageInfo": {"hasNextPage": true, "endCursor": "r1"}, "nodes": []},
            "available": {"pageInfo": {"hasNextPage": false, "endCursor": "a1"}, "nodes": [broad]},
            "rateLimit": null
        }});
        let mut requested = base(30);
        requested["author"] = json!({"login": "alice"});
        requested["reviewRequests"] = json!({"totalCount": 1, "nodes": []});
        let second = json!({"data": {
            "requested": {"pageInfo": {"hasNextPage": false, "endCursor": "r2"}, "nodes": [requested]},
            "rateLimit": null
        }});
        let transport = SequenceTransport::new(vec![Ok(first), Ok(second)]);
        let initial = fetch_board(&transport, Mode::Review, "acme/widgets", "me", &cfg()).unwrap();
        let fetched = fetch_more_board(
            &transport,
            Mode::Review,
            "acme/widgets",
            "me",
            &cfg(),
            &initial,
        )
        .unwrap();
        assert_eq!(fetched.rows.len(), 1);
        assert_eq!(
            fetched.rows[0].queue_provenance,
            Some(QueueProvenance::Requested)
        );
    }

    #[test]
    fn authored_pagination_advances_and_missing_cursor_is_terminal() {
        let first = json!({"data": {
            "search": {"pageInfo": {"hasNextPage": true, "endCursor": "p1"}, "nodes": [base(2)]},
            "rateLimit": null
        }});
        let second = json!({"data": {
            "search": {"pageInfo": {"hasNextPage": true, "endCursor": null}, "nodes": [base(1)]},
            "rateLimit": null
        }});
        let transport = SequenceTransport::new(vec![Ok(first), Ok(second)]);
        let initial =
            fetch_board(&transport, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap();
        let fetched = fetch_more_board(
            &transport,
            Mode::Authored,
            "acme/widgets",
            "me",
            &cfg(),
            &initial,
        )
        .unwrap();
        assert_eq!(
            fetched.rows.iter().map(|r| r.number).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(!fetched.pagination.can_load_more(Mode::Authored));
    }

    #[test]
    fn pagination_stops_at_five_pages_and_errors_do_not_mutate_input() {
        let page = |cursor: &str| {
            json!({"data": {
                "search": {"pageInfo": {"hasNextPage": true, "endCursor": cursor}, "nodes": []},
                "rateLimit": null
            }})
        };
        let transport = SequenceTransport::new(vec![
            Ok(page("p1")),
            Ok(page("p2")),
            Ok(page("p3")),
            Ok(page("p4")),
            Ok(page("p5")),
        ]);
        let mut fetched =
            fetch_board(&transport, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap();
        for _ in 0..4 {
            fetched = fetch_more_board(
                &transport,
                Mode::Authored,
                "acme/widgets",
                "me",
                &cfg(),
                &fetched,
            )
            .unwrap();
        }
        assert!(!fetched.pagination.can_load_more(Mode::Authored));
        assert!(fetched.pagination.page_limit_reached(Mode::Authored));

        let error_seed = SequenceTransport::new(vec![Ok(page("e1"))]);
        let before_error =
            fetch_board(&error_seed, Mode::Authored, "acme/widgets", "me", &cfg()).unwrap();
        let error_transport = SequenceTransport::new(vec![Err(GhError::Network("offline".into()))]);
        let before = before_error.rows.len();
        assert!(fetch_more_board(
            &error_transport,
            Mode::Authored,
            "acme/widgets",
            "me",
            &cfg(),
            &before_error
        )
        .is_err());
        assert_eq!(before_error.rows.len(), before);
    }

    #[test]
    fn stack_metadata_is_native_and_labels_do_not_infer_it() {
        let mut stacked = base(40);
        stacked["stack"] = json!({"number": 7, "size": 3, "baseRefName": "main"});
        stacked["stackEntry"] = json!({"position": 2});
        let row = derive_one(stacked, Mode::Authored);
        assert_eq!(
            row.stack,
            Some(StackInfo {
                number: 7,
                size: 3,
                base_ref_name: "main".into(),
                position: Some(2),
            })
        );

        let mut no_position = base(42);
        no_position["stack"] = json!({"number": 8, "size": 2, "baseRefName": "develop"});
        assert_eq!(
            derive_one(no_position, Mode::Authored)
                .stack
                .unwrap()
                .position,
            None
        );

        let mut label_only = base(41);
        label_only["labels"]["nodes"] = json!([{"name": "stack"}]);
        assert!(derive_one(label_only, Mode::Authored).stack.is_none());
    }

    #[test]
    fn available_review_keeps_health_warning() {
        let mut pr: RawPr = serde_json::from_value(base(99)).unwrap();
        pr.mergeable = Some("CONFLICTING".into());
        let row = derive_review_row(
            &pr,
            "acme/widgets",
            "reviewer",
            &cfg(),
            QueueProvenance::Available,
        );
        assert_eq!(row.category, Category::Available);
        assert_eq!(row.note, "⚠️ has conflicts");
    }

    #[test]
    fn semantic_observation_is_stable_across_scope_and_queue_derivations() {
        let mut value = base(120);
        value["id"] = json!("PR_shared");
        value["url"] = json!("https://github.com/acme/widgets/pull/120");
        value["repository"] = json!({"nameWithOwner": "acme/widgets"});
        value["author"] = json!({"login": "alice"});
        value["updatedAt"] = json!("2026-09-11T10:00:00Z");
        value["headRefOid"] = json!("head-2");
        value["reviewRequests"] = json!({
            "totalCount": 1,
            "nodes": [{"requestedReviewer": {"__typename": "User", "login": "me"}}]
        });
        value["reviews"]["nodes"] = json!([
            {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z"},
            {"author": {"login": "bob"}, "state": "COMMENTED", "submittedAt": "2026-09-09T10:00:00Z"}
        ]);
        value["latestReview"] = json!({"nodes": [{
            "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z", "commit": {"oid": "head-1"}
        }]});
        let raw: RawPr = serde_json::from_value(value).unwrap();
        let involving = derive_involving_row(&raw, "", "me", &cfg());
        let review = derive_review_row(
            &raw,
            "acme/widgets",
            "me",
            &cfg(),
            QueueProvenance::Requested,
        );
        assert_ne!(involving.category, review.category);
        assert_eq!(
            crate::attention::Observation::from_row(&involving),
            crate::attention::Observation::from_row(&review)
        );
    }

    #[test]
    fn review_note_reports_new_head_without_fabricating_commit_count() {
        let mut value = base(121);
        value["author"] = json!({"login": "alice"});
        value["headRefOid"] = json!("current-head");
        value["latestReview"] = json!({"nodes": [{
            "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z", "commit": {"oid": "reviewed-head"}
        }]});
        value["reviews"]["nodes"] = json!([
            {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-09-10T10:00:00Z"}
        ]);
        let row = derive_one(value, Mode::Review);
        assert_eq!(row.note, "new commits since your review · ✅ you approved");
        assert!(!row.note.chars().any(|character| character.is_ascii_digit()));
    }

    #[test]
    fn a_missing_repository_is_an_error_and_a_missing_tracked_pr_is_inaccessible() {
        struct Scoped;
        impl GithubTransport for Scoped {
            fn graphql(
                &self,
                _query: &str,
                _variables: &[(&str, &str)],
            ) -> Result<serde_json::Value, GhError> {
                unreachable!("tracked ids are always requested here")
            }
            fn graphql_with_ids(
                &self,
                query: &str,
                variables: &[(&str, &str)],
                ids: &[String],
            ) -> Result<serde_json::Value, GhError> {
                assert!(query.contains("scopeRepository: repository("));
                assert_eq!(ids, ["PR_gone"]);
                let found = variables.contains(&("scopeName", "widgets"));
                assert!(variables.contains(&("scopeOwner", "acme")));
                let mut errors = vec![json!({"type": "NOT_FOUND", "path": ["tracked", 0],
                    "message": "Could not resolve to a node with the global id of 'PR_gone'."})];
                if !found {
                    errors.push(json!({"type": "NOT_FOUND", "path": ["scopeRepository"],
                        "message": "Could not resolve to a Repository with the name 'acme/nope'."}));
                }
                Ok(json!({
                    "data": {
                        "scopeRepository": if found { json!({"nameWithOwner": "acme/widgets"}) } else { json!(null) },
                        "search": {"pageInfo": {"hasNextPage": false}, "nodes": []},
                        "tracked": [null],
                        "rateLimit": null
                    },
                    "errors": errors
                }))
            }
        }
        let tracked = ["PR_gone".to_owned()];
        let scope = BoardScope::Repository("acme/widgets".into());
        let fetched = fetch_board_scoped_with_tracked(
            &Scoped,
            Mode::Authored,
            &scope,
            "me",
            &cfg(),
            &tracked,
        )
        .unwrap();
        assert_eq!(fetched.tracked.len(), 1);
        assert_eq!(fetched.tracked[0].status, TrackedPrStatus::Inaccessible);

        let scope = BoardScope::Repository("acme/nope".into());
        let error =
            fetch_board_scoped_with_tracked(&Scoped, Mode::Review, &scope, "me", &cfg(), &tracked)
                .unwrap_err();
        assert_eq!(error, GhError::RepositoryNotFound("acme/nope".into()));
        assert_eq!(
            error.to_string(),
            "repository acme/nope not found, or the gh account can't access it"
        );
    }

    #[test]
    fn repository_identity_survives_alias_dedup_and_page_merges() {
        let make = |repo: &str, id: &str| {
            let mut v = base(42);
            v["id"] = json!(id);
            v["repository"] = json!({"nameWithOwner": repo});
            v["url"] = json!(format!("https://github.com/{repo}/pull/42"));
            v["author"] = json!({"login": "alice"});
            v["reviewRequests"] = json!({"totalCount":0,"nodes":[]});
            v
        };
        let a = make("acme/one", "PR_a");
        let b = make("acme/two", "PR_b");
        let body = json!({"data": {
            "requested": {"nodes": [a.clone()]},
            "available": {"nodes": [a.clone(), b.clone()]}
        }});
        let fetched = fetch_board(
            &FakeTransport(body),
            Mode::Review,
            "ignored/repo",
            "me",
            &cfg(),
        )
        .unwrap();
        assert_eq!(fetched.rows.len(), 2);
        assert_eq!(fetched.rows[0].repo, "acme/one");
        assert_eq!(fetched.rows[1].url, "https://github.com/acme/two/pull/42");
        let a: RawPr = serde_json::from_value(a).unwrap();
        let mut b: RawPr = serde_json::from_value(b).unwrap();
        let mut authored = derive_rows(
            std::slice::from_ref(&a),
            Mode::Authored,
            "ignored/repo",
            "me",
            &cfg(),
        );
        merge_authored(&mut authored, &[b.clone()], "ignored/repo", "me", &cfg());
        assert_eq!(authored.len(), 2);
        b.title = "changed title".into();
        merge_authored(&mut authored, &[b.clone()], "ignored/repo", "me", &cfg());
        assert_eq!(authored.len(), 2);
        assert_eq!(
            authored.iter().find(|r| r.id == "PR_b").unwrap().title,
            "changed title"
        );
        let mut review = fetched.rows;
        merge_review(&mut review, &[b], &[a], "ignored/repo", "me", &cfg());
        assert_eq!(review.len(), 2);
        assert!(review
            .iter()
            .all(|r| r.queue_provenance == Some(QueueProvenance::Requested)));
    }

    #[test]
    fn observation_fields_use_canonical_data_and_latest_review_alias() {
        let mut v = base(42);
        v["id"] = json!("PR_42");
        v["repository"] = json!({"nameWithOwner": "acme/actual"});
        v["updatedAt"] = json!("2026-09-11T10:00:00Z");
        v["headRefOid"] = json!("new-head");
        v["latestReview"] = json!({"nodes": [{"state":"APPROVED", "submittedAt":"2026-09-10T10:00:00Z", "commit":{"oid":"reviewed-head"}}]});
        let row = derive_one(v.clone(), Mode::Review);
        assert_eq!(row.repo, "acme/actual");
        assert_eq!(row.url, "https://github.com/acme/actual/pull/42");
        assert_eq!(row.updated_at.as_deref(), Some("2026-09-11T10:00:00Z"));
        assert_eq!(row.head_oid.as_deref(), Some("new-head"));
        assert_eq!(row.reviewed_oid.as_deref(), Some("reviewed-head"));
        assert_eq!(row.reviewed_at.as_deref(), Some("2026-09-10T10:00:00Z"));
        v["latestReview"]["nodes"][0]["commit"] = json!(null);
        assert_eq!(derive_one(v.clone(), Mode::Review).reviewed_oid, None);
        v["latestReview"]["nodes"][0]["commit"] = json!({"oid":"dismissed-head"});
        v["latestReview"]["nodes"][0]["state"] = json!("DISMISSED");
        assert_eq!(derive_one(v, Mode::Review).reviewed_oid, None);
    }

    #[test]
    fn authored_only_narrows_the_all_repositories_search_to_your_prs() {
        struct Searches(Mutex<Vec<String>>);
        impl GithubTransport for Searches {
            fn graphql(
                &self,
                _query: &str,
                variables: &[(&str, &str)],
            ) -> Result<serde_json::Value, GhError> {
                let q = variables.iter().find(|(key, _)| *key == "q").unwrap().1;
                self.0.lock().unwrap().push(q.to_owned());
                Ok(json!({"data": {
                    "search": {"pageInfo": {"hasNextPage": false}, "nodes": []},
                    "rateLimit": null
                }}))
            }
        }
        let searches = Searches(Mutex::new(Vec::new()));
        let all = BoardScope::AllRepositories;
        let mut config = cfg();
        fetch_board_scoped(&searches, Mode::Authored, &all, "me", &config).unwrap();
        config.authored_only = true;
        fetch_board_scoped(&searches, Mode::Authored, &all, "me", &config).unwrap();
        let repo = BoardScope::Repository("acme/widgets".into());
        fetch_board_scoped(&searches, Mode::Authored, &repo, "me", &config).unwrap();
        assert_eq!(
            *searches.0.lock().unwrap(),
            [
                "is:pr is:open involves:me",
                "is:pr is:open author:me",
                "repo:acme/widgets is:pr is:open author:me",
            ]
        );
    }

    /// Records every search string and answers with `pages` in turn.
    struct AllOpenSearches {
        seen: Mutex<Vec<(String, Option<String>)>>,
        requested: Mutex<Vec<String>>,
        pages: Mutex<VecDeque<serde_json::Value>>,
    }

    impl AllOpenSearches {
        fn new(pages: Vec<serde_json::Value>) -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                requested: Mutex::new(Vec::new()),
                pages: Mutex::new(pages.into()),
            }
        }

        fn answer(&self, variables: &[(&str, &str)]) -> serde_json::Value {
            let get = |name: &str| {
                variables
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            };
            if let Some(requested) = get("requested") {
                self.requested.lock().unwrap().push(requested);
            }
            self.seen
                .lock()
                .unwrap()
                .push((get("q").unwrap(), get("after")));
            self.pages.lock().unwrap().pop_front().unwrap()
        }
    }

    impl GithubTransport for AllOpenSearches {
        fn graphql(
            &self,
            query: &str,
            variables: &[(&str, &str)],
        ) -> Result<serde_json::Value, GhError> {
            assert!(
                query.contains("issueCount"),
                "the count comes with the page"
            );
            Ok(self.answer(variables))
        }
    }

    fn all_open_page(
        nodes: Vec<serde_json::Value>,
        count: u64,
        next: Option<&str>,
    ) -> serde_json::Value {
        json!({"data": {
            "scopeRepository": {"nameWithOwner": "acme/widgets"},
            "search": {
                "issueCount": count,
                "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
                "nodes": nodes
            },
            "rateLimit": null
        }})
    }

    fn someone_elses(number: u64, author: &str, updated: &str) -> serde_json::Value {
        let mut v = base(number);
        v["id"] = json!(format!("PR_{number}"));
        v["author"] = json!({"login": author});
        v["updatedAt"] = json!(updated);
        v
    }

    #[test]
    fn all_open_searches_one_repository_by_last_update_and_counts_what_is_not_loaded() {
        let transport = AllOpenSearches::new(vec![
            all_open_page(
                vec![someone_elses(7, "alice", "2026-09-20T10:00:00Z")],
                412,
                Some("c1"),
            ),
            all_open_page(
                vec![someone_elses(5, "bob", "2026-09-19T10:00:00Z")],
                412,
                None,
            ),
        ]);
        let first = fetch_all_open(
            &transport,
            "acme/widgets",
            "me",
            &cfg(),
            &RemoteFilter::default(),
            &[],
        )
        .unwrap();
        assert_eq!(first.total, Some(412));
        assert!(first.truncated);
        assert!(first.pagination.can_load_more(Mode::AllOpen));

        let more = fetch_more_board(
            &transport,
            Mode::AllOpen,
            "acme/widgets",
            "me",
            &cfg(),
            &first,
        )
        .unwrap();
        assert_eq!(
            more.rows.iter().map(|row| row.number).collect::<Vec<_>>(),
            [7, 5],
            "a later page lands below the rows already shown"
        );
        assert!(!more.truncated);
        assert!(!more.pagination.can_load_more(Mode::AllOpen));
        let search = "repo:acme/widgets is:pr is:open sort:updated-desc";
        assert_eq!(
            *transport.seen.lock().unwrap(),
            [
                (search.to_owned(), None),
                (search.to_owned(), Some("c1".to_owned())),
            ]
        );
    }

    #[test]
    fn all_open_sends_label_and_author_chips_to_github_and_pages_the_same_search() {
        use crate::search::{FilterChip, Qualifier};
        let filter = RemoteFilter::from_chips(&[
            FilterChip::new(Qualifier::Label, "Needs Review"),
            FilterChip::new(Qualifier::Author, "Alice"),
        ]);
        let transport = AllOpenSearches::new(vec![
            all_open_page(Vec::new(), 90, Some("c1")),
            all_open_page(Vec::new(), 90, None),
        ]);
        let first = fetch_all_open(&transport, "acme/widgets", "me", &cfg(), &filter, &[]).unwrap();
        // A filter change is a new search; the cursor it hands out belongs to
        // this one, so Load more repeats it word for word.
        fetch_more_board(
            &transport,
            Mode::AllOpen,
            "acme/widgets",
            "me",
            &cfg(),
            &first,
        )
        .unwrap();
        let search = "repo:acme/widgets is:pr is:open sort:updated-desc \
                      label:\"needs review\" author:alice author:app/alice";
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[0].0, search);
        assert_eq!(seen[1], (search.to_owned(), Some("c1".to_owned())));
        assert_eq!(
            *transport.requested.lock().unwrap(),
            [search_string(Mode::Review, "acme/widgets", "me")],
            "page one asks the review queue's own question, unfiltered; Load more does not"
        );
    }

    #[test]
    fn a_later_page_reads_review_requests_from_page_one() {
        let mut first = all_open_page(
            vec![someone_elses(7, "alice", "2026-09-20T10:00:00Z")],
            2,
            Some("c1"),
        );
        first["data"]["requested"] = json!({"nodes": [{"id": "PR_5"}]});
        let transport = AllOpenSearches::new(vec![
            first,
            all_open_page(
                vec![someone_elses(5, "bob", "2026-09-19T10:00:00Z")],
                2,
                None,
            ),
        ]);
        let page = fetch_all_open(
            &transport,
            "acme/widgets",
            "me",
            &cfg(),
            &RemoteFilter::default(),
            &[],
        )
        .unwrap();
        let more = fetch_more_board(
            &transport,
            Mode::AllOpen,
            "acme/widgets",
            "me",
            &cfg(),
            &page,
        )
        .unwrap();
        let bobs = more.rows.iter().find(|row| row.number == 5).unwrap();
        assert_eq!(bobs.category, Category::Todo);
    }

    #[test]
    fn all_open_asks_for_a_repository_rather_than_searching_everything() {
        let error = fetch_board_scoped(
            &SequenceTransport::new(Vec::new()),
            Mode::AllOpen,
            &BoardScope::AllRepositories,
            "me",
            &cfg(),
        )
        .unwrap_err();
        assert_eq!(error, GhError::NeedsRepository);
        assert_eq!(
            error.to_string(),
            "Pick a repository to see all of its open PRs."
        );
    }

    #[test]
    fn all_open_rows_read_as_yours_as_asked_of_you_or_in_the_authors_name() {
        let mut mine = base(1);
        mine["id"] = json!("mine");
        mine["updatedAt"] = json!("2026-09-18T10:00:00Z");
        let mut asks_me = someone_elses(2, "alice", "2026-09-19T10:00:00Z");
        asks_me["reviewRequests"] = json!({"totalCount": 1, "nodes": [
            {"requestedReviewer": {"__typename": "User", "login": "Me"}}
        ]});
        let mut asks_a_team = someone_elses(3, "bob", "2026-09-20T10:00:00Z");
        asks_a_team["reviewRequests"] = json!({"totalCount": 1, "nodes": [
            {"requestedReviewer": {"__typename": "Team", "slug": "platform"}}
        ]});
        asks_a_team["mergeable"] = json!("CONFLICTING");
        let mut draft = someone_elses(4, "carol", "2026-09-21T10:00:00Z");
        draft["isDraft"] = json!(true);
        draft["reviewRequests"] = asks_me["reviewRequests"].clone();

        let prs = [pr(mine), pr(asks_me), pr(asks_a_team), pr(draft)];
        let rows = derive_rows(&prs, Mode::AllOpen, "acme/widgets", "me", &cfg());
        assert_eq!(
            rows.iter().map(|row| row.number).collect::<Vec<_>>(),
            [4, 3, 2, 1],
            "most recently updated first, as GitHub sorted them"
        );
        let by_number = |n: u64| rows.iter().find(|row| row.number == n).unwrap();
        assert_eq!(by_number(1).note, "⚠️ no reviewers — assign alice + bob");
        assert_eq!(by_number(2).category, Category::Todo);
        assert_eq!(by_number(2).note, "🔵 needs your review");
        let team = by_number(3);
        assert_ne!(
            team.category,
            Category::Todo,
            "unless the review queue's search says a team of yours was asked"
        );
        assert_eq!(
            team.note, "merge conflict",
            "the Author column already says whose it is"
        );
        assert!(
            team.blockers.is_empty(),
            "someone else's conflict is not your blocker"
        );
        assert_eq!(by_number(4).category, Category::Draft);
        let need_you = |rows: &[BoardRow]| {
            rows.iter()
                .filter(|row| crate::status::row_needs_you(Mode::AllOpen, row))
                .count()
        };
        assert_eq!(
            need_you(&rows),
            2,
            "your own PR with no reviewers and the review asked of you; bob's conflict is his"
        );

        // The review queue's `review-requested:` search counts your teams;
        // what it returns is "Requested from you" in All open too.
        let rows = derive_all_open_rows(&prs, "acme/widgets", "me", &cfg(), &["PR_3".into()]);
        let team = rows.iter().find(|row| row.number == 3).unwrap();
        assert_eq!(team.category, Category::Todo);
        assert_eq!(team.queue_provenance, Some(QueueProvenance::Requested));
        assert_eq!(
            need_you(&rows),
            3,
            "your own PR, the review asked of you by name, and your team's"
        );
    }

    /// Your own review is not in `review_state`, and GitHub drops your
    /// request once you review, so a teammate's PR that only you reviewed
    /// reads as nobody asked and nobody reviewed. It stays under Awaiting
    /// review rather than Available to review ("no review yet").
    #[test]
    fn a_teammates_pr_you_reviewed_is_not_available_to_review() {
        let unasked = someone_elses(1, "alice", "2026-09-19T10:00:00Z");
        let mut reviewed = someone_elses(2, "alice", "2026-09-20T10:00:00Z");
        reviewed["reviews"]["nodes"] = json!([
            {"author": {"login": "me"}, "state": "APPROVED", "submittedAt": "2026-09-20T09:00:00Z"}
        ]);
        let prs = [pr(unasked), pr(reviewed)];
        for (mode, rows) in [
            (
                Mode::AllOpen,
                derive_all_open_rows(&prs, "acme/widgets", "me", &cfg(), &[]),
            ),
            (
                Mode::Authored,
                derive_involving_rows(&prs, "acme/widgets", "me", &cfg()),
            ),
        ] {
            let by_number = |n: u64| rows.iter().find(|row| row.number == n).unwrap();
            let reviewed = by_number(2);
            assert_eq!(
                (reviewed.review_state, reviewed.my_review.as_deref()),
                (ReviewState::None, Some("APPROVED")),
                "{mode:?}"
            );
            assert!(
                !crate::layout::is_available_section(mode, reviewed),
                "{mode:?}"
            );
            assert!(
                crate::layout::is_available_section(mode, by_number(1)),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn global_involving_rows_keep_own_notes_and_describe_other_authors() {
        let mut mine = base(1);
        mine["id"] = json!("mine");
        mine["repository"] = json!({"nameWithOwner":"acme/one"});
        let mut theirs = base(2);
        theirs["id"] = json!("theirs");
        theirs["repository"] = json!({"nameWithOwner":"acme/two"});
        theirs["author"] = json!({"login":"alice"});
        theirs["mergeable"] = json!("CONFLICTING");
        theirs["commits"]["nodes"] = json!([{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]);

        let rows =
            derive_involving_rows(&[pr(mine), pr(theirs)], "", "me", &BoardConfig::default());
        let mine = rows.iter().find(|row| row.id == "mine").unwrap();
        assert_eq!(mine.note, "⚠️ no reviewers");
        let theirs = rows.iter().find(|row| row.id == "theirs").unwrap();
        assert_eq!(theirs.repo, "acme/two");
        assert_eq!(theirs.note, "alice's PR · merge conflict · CI failing");
        assert!(!theirs.note.contains("rebase"));
        assert!(theirs.blockers.is_empty());
    }

    #[test]
    fn unknown_mergeability_keeps_the_last_known_conflict_and_rederives() {
        let mut known = base(1);
        known["id"] = json!("mine");
        known["mergeable"] = json!("CONFLICTING");
        let conflicted = derive_one(known.clone(), Mode::Authored);
        assert!(conflicted.conflict && !conflicted.mergeable_unknown);

        let mut recomputing = known.clone();
        recomputing["mergeable"] = json!("UNKNOWN");
        let derive = |v: &serde_json::Value| {
            derive_rows(
                &[pr(v.clone())],
                Mode::Authored,
                "acme/widgets",
                "me",
                &cfg(),
            )
        };
        let mut rows = derive(&recomputing);
        assert!(rows[0].mergeable_unknown && !rows[0].conflict);
        let last = |id: &str| (id == "mine").then_some(true);
        assert_eq!(
            carry_forward_conflicts(&mut rows, last, Mode::Authored, "me", &cfg()),
            1
        );
        assert!(rows[0].conflict);
        assert_eq!(rows[0].category, conflicted.category);
        assert_eq!(rows[0].blockers, conflicted.blockers);
        assert_eq!(rows[0].note, conflicted.note);

        // Last known mergeable, no memory, or a value GitHub did report: untouched.
        let mut rows = derive(&recomputing);
        assert_eq!(
            carry_forward_conflicts(&mut rows, |_| Some(false), Mode::Authored, "me", &cfg()),
            0
        );
        assert_eq!(
            carry_forward_conflicts(&mut rows, |_| None, Mode::Authored, "me", &cfg()),
            0
        );
        let mut clean = known.clone();
        clean["mergeable"] = json!("MERGEABLE");
        let mut rows = derive(&clean);
        assert_eq!(
            carry_forward_conflicts(&mut rows, last, Mode::Authored, "me", &cfg()),
            0
        );
        assert!(!rows[0].conflict);
        let mut absent = known.clone();
        absent["mergeable"] = serde_json::Value::Null;
        assert!(derive(&absent)[0].mergeable_unknown);

        // Someone else's PR in the involving view keeps its status Note.
        let mut theirs = known.clone();
        theirs["author"] = json!({"login": "alice"});
        let expected = derive_involving_rows(&[pr(theirs.clone())], "acme/widgets", "me", &cfg());
        theirs["mergeable"] = json!("UNKNOWN");
        let mut rows = derive_involving_rows(&[pr(theirs)], "acme/widgets", "me", &cfg());
        carry_forward_conflicts(&mut rows, last, Mode::Authored, "me", &cfg());
        assert_eq!(rows[0].note, expected[0].note);
        assert_eq!(rows[0].category, expected[0].category);
        assert!(rows[0].blockers.is_empty());

        // Review queue Notes mention the conflict again.
        let mut review = known.clone();
        review["author"] = json!({"login": "alice"});
        let expected = derive_one(review.clone(), Mode::Review);
        review["mergeable"] = json!("UNKNOWN");
        let mut rows = derive_rows(&[pr(review)], Mode::Review, "acme/widgets", "me", &cfg());
        carry_forward_conflicts(&mut rows, last, Mode::Review, "me", &cfg());
        assert_eq!(rows[0].note, expected.note);
    }

    #[test]
    fn settings_comparison_tracks_reviewers_and_issue_rule_content() {
        let mut first = BoardConfig::default();
        let mut second = first.clone();
        assert_eq!(first, second);
        second.default_reviewers.push("alex".into());
        assert_ne!(first, second);
        first = second.clone();
        first.issue_link =
            Some(IssueLinkRule::new("DEMO-[0-9]+", "https://example.com/{id}").unwrap());
        second.issue_link =
            Some(IssueLinkRule::new("DEMO-[0-9]+", "https://example.com/{id}").unwrap());
        assert_eq!(first, second);
        second.issue_link =
            Some(IssueLinkRule::new("TASK-[0-9]+", "https://example.com/{id}").unwrap());
        assert_ne!(first, second);
        second.issue_link =
            Some(IssueLinkRule::new("DEMO-[0-9]+", "https://other.example/{id}").unwrap());
        assert_ne!(first, second);
    }
}
