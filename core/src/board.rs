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
    search_string, with_page_size, with_requested_ids, with_scope_repository, with_tracked_nodes,
    with_tracked_status_nodes, RawPr, ReviewNode, ReviewSearchResult, RollupContexts, StateCount,
    PAGE_SIZE, PR_SEARCH_PAGE_QUERY, PR_SEARCH_QUERY, REVIEW_AVAILABLE_PAGE_QUERY,
    REVIEW_AVAILABLE_QUERY, REVIEW_BOTH_PAGE_QUERY, REVIEW_REQUESTED_PAGE_QUERY,
    REVIEW_REQUESTED_QUERY, REVIEW_SEARCH_QUERY, SMALL_PAGE_SIZE, TRACKED_ONLY_QUERY,
    TRACKED_STATUS_QUERY,
};
use crate::github::rate_limit::RateLimitInfo;
pub use crate::github::states::{Mergeable, PrState, ReviewDecision, ReviewVerdict};
use crate::github::{GhError, GithubTransport};
use crate::pickup::pickup_since;
use crate::search::RemoteFilter;
use crate::size::ChangeSize;

mod fetch;
pub use fetch::*;

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
    pub state: ReviewVerdict,
    pub submitted_at: Option<String>,
    /// The reviewer is a GitHub `Bot` account (a CI integration, a coding
    /// agent's reviewer), so this review is not a person's look at the PR.
    pub bot: bool,
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

/// Where a PR stands in its repository's merge queue (`mergeQueueEntry.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeQueueState {
    /// Waiting its turn (`QUEUED`).
    Queued,
    /// The queue is running its checks (`AWAITING_CHECKS`).
    AwaitingChecks,
    /// Ready; the queue merges it next (`MERGEABLE`).
    Mergeable,
    /// The queue is merging it now (`LOCKED`).
    Locked,
    /// The queue could not merge it; it leaves the queue (`UNMERGEABLE`).
    Unmergeable,
    /// A state this build does not know.
    Unknown,
}

impl MergeQueueState {
    pub fn from_github(state: &str) -> Self {
        match state {
            "QUEUED" => Self::Queued,
            "AWAITING_CHECKS" => Self::AwaitingChecks,
            "MERGEABLE" => Self::Mergeable,
            "LOCKED" => Self::Locked,
            "UNMERGEABLE" => Self::Unmergeable,
            _ => Self::Unknown,
        }
    }
}

/// A PR's place in the merge queue: GitHub merges it when its turn comes, so
/// an approved PR here is not "press merge" but "wait". `position` is
/// one-based, as GitHub shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeQueue {
    pub state: MergeQueueState,
    pub position: Option<u64>,
}

/// GitHub's own verdict on whether the merge button would work
/// (`mergeStateStatus`). The Note says "mergeable" only when GitHub does;
/// when GitHub did not report one (prototype fixtures) the prototype's Note
/// stands. See [`approved_note`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeState {
    /// Mergeable, checks passing (`CLEAN`, or `HAS_HOOKS`: clean with
    /// pre-receive hooks).
    Clean,
    /// Mergeable, but checks are not all passing yet (`UNSTABLE`).
    Unstable,
    /// Branch protection blocks the merge: a required check, review or rule
    /// (`BLOCKED`).
    Blocked,
    /// The base branch moved and the rules require an up-to-date branch
    /// (`BEHIND`).
    Behind,
    /// A merge commit cannot be cleanly created (`DIRTY`).
    Dirty,
    /// GitHub has not worked it out yet (`UNKNOWN`), or a value this build
    /// does not know.
    Unknown,
}

impl MergeState {
    pub fn from_github(status: &str) -> Self {
        match status {
            "CLEAN" | "HAS_HOOKS" => Self::Clean,
            "UNSTABLE" => Self::Unstable,
            "BLOCKED" => Self::Blocked,
            "BEHIND" => Self::Behind,
            "DIRTY" => Self::Dirty,
            _ => Self::Unknown,
        }
    }
}

/// The Note's tail when GitHub reports that rebase-and-merge would fail but
/// another merge method still works.
pub const CANNOT_REBASE_NOTE: &str = "can't rebase";

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
    /// GitHub cannot rebase the branch (e.g. it contains a merge commit or
    /// conflicts commit by commit) and the repository allows no other merge
    /// method, so it blocks like a conflict.
    CannotRebase,
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
    /// Authors whose PRs count as agent-authored on top of GitHub's `Bot`
    /// accounts: logins or `*` patterns (`copilot*`, `*[bot]`), ignoring case.
    pub agent_authors: Vec<String>,
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

