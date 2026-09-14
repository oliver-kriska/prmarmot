//! The board view-model: `RawPr` → `BoardRow` (category, CI, review state,
//! unresolved count, Note). Ported from the shell prototype's `jq` programs
//! (categorization) and `SKILL.md` (Note composition); the golden tests in
//! `tests/parity.rs` pin this module to the prototype's actual output.

use regex::Regex;

use std::collections::{HashMap, HashSet};

use crate::github::query::{
    available_search_string, global_available_search_string, global_search_string, page_info,
    parse_alias_response, parse_review_response, parse_search_response, with_tracked_nodes, RawPr,
    PR_SEARCH_PAGE_QUERY, PR_SEARCH_QUERY, REVIEW_AVAILABLE_PAGE_QUERY, REVIEW_BOTH_PAGE_QUERY,
    REVIEW_REQUESTED_PAGE_QUERY, REVIEW_SEARCH_QUERY,
};
use crate::github::rate_limit::RateLimitInfo;
use crate::github::{GhError, GithubTransport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// PRs the user authored — the outgoing queue.
    Authored,
    /// PRs awaiting the user's review — the incoming queue.
    Review,
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
}

impl Ci {
    pub fn as_str(&self) -> &'static str {
        match self {
            Ci::Pass => "pass",
            Ci::Fail => "fail",
            Ci::None => "none",
            Ci::Running => "running",
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
    pub fn new(pattern: &str, url_template: &str) -> Result<Self, regex::Error> {
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
    /// Suggested reviewers for the "no reviewers — assign …" note.
    pub default_reviewers: Vec<String>,
    pub issue_link: Option<IssueLinkRule>,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            bots: vec!["chatgpt-codex-connector".into(), "github-actions".into()],
            default_reviewers: Vec::new(),
            issue_link: None,
        }
    }
}

/// One row of the dashboard. Fields not applicable to the row's mode are
/// empty/None (`review_*`/`requested`/`reviews` are authored-mode; `author`/
/// `my_review` are review-mode).
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
    pub review_decision: Option<String>,
    pub review_state: ReviewState,
    pub requested: Vec<String>,
    pub reviews: Vec<ReviewSummary>,
    pub my_review: Option<String>,
    pub unresolved: usize,
    /// Structured blockers behind the action `note`, most-blocking-first
    /// (authored mode only; empty for await/review rows). The UI reorders and
    /// colors these; the `note` string is generated from exactly this list.
    pub blockers: Vec<Blocker>,
    pub created_at: String,
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
    authored: AliasCursor,
    requested: AliasCursor,
    available: AliasCursor,
}

impl BoardPagination {
    pub fn can_load_more(&self, mode: Mode) -> bool {
        match mode {
            Mode::Authored => self.authored.can_load(),
            Mode::Review => self.requested.can_load() || self.available.can_load(),
        }
    }

    pub fn page_limit_reached(&self, mode: Mode) -> bool {
        let limited = |c: &AliasCursor| c.has_next && c.pages >= MAX_PAGES_PER_ALIAS;
        match mode {
            Mode::Authored => limited(&self.authored),
            Mode::Review => limited(&self.requested) || limited(&self.available),
        }
    }