impl BoardConfig {
    /// Whether a PR author is a coding agent or another bot: GitHub says so
    /// (`Bot`), or `agent_authors` names the login, ignoring case, with `*`
    /// standing for any run of characters.
    pub fn is_agent(&self, author: &crate::github::query::Login) -> bool {
        if author.typename.as_deref() == Some("Bot") {
            return true;
        }
        let Some(login) = author.login.as_deref() else {
            return false;
        };
        let login = login.to_lowercase();
        self.agent_authors
            .iter()
            .any(|pattern| glob_matches(&pattern.to_lowercase(), &login))
    }
}

/// `pattern` against `text`, where `*` matches any run of characters
/// (including none) and everything else matches itself.
fn glob_matches(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => {
            text.starts_with(head)
                && (0..=text.len() - head.len())
                    .filter(|&skip| text.is_char_boundary(head.len() + skip))
                    .any(|skip| glob_matches(tail, &text[head.len() + skip..]))
        }
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
            agent_authors: Vec::new(),
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
    /// Commits since your latest review, when the head moved on from the
    /// commit you reviewed and GitHub's commit list was read.
    pub commits_since_review: Option<CommitsSinceReview>,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub issue: Option<String>,
    pub issue_url: Option<String>,
    pub author: Option<String>,
    /// Opened by a coding agent or another bot: GitHub's `Bot` account type,
    /// or an author `BoardConfig::agent_authors` names (`is:agent`).
    pub agent: bool,
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
    /// GitHub's merge-button verdict; `None` when it was not reported.
    pub merge_state: Option<MergeState>,
    /// The PR's merge-queue entry, when it is in one.
    pub merge_queue: Option<MergeQueue>,
    /// GitHub reports that rebase-and-merge would fail although the
    /// repository allows it and the PR has no merge conflict.
    pub cannot_rebase: bool,
    /// The repository allows rebase merges and no other method, so a PR that
    /// cannot be rebased cannot be merged at all.
    pub rebase_only: bool,
    pub review_decision: Option<ReviewDecision>,
    pub review_state: ReviewState,
    /// Requested reviewers: logins and team slugs.
    pub requested: Vec<String>,
    /// The team slugs among `requested`.
    pub requested_teams: Vec<String>,
    pub reviews: Vec<ReviewSummary>,
    /// The viewer's standing review state (`APPROVED`, `COMMENTED`, …), or
    /// `NONE`; see [`standing_review`].
    pub my_review: Option<ReviewVerdict>,
    pub unresolved: usize,
    /// The checks on the latest commit that failed, newest first, each with
    /// its run page when GitHub gave one. At most [`MAX_FAILED_CHECKS`], from
    /// the newest 30 contexts; empty when none failed or the token may not
    /// read checks.
    pub failed_checks: Vec<FailedCheck>,
    /// The files the unresolved review threads are on, each once, newest
    /// thread first. At most [`MAX_UNRESOLVED_PATHS`].
    pub unresolved_paths: Vec<String>,
    /// The PR has more review threads than the newest 100 that were read, so
    /// `unresolved` counts only those and may be low ("5+").
    pub unresolved_capped: bool,
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
    /// The latest commit's checks counted by state; `None` when GitHub
    /// reported no counts or the token may not read them.
    pub checks: Option<CheckCounts>,
    pub note: String,
}

impl Default for BoardRow {
    /// An open PR with nothing against it and nothing known about it: no
    /// reviews, no checks, no labels, in Awaiting review. The starting point
    /// for fixtures and builders; `derive_row` fills every field itself.
    fn default() -> Self {
        Self {
            id: String::new(),
            repo: String::new(),
            updated_at: None,
            head_oid: None,
            reviewed_oid: None,
            reviewed_at: None,
            commits_since_review: None,
            number: 0,
            url: String::new(),
            title: String::new(),
            issue: None,
            issue_url: None,
            author: None,
            agent: false,
            stack: None,
            queue_provenance: None,
            draft: false,
            category: Category::Await,
            bug: false,
            labels: Vec::new(),
            ci: Ci::None,
            conflict: false,
            mergeable_unknown: false,
            merge_state: None,
            merge_queue: None,
            cannot_rebase: false,
            rebase_only: false,
            review_decision: None,
            review_state: ReviewState::None,
            requested: Vec::new(),
            requested_teams: Vec::new(),
            reviews: Vec::new(),
            my_review: None,
            unresolved: 0,
            failed_checks: Vec::new(),
            unresolved_paths: Vec::new(),
            unresolved_capped: false,
            blockers: Vec::new(),
            created_at: String::new(),
            waiting_since: None,
            size: None,
            checks: None,
            note: String::new(),
        }
    }
}

/// Defensive bound on the board size. The query already caps at `first:60`;
/// this keeps the bound explicit at the data boundary (bounded-everything
/// guardrail from the PRFlow post-mortem).
pub const MAX_BOARD_ROWS: usize = 60;

/// How many failing checks a row names (`BoardRow::failed_checks`).
pub const MAX_FAILED_CHECKS: usize = 10;
/// How many files a row names for its unresolved threads
/// (`BoardRow::unresolved_paths`).
pub const MAX_UNRESOLVED_PATHS: usize = 20;

/// How many commits the PR gained since your latest review, for a row whose
/// head is no longer the commit you reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitsSinceReview {
    pub count: u64,
    /// The commit you reviewed is older than the newest 20 read, so `count`
    /// is at least this ("20+").
    pub lower_bound: bool,
}

/// A check or status on the latest commit that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    /// The check run's name or the status's context, as GitHub shows it.
    pub name: String,
    /// The run's page (a check's `detailsUrl`, a status's `targetUrl`).
    pub url: Option<String>,
}
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
    /// GitHub gave up on a full page for this view, so its pages ask for
    /// [`SMALL_PAGE_SIZE`] rows, Load more included.
    small_pages: bool,
}

impl BoardPagination {
    /// Whether this view fell back to [`SMALL_PAGE_SIZE`] pages. A front end
    /// passes it back to the next refresh of the same view, so a view GitHub
    /// could not answer in time stops asking for full pages until it quits.
    pub fn small_pages(&self) -> bool {
        self.small_pages
    }

    /// Cursors for a new search of the same view (All open's filter changed),
    /// keeping [`Self::small_pages`]: the repository GitHub could not answer
    /// in time is the same one.
    pub fn restarted(&self) -> Self {
        Self {
            small_pages: self.small_pages,
            ..Self::default()
        }
    }

    /// GitHub gave up on this view even at [`SMALL_PAGE_SIZE`] rows; keep
    /// asking for small pages rather than starting at full ones again.
    pub fn mark_small_pages(&mut self) {
        self.small_pages = true;
    }

    fn page_size(&self) -> u8 {
        if self.small_pages {
            SMALL_PAGE_SIZE
        } else {
            PAGE_SIZE
        }
    }

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

/// The PRs a refresh follows besides its search results (watched and snoozed
/// ones), in the same request.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tracked<'a> {
    /// Full rows, for PRs off the board whose changes are observed.
    pub rows: &'a [String],
    /// State only (open, closed, merged): PRs whose row the board already
    /// holds, and watches known to be closed. A small fraction of the cost.
    pub status: &'a [String],
}

impl<'a> Tracked<'a> {
    /// Every id with its full row.
    pub fn rows(ids: &'a [String]) -> Self {
        Self {
            rows: ids,
            status: &[],
        }
    }
}

/// At most this many followed PRs ride a [`SMALL_PAGE_SIZE`] request with
/// their full rows; the rest are asked for their state only, and the rotation
/// brings their rows back on a later refresh. Without it, a view GitHub could
/// not answer at 60 rows could carry 30 search results plus 50 full tracked
/// PRs, more than the request that timed out.
pub const SMALL_PAGE_TRACKED_ROWS: usize = 15;

/// Tracked PRs on their own, without a board search.
#[derive(Debug, Clone)]
pub struct TrackedFetch {
    pub tracked: Vec<TrackedPr>,
    pub rate: Option<RateLimitInfo>,
    pub access: AccessGaps,
}

/// All open's order: the search's own, most recently updated first, so a Load
/// more page lands below what is already on screen.
fn sort_most_recently_updated(rows: &mut [BoardRow]) {
    rows.sort_by(most_recently_updated_first);
}