    fn truncated(&self, mode: Mode) -> bool {
        match mode {
            Mode::Authored => self.authored.has_next,
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
    let repo = scope.fallback_repo();
    match mode {
        Mode::Authored => {
            let search = scope_search_string(scope, mode, me);
            let query = if tracked_ids.is_empty() {
                PR_SEARCH_QUERY.to_owned()
            } else {
                with_tracked_nodes(PR_SEARCH_QUERY)?
            };
            let body =
                transport.graphql_with_ids(&query, &[("q", &search), ("who", me)], tracked_ids)?;
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
            })
        }
        Mode::Review => {
            let requested_search = scope_search_string(scope, mode, me);
            let available_search = scope_available_search_string(scope, me);
            let query = if tracked_ids.is_empty() {
                REVIEW_SEARCH_QUERY.to_owned()
            } else {
                with_tracked_nodes(REVIEW_SEARCH_QUERY)?
            };
            let body = transport.graphql_with_ids(
                &query,
                &[
                    ("requested", &requested_search),
                    ("available", &available_search),
                    ("who", me),
                ],
                tracked_ids,
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
                        row: Some(derive_row(&raw, Mode::Authored, repo, me, cfg)),
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
            let search = scope_search_string(scope, mode, me);
            let cursor = next.pagination.authored.end_cursor.clone().unwrap();
            let body = transport.graphql(
                PR_SEARCH_PAGE_QUERY,
                &[("q", &search), ("after", &cursor), ("who", me)],
            )?;
            let (prs, rate) = parse_search_response(&body)?;
            next.pagination.authored.update(page_info(&body, "search"));
            if scope.is_all() {
                merge_involving(&mut next.rows, &prs, repo, me, cfg);
            } else {
                merge_authored(&mut next.rows, &prs, repo, me, cfg);
            }
            next.rate = rate;
        }
        Mode::Review => {
            let requested = next.pagination.requested.can_load();
            let available = next.pagination.available.can_load();
            if !requested && !available {
                return Ok(next);
            }
            let requested_search = scope_search_string(scope, mode, me);
            let available_search = scope_available_search_string(scope, me);
            let mut requested_prs = Vec::new();
            let mut available_prs = Vec::new();
            if requested && available {
                let rc = next.pagination.requested.end_cursor.clone().unwrap();
                let ac = next.pagination.available.end_cursor.clone().unwrap();
                let body = transport.graphql(
                    REVIEW_BOTH_PAGE_QUERY,
                    &[
                        ("requested", &requested_search),
                        ("requestedAfter", &rc),
                        ("available", &available_search),
                        ("availableAfter", &ac),
                        ("who", me),
                    ],
                )?;
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
                let body = transport
                    .graphql(query, &[(alias, search), ("after", &cursor), ("who", me)])?;
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

fn scope_search_string(scope: &BoardScope, mode: Mode, me: &str) -> String {
    match scope {
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
        .map(|pr| derive_row(pr, mode, repo, me, cfg))
        .collect();
    match mode {
        // action → await → draft, newest first within each.
        Mode::Authored => rows.sort_by_key(|r| (r.category.rank(), std::cmp::Reverse(r.number))),
        // todo → done → draft, oldest first — clear the backlog.
        Mode::Review => rows.sort_by_key(|r| (r.category.rank(), r.number)),
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

fn derive_involving_row(pr: &RawPr, repo: &str, me: &str, cfg: &BoardConfig) -> BoardRow {
    if is_own_pr(pr, me) {
        return derive_row(pr, Mode::Authored, repo, me, cfg);
    }

    let mut row = derive_row(pr, Mode::Authored, repo, me, cfg);
    row.blockers.clear();
    let author = row.author.as_deref().unwrap_or("Unknown author").to_owned();
    if row.draft {
        row.category = Category::Draft;
        row.note = format!("draft by {author}");
        return row;
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
        facts.push(format!(
            "{} unresolved comment{}",
            row.unresolved,
            if row.unresolved == 1 { "" } else { "s" }
        ));
    }
    row.category = if facts.is_empty() {
        Category::Await
    } else {
        Category::Action
    };
    row.note = if facts.is_empty() {
        match row.review_state {
            ReviewState::Approved => format!("{author}'s PR · approved"),
            ReviewState::Commented => format!("{author}'s PR · review comments received"),
            ReviewState::Waiting => format!("{author}'s PR · awaiting review"),
            ReviewState::None | ReviewState::Changes => format!("{author}'s PR · open"),
        }
    } else {
        format!("{author}'s PR · {}", facts.join(" · "))
    };
    row
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
        review_decision: pr.review_decision.clone(),
        review_state: ReviewState::None,
        requested: Vec::new(),
        reviews: Vec::new(),
        my_review: None,
        unresolved,
        blockers: Vec::new(),
        created_at: pr.created_at.clone(),
        note: String::new(),
    };

    match mode {
        Mode::Authored => {
            row.requested = requested_reviewers(pr);
            row.reviews = latest_reviews_excluding(pr, me, &cfg.bots);
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
            row.category = if pr.is_draft {
                Category::Draft
            } else if ci == Ci::Fail
                || conflict
                || pr.review_decision.as_deref() == Some("CHANGES_REQUESTED")
                || unresolved > 0
                || row.review_state == ReviewState::None
            {
                Category::Action
            } else {
                Category::Await
            };
            // Structured blockers first (one source of truth), then the exact
            // legacy note generated from them.
            row.blockers = authored_blockers(&row, cfg);
            row.note = authored_note(&row);
        }
        Mode::Review => {
            let mine = my_latest_review(pr, me);
            row.category = if pr.is_draft {
                Category::Draft
            } else if matches!(
                mine.as_str(),
                "APPROVED" | "COMMENTED" | "CHANGES_REQUESTED"
            ) {
                Category::Done
            } else {
                Category::Todo
            };
            row.my_review = Some(mine);
            row.note = review_note(&row);
        }
    }
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
    // The compact review queue does not display these columns, but its details
    // panel still needs the metadata already present in the response.
    row.requested = requested_reviewers(pr);
    row.reviews = latest_reviews_excluding(pr, me, &cfg.bots);
    if provenance == QueueProvenance::Available
        && !matches!(row.category, Category::Done | Category::Draft)
    {
        row.category = Category::Available;
        row.note = review_note(&row);
    }
    row
}

fn derive_ci(pr: &RawPr) -> Ci {
    let state = pr
        .commits
        .nodes
        .first()
        .and_then(|c| c.commit.status_check_rollup.as_ref())
        .and_then(|r| r.state.as_deref())
        .unwrap_or("NONE");
    match state {
        "SUCCESS" => Ci::Pass,
        "FAILURE" | "ERROR" => Ci::Fail,
        "NONE" => Ci::None,
        _ => Ci::Running, // PENDING / EXPECTED
    }
}

fn requested_reviewers(pr: &RawPr) -> Vec<String> {
    pr.review_requests
        .nodes
        .iter()
        .filter_map(|n| n.requested_reviewer.as_ref())
        .filter_map(|r| r.login.clone().or_else(|| r.slug.clone()))
        .collect()
}

/// Latest review state per author, excluding the PR author and bots — the
/// prototype's `$rv`. Ordered by login (`group_by` sorts; null first).
fn latest_reviews_excluding(pr: &RawPr, me: &str, bots: &[String]) -> Vec<ReviewSummary> {
    let mut latest: Vec<(Option<String>, &str, Option<&str>)> = Vec::new(); // (login, state, submitted_at)
    for review in &pr.reviews.nodes {
        let login = review.author.as_ref().and_then(|a| a.login.clone());
        if let Some(l) = &login {
            if l == me || bots.iter().any(|b| b == l) {
                continue;
            }
        }
        let submitted = review.submitted_at.as_deref();
        match latest.iter_mut().find(|(l, _, _)| *l == login) {
            // Later-or-equal submittedAt wins, like jq's max_by.
            Some(entry) => {
                if submitted >= entry.2 {
                    entry.1 = &review.state;
                    entry.2 = submitted;
                }
            }
            None => latest.push((login, &review.state, submitted)),
        }
    }
    latest.sort_by(|a, b| a.0.cmp(&b.0));
    latest
        .into_iter()
        .map(|(login, state, submitted_at)| ReviewSummary {
            login,
            state: state.to_string(),
            submitted_at: submitted_at.map(str::to_owned),
        })
        .collect()
}

fn latest_review_evidence(pr: &RawPr) -> Option<&crate::github::query::LatestReview> {
    pr.latest_review
        .nodes
        .first()
        .filter(|review| review.state != "DISMISSED")
}

/// The user's own latest review state, or "NONE" — the prototype's `$mine`.
fn my_latest_review(pr: &RawPr, me: &str) -> String {
    pr.reviews
        .nodes
        .iter()
        .filter(|r| r.author.as_ref().and_then(|a| a.login.as_deref()) == Some(me))
        .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
        .map(|r| r.state.clone())
        .unwrap_or_else(|| "NONE".to_string())
}

fn derive_title(raw_title: &str, cfg: &BoardConfig) -> (Option<String>, Option<String>, String) {
    let (issue, issue_url) = match &cfg.issue_link {
        Some(rule) => match rule.pattern.find(raw_title) {
            Some(m) => {
                let id = m.as_str().to_string();
                let url = rule.url_template.replace("{id}", &id);
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
            suggested: cfg.default_reviewers.clone(),
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
        Blocker::UnresolvedComments(n) => format!("🟡 {n} unresolved comments"),
    }
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
    s
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
            _ => "✅ awaiting review".to_string(),
        },
        Category::Draft => {
            if row.conflict {
                "🔴 draft · merge conflict".to_string()
            } else if row.unresolved > 0 {
                format!("🟡 draft · {} unresolved comments", row.unresolved)
            } else if row.ci == Ci::Fail {
                "🔴 draft · CI failing".to_string()
            } else {
                "· draft".to_string()
            }
        }
        _ => String::new(),
    }
}

/// Mode B Note (SKILL.md).
fn review_note(row: &BoardRow) -> String {
    let note = match row.category {
        Category::Todo | Category::Available => {
            if row.ci == Ci::Fail {
                "⚠️ CI red — maybe wait for green".to_string()
            } else if row.conflict {
                "⚠️ has conflicts".to_string()
            } else if row.category == Category::Available {
                "available for review".to_string()
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
            assert_eq!(query, REVIEW_SEARCH_QUERY);
            assert_eq!(variables.len(), 3);
            assert!(variables.contains(&("who", "me")));
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
    fn team_slug_counts_as_requested_reviewer() {
        let mut v = base(7);
        v["reviewRequests"]["nodes"] = json!([
            {"requestedReviewer": {"__typename": "Team", "slug": "platform"}},
            {"requestedReviewer": null}
        ]);
        let row = derive_one(v, Mode::Authored);
        assert_eq!(row.requested, vec!["platform"]);
        assert_eq!(row.review_state, ReviewState::Waiting);
        assert_eq!(row.category, Category::Await);
        assert_eq!(row.note, "✅ awaiting review");
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
            "🟡 draft · 1 unresolved comments"
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