fn most_recently_updated_first(a: &BoardRow, b: &BoardRow) -> std::cmp::Ordering {
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
    if row.blocks_on_rebase() {
        facts.push(CANNOT_REBASE_NOTE.to_owned());
    }
    if row.ci == Ci::Fail {
        facts.push(ci_failing_text(row, ": "));
    }
    if row.review_decision == Some(ReviewDecision::ChangesRequested) {
        facts.push(changes_requested_text(row));
    }
    if row.unresolved > 0 {
        facts.push(unresolved_comments(row.unresolved, row.unresolved_capped));
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
    // An agent's PR with nothing against it and no review yet: say that no
    // person has looked, which is the one fact that decides who picks it up.
    let state = if facts.is_empty() && row.agent && !row.reviewed_by_a_person() {
        NO_HUMAN_LOOKED_NOTE.to_owned()
    } else {
        state
    };
    row.note = match named {
        Named::Yes => format!("{author}'s PR · {state}"),
        Named::No => state,
    };
}

/// The Note's state for an agent-authored PR that no person has reviewed and
/// nothing blocks. A review from a `Bot` account does not count, and a plain
/// comment is not a review.
pub const NO_HUMAN_LOOKED_NOTE: &str = "no human has looked yet";

impl BoardRow {
    /// Someone other than a `Bot` account has reviewed this PR: approved,
    /// commented through a review, or requested changes.
    pub fn reviewed_by_a_person(&self) -> bool {
        self.reviews.iter().any(|review| !review.bot)
    }
}

/// Category, blockers, and Note for one of your own PRs, from row facts only
/// (so a carried-forward fact can re-derive them).
fn classify_authored(row: &mut BoardRow, cfg: &BoardConfig) {
    row.category = if row.draft {
        Category::Draft
    } else if row.ci == Ci::Fail
        || row.conflict
        || row.blocks_on_rebase()
        || row.review_decision == Some(ReviewDecision::ChangesRequested)
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
    let unresolved_capped = pr.review_threads.total_count > pr.review_threads.nodes.len();
    let ci = derive_ci(pr);
    let conflict = pr.mergeable == Some(Mergeable::Conflicting);
    let mergeable_unknown = !matches!(
        pr.mergeable,
        Some(Mergeable::Mergeable | Mergeable::Conflicting)
    );
    let merge_state = pr
        .merge_state_status
        .as_deref()
        .map(MergeState::from_github);
    let merge_queue = pr.merge_queue_entry.as_ref().map(|entry| MergeQueue {
        state: entry
            .state
            .as_deref()
            .map_or(MergeQueueState::Unknown, MergeQueueState::from_github),
        position: entry.position,
    });
    let methods = pr.repository.as_ref();
    let rebase_allowed = methods.and_then(|r| r.rebase_merge_allowed) == Some(true);
    // Only a known-clean merge says anything about rebasing: a conflict is
    // already the blocker, and GitHub may not have worked either out yet.
    let cannot_rebase = rebase_allowed
        && pr.can_be_rebased == Some(false)
        && pr.mergeable == Some(Mergeable::Mergeable);
    let rebase_only = rebase_allowed
        && methods.and_then(|r| r.merge_commit_allowed) == Some(false)
        && methods.and_then(|r| r.squash_merge_allowed) == Some(false);
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
        commits_since_review: derive_commits_since_review(pr),
        number: pr.number,
        url,
        title,
        issue,
        issue_url,
        author: pr.author.as_ref().and_then(|a| a.login.clone()),
        agent: pr
            .author
            .as_ref()
            .is_some_and(|author| cfg.is_agent(author)),
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
        merge_state,
        merge_queue,
        cannot_rebase,
        rebase_only,
        review_decision: pr.review_decision.clone(),
        review_state: ReviewState::None,
        requested: requested_reviewers(pr),
        requested_teams: requested_teams(pr),
        reviews: latest_reviews_excluding(pr, me, &cfg.bots),
        my_review: Some(my_latest_review(pr, me)),
        unresolved,
        failed_checks: derive_failed_checks(pr),
        unresolved_paths: derive_unresolved_paths(pr),
        unresolved_capped,
        blockers: Vec::new(),
        created_at: pr.created_at.clone(),
        waiting_since: None,
        size: ChangeSize::from_counts(pr.additions, pr.deletions, pr.changed_files),
        checks: derive_checks(pr),
        note: String::new(),
    };

    match mode {
        // All open rows come from `derive_all_open_row`, which asks for one of
        // the other two; asked directly, a row reads as its author's.
        Mode::Authored | Mode::AllOpen => {
            let appr = row
                .reviews
                .iter()
                .filter(|r| r.state == ReviewVerdict::Approved)
                .count();
            let cmt = row
                .reviews
                .iter()
                .any(|r| r.state == ReviewVerdict::Commented);
            let chg = row
                .reviews
                .iter()
                .any(|r| r.state == ReviewVerdict::ChangesRequested);
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
            } else if row
                .my_review
                .as_ref()
                .is_some_and(ReviewVerdict::is_standing)
            {
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
    let rollup = latest_rollup(pr);
    if rollup.is_some_and(|r| r.hidden) {
        return Ci::Hidden;
    }
    let state = rollup.and_then(|r| r.state.as_deref()).unwrap_or("NONE");
    match state {
        "SUCCESS" => Ci::Pass,
        "FAILURE" | "ERROR" => derive_checks(pr)
            .and_then(ci_without_cancelled)
            .unwrap_or(Ci::Fail),
        "NONE" => Ci::None,
        _ => Ci::Running, // PENDING / EXPECTED
    }
}

fn latest_rollup(pr: &RawPr) -> Option<&crate::github::query::StatusCheckRollup> {
    pr.commits
        .nodes
        .first()
        .and_then(|c| c.commit.status_check_rollup.as_ref())
}

/// The latest commit's checks counted by state, or `None` when the token may
/// not read them or GitHub reported no counts.
fn derive_checks(pr: &RawPr) -> Option<CheckCounts> {
    latest_rollup(pr)
        .filter(|rollup| !rollup.hidden)
        .and_then(|rollup| rollup.contexts.as_ref())
        .and_then(CheckCounts::from_contexts)
}

/// Commits since the one you reviewed: those after it in the newest 20. A
/// head that is still the reviewed commit, no review, or no commit list
/// (prototype fixtures) gives `None`. A reviewed commit no longer in the list
/// — older than the window, or rewritten by a rebase — counts the whole
/// window, as a lower bound when the PR has more commits than were read.
fn derive_commits_since_review(pr: &RawPr) -> Option<CommitsSinceReview> {
    let reviewed = latest_review_evidence(pr)?.commit.as_ref()?.oid.as_str();
    if pr
        .head_ref_oid
        .as_deref()
        .is_none_or(|head| head == reviewed)
    {
        return None;
    }
    let oids: Vec<&str> = pr
        .history
        .nodes
        .iter()
        .map(|node| node.commit.oid.as_str())
        .collect();
    if oids.is_empty() {
        return None;
    }
    match oids.iter().rposition(|oid| *oid == reviewed) {
        Some(index) => {
            let count = (oids.len() - 1 - index) as u64;
            (count > 0).then_some(CommitsSinceReview {
                count,
                lower_bound: false,
            })
        }
        None => Some(CommitsSinceReview {
            count: oids.len() as u64,
            lower_bound: pr.history.total_count > oids.len(),
        }),
    }
}

/// "3 new commits since your review", "20+ new commits since your review";
/// `None` when the row does not know the count.
pub fn commits_since_review_text(row: &BoardRow) -> Option<String> {
    row.commits_since_review.map(|since| {
        let plus = if since.lower_bound { "+" } else { "" };
        let noun = if since.count == 1 && !since.lower_bound {
            "commit"
        } else {
            "commits"
        };
        format!("{}{plus} new {noun} since your review", since.count)
    })
}

/// The latest commit's failing checks and statuses, by the rule the counts
/// use: a check run whose conclusion is not a pass, a wait, a cancel, a skip,
/// neutral or stale failed; a status that is not success, pending or
/// expected failed. Each name once, in GitHub's order (newest first).
fn derive_failed_checks(pr: &RawPr) -> Vec<FailedCheck> {
    let Some(contexts) = latest_rollup(pr)
        .filter(|rollup| !rollup.hidden)
        .and_then(|rollup| rollup.contexts.as_ref())
        .and_then(|contexts| contexts.nodes.as_ref())
    else {
        return Vec::new();
    };
    let mut failed: Vec<FailedCheck> = Vec::new();
    for context in contexts {
        let (name, url, is_failure) = match context.typename.as_deref() {
            Some("CheckRun") => (
                context.name.as_deref(),
                context.details_url.as_deref(),
                check_run_failed(context.conclusion.as_deref()),
            ),
            Some("StatusContext") => (
                context.context.as_deref(),
                context.target_url.as_deref(),
                status_failed(context.state.as_deref()),
            ),
            _ => continue,
        };
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            continue;
        };
        if is_failure && !failed.iter().any(|known| known.name == name) {
            failed.push(FailedCheck {
                name: name.to_owned(),
                url: url.filter(|url| !url.is_empty()).map(str::to_owned),
            });
            if failed.len() == MAX_FAILED_CHECKS {
                break;
            }
        }
    }
    failed
}

/// [`CheckCounts::from_contexts`]'s rule for one check run's conclusion. A
/// run still going has no conclusion.
fn check_run_failed(conclusion: Option<&str>) -> bool {
    !matches!(
        conclusion,
        None | Some(
            "SUCCESS"
                | "PENDING"
                | "QUEUED"
                | "IN_PROGRESS"
                | "WAITING"
                | "CANCELLED"
                | "SKIPPED"
                | "NEUTRAL"
                | "COMPLETED"
                | "STALE"
        )
    )
}

/// [`CheckCounts::from_contexts`]'s rule for one commit status's state.
fn status_failed(state: Option<&str>) -> bool {
    !matches!(state, None | Some("SUCCESS" | "PENDING" | "EXPECTED"))
}

/// The files the unresolved threads are on, each once, in GitHub's order
/// (newest thread first within the window read).
fn derive_unresolved_paths(pr: &RawPr) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for thread in pr.review_threads.nodes.iter().rev() {
        if thread.is_resolved {
            continue;
        }
        let Some(path) = thread.path.as_deref().filter(|path| !path.is_empty()) else {
            continue;
        };
        if !paths.iter().any(|known| known == path) {
            paths.push(path.to_owned());
            if paths.len() == MAX_UNRESOLVED_PATHS {
                break;
            }
        }
    }
    paths
}

/// How the latest commit's checks stand, counted from GitHub's per-state
/// counts of check runs and commit statuses together. The CI column says one
/// word; the Details panel says these numbers.
///
/// A state this build does not know counts as failed, the same rule the CI
/// column follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckCounts {
    /// FAILURE, TIMED_OUT, STARTUP_FAILURE, ACTION_REQUIRED, a failing or
    /// erroring status, or a state this build does not know.
    pub failed: u64,
    /// Pending, queued, in progress or waiting; a pending or expected status.
    pub running: u64,
    pub passed: u64,
    pub cancelled: u64,
    pub skipped: u64,
    /// NEUTRAL, or COMPLETED without a conclusion.
    pub neutral: u64,
    pub stale: u64,
}

impl CheckCounts {
    /// `None` when every count is zero or there were none.
    pub fn from_contexts(contexts: &RollupContexts) -> Option<Self> {
        fn counted(counts: &Option<Vec<StateCount>>) -> impl Iterator<Item = (&str, u64)> {
            counts
                .iter()
                .flatten()
                .map(|count| (count.state.as_str(), count.count))
        }
        let mut checks = Self::default();
        for (state, count) in counted(&contexts.check_run_counts_by_state) {
            *match state {
                "SUCCESS" => &mut checks.passed,
                "PENDING" | "QUEUED" | "IN_PROGRESS" | "WAITING" => &mut checks.running,
                "CANCELLED" => &mut checks.cancelled,
                "SKIPPED" => &mut checks.skipped,
                "NEUTRAL" | "COMPLETED" => &mut checks.neutral,
                "STALE" => &mut checks.stale,
                _ => &mut checks.failed,
            } += count;
        }
        for (state, count) in counted(&contexts.status_context_counts_by_state) {
            *match state {
                "SUCCESS" => &mut checks.passed,
                "PENDING" | "EXPECTED" => &mut checks.running,
                _ => &mut checks.failed, // ERROR, FAILURE, or unknown
            } += count;
        }
        (checks.total() > 0).then_some(checks)
    }

    pub fn total(&self) -> u64 {
        self.failed
            + self.running
            + self.passed
            + self.cancelled
            + self.skipped
            + self.neutral
            + self.stale
    }
}

/// The CI a failing rollup stands for once cancelled checks are set aside:
/// running while any check or status still is, otherwise passing. `None` —
/// keep GitHub's failure — when a check or status failed or a state is one
/// this build does not know.
///
/// GitHub's rollup reads FAILURE for a cancelled check too, even one no rule
/// requires; a cancelled, skipped, neutral or stale check is not a failure
/// here (Oliver, 2026-09-30).
fn ci_without_cancelled(checks: CheckCounts) -> Option<Ci> {
    if checks.failed > 0 {
        None
    } else if checks.running > 0 {
        Some(Ci::Running)
    } else {
        Some(Ci::Pass)
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
    if last.state == ReviewVerdict::Commented {
        if let Some(held) = latest(|review| review.state != ReviewVerdict::Commented) {
            if matches!(
                held.state,
                ReviewVerdict::Approved | ReviewVerdict::ChangesRequested
            ) {
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
            // One author, so any of their reviews says what kind of account
            // it is.
            let bot = reviews.iter().any(|review| {
                review
                    .author
                    .as_ref()
                    .is_some_and(|author| author.typename.as_deref() == Some("Bot"))
            });
            standing_review(&reviews).map(|review| ReviewSummary {
                login,
                state: review.state.clone(),
                submitted_at: review.submitted_at.clone(),
                bot,
            })
        })
        .collect()
}

fn latest_review_evidence(pr: &RawPr) -> Option<&crate::github::query::LatestReview> {
    pr.latest_review
        .nodes
        .first()
        .filter(|review| review.state != ReviewVerdict::Dismissed)
}

/// The user's own standing review state, or "NONE" — the prototype's `$mine`.
fn my_latest_review(pr: &RawPr, me: &str) -> ReviewVerdict {
    let mine: Vec<&ReviewNode> = pr
        .reviews
        .nodes
        .iter()
        .filter(|r| r.author.as_ref().and_then(|a| a.login.as_deref()) == Some(me))
        .collect();
    standing_review(&mine)
        .map(|r| r.state.clone())
        .unwrap_or(ReviewVerdict::NoReview)
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
    if row.blocks_on_rebase() {
        blockers.push(Blocker::CannotRebase);
    }
    if row.ci == Ci::Fail {
        blockers.push(Blocker::CiFailing);
    }
    if row.review_decision == Some(ReviewDecision::ChangesRequested) {
        blockers.push(Blocker::ChangesRequested);
    }
    if row.unresolved > 0 {
        blockers.push(Blocker::UnresolvedComments(row.unresolved));
    }
    blockers
}

/// The failing checks' names, joined for a Note or a Details line:
/// "build, lint"; `None` when the row names none.
pub fn failed_checks_text(row: &BoardRow) -> Option<String> {
    (!row.failed_checks.is_empty()).then(|| {
        row.failed_checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Who asked for changes: the reviewers whose standing review is a change
/// request, in review order. Empty when the request came from a review the
/// window did not read.
pub fn changes_requested_by(row: &BoardRow) -> Vec<&str> {
    row.reviews
        .iter()
        .filter(|review| review.state == ReviewVerdict::ChangesRequested)
        .filter_map(|review| review.login.as_deref())
        .collect()
}

/// "changes requested", naming who asked when the row knows: "changes
/// requested by bob".
pub fn changes_requested_text(row: &BoardRow) -> String {
    let by = changes_requested_by(row);
    if by.is_empty() {
        "changes requested".to_owned()
    } else {
        format!("changes requested by {}", by.join(", "))
    }
}

/// "CI failing", naming the checks when the row knows: "CI failing — build,
/// lint". `sep` joins the two.
fn ci_failing_text(row: &BoardRow, sep: &str) -> String {
    match failed_checks_text(row) {
        Some(names) => format!("CI failing{sep}{names}"),
        None => "CI failing".to_owned(),
    }
}

/// The exact SKILL.md note fragment for one blocker — the prototype's wording
/// and emoji, verbatim. The joined fragments reproduce the legacy `note`
/// byte-for-byte (pinned by the golden tests). The evidence the prototype
/// never had — the failing checks' names, who asked for changes — follows the
/// fragment when the row knows it; the fixtures know none, so the goldens
/// hold.
fn blocker_note(blocker: &Blocker, row: &BoardRow) -> String {
    match blocker {
        Blocker::NoReviewers { suggested } => {
            if suggested.is_empty() {
                "⚠️ no reviewers".to_string()
            } else {
                format!("⚠️ no reviewers — assign {}", suggested.join(" + "))
            }
        }
        Blocker::MergeConflict => "🔴 merge conflict — rebase".to_string(),
        Blocker::CannotRebase => format!("🔴 {CANNOT_REBASE_NOTE} — rebase locally"),
        Blocker::CiFailing => format!("❌ {}", ci_failing_text(row, " — ")),
        Blocker::ChangesRequested => format!("✋ {}", changes_requested_text(row)),
        Blocker::UnresolvedComments(n) => {
            format!("🟡 {}", unresolved_comments(*n, row.unresolved_capped))
        }
    }
}

/// "1 unresolved comment", "2 unresolved comments". SKILL.md writes the rule
/// as "<n> unresolved comments"; the count decides the plural. A count taken
/// from only the newest 100 threads is a lower bound: "5+ unresolved comments".
fn unresolved_comments(n: usize, capped: bool) -> String {
    if capped {
        return format!("{n}+ unresolved comments");
    }
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

/// The Note for your own approved PR that nothing blocks. The prototype
/// says "approved — mergeable" for every one; this port says it only when
/// GitHub's merge state agrees, and otherwise names what GitHub is waiting
/// for (a recorded divergence from SKILL.md, Oliver 2026-09-29: an approved PR
/// with checks still running is not mergeable yet). Rows GitHub reported no
/// merge state for keep the prototype's wording, which is what the golden
/// fixtures pin. A branch GitHub cannot rebase, in a repository that allows
/// other methods, gets [`CANNOT_REBASE_NOTE`] as a tail.
fn approved_note(row: &BoardRow) -> String {
    if let Some(queue) = row.merge_queue {
        let mut note = format!("🟢 approved — {}", merge_queue_text(queue));
        if row.cannot_rebase {
            note.push_str(" · ");
            note.push_str(CANNOT_REBASE_NOTE);
        }
        return note;
    }
    let state = match (row.merge_state, row.ci) {
        (None, _) => Some("mergeable"),
        (Some(MergeState::Dirty | MergeState::Unknown), _) => None,
        (_, Ci::Running) => Some("waiting for CI"),
        (Some(MergeState::Clean), _) => Some("mergeable"),
        (Some(MergeState::Unstable), _) => Some("checks not passing"),
        (Some(MergeState::Blocked), _) => Some("blocked by branch rules"),
        (Some(MergeState::Behind), _) => Some("branch out of date"),
    };
    let mut note = match state {
        Some(state) => format!("🟢 approved — {state}"),
        None => "🟢 approved".to_string(),
    };
    if row.cannot_rebase {
        note.push_str(" · ");
        note.push_str(CANNOT_REBASE_NOTE);
    }
    note
}

/// What the merge queue is doing with the PR: "in merge queue, position 2"
/// while it waits or its checks run, "merging" once the queue has it, and
/// "merge queue couldn't merge it" when the queue gave up. Position is
/// GitHub's, one-based.
pub fn merge_queue_text(queue: MergeQueue) -> String {
    match queue.state {
        MergeQueueState::Locked => "merging".to_owned(),
        MergeQueueState::Unmergeable => "merge queue couldn't merge it".to_owned(),
        MergeQueueState::Queued
        | MergeQueueState::AwaitingChecks
        | MergeQueueState::Mergeable
        | MergeQueueState::Unknown => match queue.position {
            Some(position) => format!("in merge queue, position {position}"),
            None => "in merge queue".to_owned(),
        },
    }
}

impl BoardRow {
    /// GitHub cannot rebase this branch and the repository allows nothing
    /// else, so it blocks the merge like a conflict does.
    pub fn blocks_on_rebase(&self) -> bool {
        self.cannot_rebase && self.rebase_only && !self.conflict
    }

    /// Whether GitHub's own merge state agrees the merge button would work.
    /// `true` when GitHub did not report one (prototype fixtures).
    pub fn merge_state_clean(&self) -> bool {
        matches!(self.merge_state, None | Some(MergeState::Clean))
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
            .map(|blocker| blocker_note(blocker, row))
            .collect::<Vec<_>>()
            .join(" · "),
        Category::Await => match row.review_state {
            ReviewState::Approved => approved_note(row),
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
                format!(
                    "🟡 draft · {}",
                    unresolved_comments(row.unresolved, row.unresolved_capped)
                )
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
                match failed_checks_text(row) {
                    Some(names) => format!("⚠️ CI red: {names} — maybe wait for green"),
                    None => "⚠️ CI red — maybe wait for green".to_string(),
                }
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
        Category::Done => match row.my_review {
            Some(ReviewVerdict::Approved) => "✅ you approved".to_string(),
            Some(ReviewVerdict::ChangesRequested) => {
                "✋ you requested changes — on the author now".to_string()
            }
            Some(ReviewVerdict::Commented) => "💬 you commented".to_string(),
            _ => String::new(),
        },
        Category::Draft => "· draft (not ready)".to_string(),
        _ => String::new(),
    };
    // An agent's PR nobody has reviewed yet: the one fact that decides who
    // picks it up, after what the queue says about it.
    let note = if row.agent
        && !row.reviewed_by_a_person()
        && matches!(row.category, Category::Todo | Category::Available)
        && row.ci != Ci::Fail
        && !row.conflict
    {
        format!("{note} · {NO_HUMAN_LOOKED_NOTE}")
    } else {
        note
    };
    if row.reviewed_oid.is_some()
        && row.head_oid.is_some()
        && row.reviewed_oid != row.head_oid
        && !note.is_empty()
    {
        let since = commits_since_review_text(row)
            .unwrap_or_else(|| "new commits since your review".to_owned());
        format!("{since} · {note}")
    } else {
        note
    }
}

#[cfg(test)]
mod tests;
