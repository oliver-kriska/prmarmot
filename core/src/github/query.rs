//! Bounded GraphQL board queries and the raw response model.
//!
//! Never fan out per-PR REST calls. Authored mode and All open each use one
//! search; review mode combines requested and unrequested candidates in one
//! operation. All return `rateLimit{}` so the UI reports the actual shared
//! budget.

use super::states::{Mergeable, PrState, ReviewDecision, ReviewVerdict};
use serde::Deserialize;
use serde_json::Value;

use super::rate_limit::RateLimitInfo;
use super::GhError;

/// Search query string for a view. `who` must be a resolved login, not `@me`
/// (GraphQL search does not expand `@me`; the prototype resolves it first).
pub fn search_string(mode: crate::board::Mode, repo: &str, who: &str) -> String {
    match mode {
        crate::board::Mode::Authored => format!("repo:{repo} is:pr is:open author:{who}"),
        crate::board::Mode::Review => {
            format!("repo:{repo} is:pr is:open review-requested:{who} -author:{who}")
        }
        crate::board::Mode::AllOpen => all_open_search_string(repo, ""),
    }
}

/// Every open PR in one repository, most recently updated first, so the first
/// page is the live end of the list and Load more walks back in time.
/// `qualifiers` is [`crate::search::RemoteFilter::qualifiers`]: the labels and
/// authors GitHub can match exactly, so a filtered view counts and pages over
/// the whole repository instead of the rows already loaded.
pub fn all_open_search_string(repo: &str, qualifiers: &str) -> String {
    format!("repo:{repo} is:pr is:open sort:updated-desc{qualifiers}")
}

/// Broad review-queue candidates. GitHub search has no working
/// `no:review-requested` qualifier, so callers filter `reviewRequests.totalCount`
/// after fetching these alongside the requested-review alias.
pub fn available_search_string(repo: &str, who: &str) -> String {
    format!("repo:{repo} is:pr is:open -author:{who}")
}

/// All-repositories queue searches. The resolved login is used deliberately:
/// GitHub's GraphQL search does not expand `@me` consistently. Across every
/// repository a search can match more PRs than one page holds, and without a
/// `sort:` GitHub lists the newest *created* first, so an old PR with fresh
/// activity would fall off the first page; `sort:updated-desc` keeps the live
/// end on it, as All open does.
pub fn global_search_string(mode: crate::board::Mode, who: &str) -> String {
    match mode {
        // All open covers one repository, and the fetch refuses it across all
        // of them; were it ever asked here, it must stay scoped to the viewer
        // rather than become a search of every open PR on GitHub.
        crate::board::Mode::Authored | crate::board::Mode::AllOpen => {
            format!("is:pr is:open involves:{who} sort:updated-desc")
        }
        crate::board::Mode::Review => {
            format!("is:pr is:open review-requested:{who} -author:{who} sort:updated-desc")
        }
    }
}

/// Every open PR you authored, in any repository (the narrower alternative to
/// the involving search above).
pub fn global_authored_search_string(who: &str) -> String {
    format!("is:pr is:open author:{who} sort:updated-desc")
}

/// Global available-review candidates must remain involvement-scoped. A bare
/// `-author:` query would pull arbitrary public PRs from across GitHub.
pub fn global_available_search_string(who: &str) -> String {
    format!("is:pr is:open involves:{who} -author:{who} sort:updated-desc")
}

/// The fields every board query selects for one PR, in one place so the
/// initial, Load-more, and tracked selections can never drift apart. The
/// `timelineItems` window dates the current review requests and the last
/// "ready for review" for the pickup age (`crate::pickup`); it adds one point
/// to a two-alias review query's `rateLimit.cost` (7 → 8) and nothing to a
/// single search (4). `additions deletions changedFiles` feed the size band
/// (`crate::size`) and cost nothing. `reviews` and `reviewThreads` come oldest
/// first, so both take the newest end (`last:`): a busy PR's latest reviews
/// decide its standing reviews, and every reply in a thread is a review of its
/// own. `totalCount` says when a PR has more threads than the window, so the
/// unresolved count is a lower bound. `mergeStateStatus`, `canBeRebased` and
/// the repository's allowed merge methods say whether GitHub's merge button
/// would work (`crate::board::MergeState`); plain fields, no preview header on
/// github.com or GHES 3.17+. The rollup's per-state counts of checks and
/// statuses let a cancelled check stop reading as a failure
/// (`crate::board::derive_ci`); they cost nothing extra (2 points for 30 rows
/// either way, measured 2026-09-30). The newest 30 contexts' names and run
/// URLs, and each thread's file path, are plain fields under windows already
/// paid for: still 2 points and the same wall time (measured 2026-10-02).
macro_rules! pr_fields {
    () => {
        r#"id url repository { nameWithOwner mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed } updatedAt headRefOid
  number title isDraft reviewDecision mergeable mergeStateStatus canBeRebased createdAt
  mergeQueueEntry { state position }
  additions deletions changedFiles
  stack { number size baseRefName }
  stackEntry { position }
  author{ __typename login }
  labels(first:20){ nodes{ name } }
  latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(last:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(last:100){ totalCount nodes{ isResolved isOutdated path } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state contexts(first:30){ checkRunCountsByState{ state count } statusContextCountsByState{ state count } nodes{ __typename ... on CheckRun{ name conclusion detailsUrl } ... on StatusContext{ context state targetUrl } } } } } } }
  history: commits(last:20){ totalCount nodes{ commit{ oid } } }
  timelineItems(last:10, itemTypes:[REVIEW_REQUESTED_EVENT, READY_FOR_REVIEW_EVENT]){ nodes{ __typename ... on ReviewRequestedEvent{ createdAt requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } ... on ReadyForReviewEvent{ createdAt } } }"#
    };
}

/// Rows a search page asks for. GitHub stops a GraphQL request after about
/// 10 s, and with every field above a page of 60 PRs takes 6–11 s when they
/// span many repositories (measured 2026-09-29), so a request GitHub gives up
/// on is asked again once with [`SMALL_PAGE_SIZE`].
pub const PAGE_SIZE: u8 = 60;

/// The page a view falls back to after GitHub gave up on [`PAGE_SIZE`];
/// about 5 s for the same searches.
pub const SMALL_PAGE_SIZE: u8 = 30;

/// `query` with its searches asking for `first` rows instead of
/// [`PAGE_SIZE`]. Only the board searches change: the per-PR windows and All
/// open's `requested` id list keep their own bounds.
pub fn with_page_size(query: &str, first: u8) -> String {
    if first == PAGE_SIZE {
        return query.to_owned();
    }
    query.replace(
        &format!("type:ISSUE, first:{PAGE_SIZE}"),
        &format!("type:ISSUE, first:{first}"),
    )
}

/// The shared PR selection as the review queries' fragment.
macro_rules! review_queue_fragment {
    () => {
        concat!(
            "\nfragment ReviewQueuePr on PullRequest {\n  ",
            pr_fields!(),
            "\n}"
        )
    };
}

/// Prototype authored query extended with native stacks, request totals,
/// pagination visibility, and the live rate-limit budget. `issueCount` is how
/// many PRs the search matched in all, loaded or not; it costs nothing.
pub const PR_SEARCH_QUERY: &str = concat!(
    r#"query($q:String!,$who:String!){
  search(query:$q, type:ISSUE, first:60){
    issueCount
    pageInfo{ hasNextPage endCursor }
    nodes{ ... on PullRequest {
      "#,
    pr_fields!(),
    r#"
    } }
  }
  rateLimit { limit cost remaining resetAt }
}"#
);

pub const PR_SEARCH_PAGE_QUERY: &str = concat!(
    r#"query($q:String!,$after:String!,$who:String!){
  search(query:$q, type:ISSUE, first:60, after:$after){
    issueCount
    pageInfo{ hasNextPage endCursor }
    nodes{ ... on PullRequest {
      "#,
    pr_fields!(),
    r#"
    } }
  }
  rateLimit { limit cost remaining resetAt }
}"#
);

/// Review mode keeps requested PRs as the first alias so they cannot be
/// starved by broad available candidates. Both aliases are bounded at 60.
pub const REVIEW_SEARCH_QUERY: &str = concat!(
    r#"query($requested:String!,$available:String!,$who:String!){
  requested: search(query:$requested, type:ISSUE, first:60){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  available: search(query:$available, type:ISSUE, first:60){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}"#,
    review_queue_fragment!()
);

pub const REVIEW_REQUESTED_PAGE_QUERY: &str = concat!(
    r#"query($requested:String!,$after:String!,$who:String!){
  requested: search(query:$requested, type:ISSUE, first:60, after:$after){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}"#,
    review_queue_fragment!()
);

pub const REVIEW_AVAILABLE_PAGE_QUERY: &str = concat!(
    r#"query($available:String!,$after:String!,$who:String!){
  available: search(query:$available, type:ISSUE, first:60, after:$after){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}"#,
    review_queue_fragment!()
);

pub const REVIEW_BOTH_PAGE_QUERY: &str = concat!(
    r#"query($requested:String!,$requestedAfter:String!,$available:String!,$availableAfter:String!,$who:String!){
  requested: search(query:$requested, type:ISSUE, first:60, after:$requestedAfter){ pageInfo{ hasNextPage endCursor } nodes{ ...ReviewQueuePr } }
  available: search(query:$available, type:ISSUE, first:60, after:$availableAfter){ pageInfo{ hasNextPage endCursor } nodes{ ...ReviewQueuePr } }
  rateLimit { limit cost remaining resetAt }
}"#,
    review_queue_fragment!()
);

const TRACKED_FRAGMENT: &str = concat!(
    "\nfragment TrackedPr on PullRequest {\n  state merged ",
    pr_fields!(),
    "\n}"
);

/// Add one bounded `nodes(ids:)` selection to an existing initial refresh
/// operation. This is deliberately not used for Load more: tracked coverage is
/// refreshed once with page one and never fanned out per PR.
pub fn with_tracked_nodes(query: &str) -> Result<String, GhError> {
    let mut extended = extend_operation(
        query,
        ",$tracked:[ID!]!",
        "  tracked: nodes(ids:$tracked){ ... on PullRequest { ...TrackedPr } }\n",
    )?;
    extended.push_str(TRACKED_FRAGMENT);
    Ok(extended)
}

/// Add a state-only `nodes(ids:)` selection, `trackedStatus`, for followed PRs
/// whose full row the caller does not need: one already on the board, or a
/// watch known to be closed. About a point and almost no server time, where
/// the full fragment costs as much as a search result. The transport carries
/// one id list (`$tracked`), so these ids are written into the operation, and
/// each must look like a GitHub node id.
pub fn with_tracked_status_nodes(query: &str, ids: &[String]) -> Result<String, GhError> {
    if ids.is_empty() {
        return Ok(query.to_owned());
    }
    if let Some(bad) = ids.iter().find(|id| !is_node_id(id)) {
        return Err(GhError::Parse(format!("not a GitHub node id: {bad:?}")));
    }
    let list = ids
        .iter()
        .map(|id| format!("\"{id}\""))
        .collect::<Vec<_>>()
        .join(",");
    extend_operation(
        query,
        "",
        &format!(
            "  trackedStatus: nodes(ids:[{list}]){{ ... on PullRequest {{ id state merged }} }}\n"
        ),
    )
}

/// GitHub node ids are ASCII letters, digits, `_`, `-` and `=` (`PR_kwDO…`,
/// and the older base64 `MDExOlB1bGxSZXF1ZXN0…`).
fn is_node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'='))
}

/// What became of specific PRs, and nothing else: state only, no connections,
/// so it costs a point however many ids it carries (at most 100).
pub const TRACKED_STATUS_QUERY: &str = "query($tracked:[ID!]!){\n  rateLimit { limit cost remaining resetAt }\n  tracked: nodes(ids:$tracked){ ... on PullRequest { id state merged } }\n}";

/// Only the rate budget; [`with_tracked_nodes`] adds the tracked PRs. For
/// following specific PRs without running a board search.
pub const TRACKED_ONLY_QUERY: &str =
    "query($who:String!){\n  rateLimit { limit cost remaining resetAt }\n}";

/// Resolve `owner/name#number` to a node id (`$owner`, `$name`). The number is
/// inlined because string variables are the transport's only kind.
pub fn pull_request_id_query(number: u64) -> String {
    format!(
        "query($owner:String!,$name:String!){{\n  repository(owner:$owner, name:$name){{ pullRequest(number:{number}){{ id }} }}\n}}"
    )
}

/// The node id from a [`pull_request_id_query`] response, telling a missing
/// repository apart from a missing pull request.
pub fn parse_pull_request_id(body: &Value, repo: &str, number: u64) -> Result<String, GhError> {
    if let Some(id) = body
        .pointer("/data/repository/pullRequest/id")
        .and_then(Value::as_str)
    {
        return Ok(id.to_owned());
    }
    let not_found_at = |path: &[&str]| {
        body.get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| {
                errors.iter().any(|error| {
                    error.get("type").and_then(Value::as_str) == Some("NOT_FOUND")
                        && error
                            .get("path")
                            .and_then(Value::as_array)
                            .is_some_and(|at| {
                                at.iter()
                                    .map(Value::as_str)
                                    .eq(path.iter().map(|p| Some(*p)))
                            })
                })
            })
    };
    if not_found_at(&["repository", "pullRequest"]) {
        return Err(GhError::PullRequestNotFound(format!("{repo}#{number}")));
    }
    if not_found_at(&["repository"]) {
        return Err(GhError::RepositoryNotFound(repo.to_owned()));
    }
    check_graphql_errors(body)?;
    Err(GhError::PullRequestNotFound(format!("{repo}#{number}")))
}

/// Rate budget of a [`TRACKED_ONLY_QUERY`] response, after the error check
/// (a tracked PR GitHub can no longer resolve is not an error).
pub fn parse_tracked_response(body: &Value) -> Result<Option<RateLimitInfo>, GhError> {
    check_graphql_errors(body)?;
    Ok(parse_rate(body))
}

/// Most review requests All open reads per refresh; more than this many open
/// requests to one person in one repository is not a queue anyone works.
pub const MAX_ALL_OPEN_REQUESTED: usize = 100;

/// All open asks, in the same operation, which of the repository's open PRs
/// request your review — the review queue's own search (`$requested`), team
/// requests included — so "Requested from you" is the same set in both views.
/// Ids only: the rows come from the main search.
pub fn with_requested_ids(query: &str) -> Result<String, GhError> {
    extend_operation(
        query,
        ",$requested:String!",
        "  requested: search(query:$requested, type:ISSUE, first:100){ nodes{ ... on PullRequest { id } } }\n",
    )
}

/// The ids [`with_requested_ids`] returned, at most
/// [`MAX_ALL_OPEN_REQUESTED`].
pub fn requested_ids(body: &Value) -> Vec<String> {
    body.pointer("/data/requested/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|node| node.get("id").and_then(Value::as_str))
        .take(MAX_ALL_OPEN_REQUESTED)
        .map(str::to_owned)
        .collect()
}

/// Alias holding the scoped repository in an initial refresh operation.
pub const SCOPE_REPOSITORY_ALIAS: &str = "scopeRepository";

/// Look the scoped repository up in the same initial operation (variables
/// `$scopeOwner`, `$scopeName`), so a missing or inaccessible repository is an
/// error instead of an empty board: search silently matches nothing for it.
pub fn with_scope_repository(query: &str) -> Result<String, GhError> {
    extend_operation(
        query,
        ",$scopeOwner:String!,$scopeName:String!",
        "  scopeRepository: repository(owner:$scopeOwner, name:$scopeName){ nameWithOwner }\n",
    )
}

/// `RepositoryNotFound` when GitHub could not resolve the scoped repository.
pub fn scope_repository_error(body: &Value, repo: &str) -> Option<GhError> {
    body.get("errors")?
        .as_array()?
        .iter()
        .any(|error| {
            error.get("type").and_then(Value::as_str) == Some("NOT_FOUND")
                && error.pointer("/path/0").and_then(Value::as_str) == Some(SCOPE_REPOSITORY_ALIAS)
        })
        .then(|| GhError::RepositoryNotFound(repo.to_owned()))
}

/// Insert `variables` into the operation's variable list and `selection` as
/// its last top-level field.
fn extend_operation(query: &str, variables: &str, selection: &str) -> Result<String, GhError> {
    let variables_end = query
        .find("){\n")
        .ok_or_else(|| GhError::Parse("could not extend GraphQL operation".into()))?;
    let mut extended = query.to_owned();
    extended.insert_str(variables_end, variables);
    let operation_open = extended[variables_end..]
        .find('{')
        .map(|offset| variables_end + offset)
        .ok_or_else(|| GhError::Parse("GraphQL operation has no selection set".into()))?;
    let mut depth = 0usize;
    let mut operation_end = None;
    for (offset, byte) in extended.as_bytes()[operation_open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    operation_end = Some(operation_open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let operation_end =
        operation_end.ok_or_else(|| GhError::Parse("GraphQL operation is not balanced".into()))?;
    extended.insert_str(operation_end, selection);
    Ok(extended)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Nodes<T> {
    #[serde(default = "Vec::new")]
    pub nodes: Vec<T>,
    #[serde(default, rename = "totalCount")]
    pub total_count: usize,
}

impl<T> Default for Nodes<T> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            total_count: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Login {
    pub login: Option<String>,
    /// `User`, `Bot`, `Organization`, `Mannequin` or `EnterpriseUserAccount`;
    /// only asked for on the PR author, so absent elsewhere.
    #[serde(default, rename = "__typename")]
    pub typename: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelNode {
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestedReviewer {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

/// A `ReviewRequestedEvent` or `ReadyForReviewEvent` from the PR timeline.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEvent {
    #[serde(rename = "__typename", default)]
    pub typename: String,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Who was asked, for a review request. `None` for a deleted user or team.
    #[serde(default)]
    pub requested_reviewer: Option<RequestedReviewer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequestNode {
    #[serde(default)]
    pub requested_reviewer: Option<RequestedReviewer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNode {
    #[serde(default)]
    pub author: Option<Login>,
    pub state: ReviewVerdict,
    #[serde(default)]
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestReview {
    pub state: ReviewVerdict,
    pub submitted_at: Option<String>,
    pub commit: Option<ReviewCommit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewCommit {
    pub oid: String,
}

/// One of the PR's newest commits (`history`), oldest first.
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryNode {
    pub commit: ReviewCommit,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNode {
    pub is_resolved: bool,
    /// The code the thread was on has since changed.
    #[serde(default)]
    pub is_outdated: bool,
    /// The file the thread is on. Absent from prototype fixtures.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCheckRollup {
    #[serde(default)]
    pub state: Option<String>,
    /// How many of the head commit's checks and statuses are in each state.
    /// Absent from recorded prototype fixtures, which the rollup `state`
    /// alone decides.
    #[serde(default)]
    pub contexts: Option<RollupContexts>,
    /// GitHub refused the rollup to this token (never part of GitHub's own
    /// answer; written by [`super::access::tolerate_access_errors`]).
    #[serde(default)]
    pub hidden: bool,
}

/// `StatusCheckRollupContextConnection`'s counts; its nodes are not read.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollupContexts {
    /// Check runs by `CheckRunState` (`SUCCESS`, `CANCELLED`, `IN_PROGRESS`…).
    #[serde(default)]
    pub check_run_counts_by_state: Option<Vec<StateCount>>,
    /// Commit statuses by `StatusState` (`SUCCESS`, `ERROR`, `PENDING`…).
    #[serde(default)]
    pub status_context_counts_by_state: Option<Vec<StateCount>>,
    /// The newest contexts themselves. Absent from prototype fixtures.
    #[serde(default)]
    pub nodes: Option<Vec<RawContext>>,
}

/// One check run or commit status on the head commit: a `CheckRun` has a
/// `name`, a `conclusion` and a `details_url`; a `StatusContext` has a
/// `context`, a `state` and a `target_url`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawContext {
    #[serde(default, rename = "__typename")]
    pub typename: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub details_url: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StateCount {
    pub state: String,
    #[serde(default)]
    pub count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    #[serde(default)]
    pub status_check_rollup: Option<StatusCheckRollup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommitNode {
    pub commit: Commit,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStack {
    pub number: u64,
    pub size: u64,
    pub base_ref_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStackEntry {
    pub position: u64,
}

/// `mergeQueueEntry { state position }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawMergeQueueEntry {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub position: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub name_with_owner: String,
    /// The merge methods the repository allows. Absent in prototype fixtures.
    #[serde(default)]
    pub merge_commit_allowed: Option<bool>,
    #[serde(default)]
    pub squash_merge_allowed: Option<bool>,
    #[serde(default)]
    pub rebase_merge_allowed: Option<bool>,
}

/// One PR as returned by the search query. Field names follow GitHub's schema;
/// the product view-model lives in [`crate::board::BoardRow`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPr {
    /// Optional only for legacy prototype fixtures. Live queries request these.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub repository: Option<Repository>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub head_ref_oid: Option<String>,
    #[serde(default)]
    pub state: Option<PrState>,
    #[serde(default)]
    pub merged: bool,
    pub number: u64,
    pub title: String,
    pub is_draft: bool,
    #[serde(default)]
    pub review_decision: Option<ReviewDecision>,
    #[serde(default)]
    pub mergeable: Option<Mergeable>,
    /// GitHub's verdict on the merge button (`CLEAN`, `UNSTABLE`, `BLOCKED`,
    /// …). Absent in prototype fixtures.
    #[serde(default)]
    pub merge_state_status: Option<String>,
    /// Whether rebase-and-merge would go through. Absent in prototype fixtures.
    #[serde(default)]
    pub can_be_rebased: Option<bool>,
    /// The PR's entry in its repository's merge queue, when it is in one.
    /// Absent in prototype fixtures and `null` for every PR not queued.
    #[serde(default)]
    pub merge_queue_entry: Option<RawMergeQueueEntry>,
    pub created_at: String,
    /// Change counts for the size band. Absent in prototype fixtures.
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub changed_files: Option<u64>,
    #[serde(default)]
    pub stack: Option<RawStack>,
    #[serde(default)]
    pub stack_entry: Option<RawStackEntry>,
    #[serde(default)]
    pub author: Option<Login>,
    #[serde(default)]
    pub labels: Nodes<LabelNode>,
    #[serde(default)]
    pub review_requests: Nodes<ReviewRequestNode>,
    #[serde(default)]
    pub reviews: Nodes<ReviewNode>,
    #[serde(default)]
    pub latest_review: Nodes<LatestReview>,
    #[serde(default)]
    pub review_threads: Nodes<ThreadNode>,
    /// The newest 20 commits, oldest first, and how many there are. Absent
    /// in prototype fixtures.
    #[serde(default)]
    pub history: Nodes<HistoryNode>,
    #[serde(default)]
    pub commits: Nodes<CommitNode>,
    /// The latest review requests and "ready for review" events, oldest
    /// first. Absent in prototype fixtures.
    #[serde(default)]
    pub timeline_items: Nodes<Option<TimelineEvent>>,
}

impl RawPr {
    pub fn repository_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.repository
            .as_ref()
            .map_or(fallback, |repo| repo.name_with_owner.as_str())
    }

    pub fn canonical_url(&self, fallback_repo: &str) -> String {
        self.url.clone().unwrap_or_else(|| {
            format!(
                "https://github.com/{}/pull/{}",
                self.repository_name(fallback_repo),
                self.number
            )
        })
    }
}

#[derive(Debug, Clone)]
pub struct ReviewSearchResult {
    pub requested: Vec<RawPr>,
    pub available: Vec<RawPr>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
    pub requested_page: PageInfo,
    pub available_page: PageInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

pub fn page_info(body: &Value, path: &str) -> PageInfo {
    let base = format!("/data/{path}/pageInfo");
    PageInfo {
        has_next_page: body
            .pointer(&format!("{base}/hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        end_cursor: body
            .pointer(&format!("{base}/endCursor"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// How many PRs a search alias matched in all, loaded or not.
pub fn issue_count(body: &Value, path: &str) -> Option<u64> {
    body.pointer(&format!("/data/{path}/issueCount"))
        .and_then(Value::as_u64)
}

pub fn parse_review_response(body: &Value) -> Result<ReviewSearchResult, GhError> {
    check_graphql_errors(body)?;
    let requested = parse_pr_nodes(body, "/data/requested/nodes")?;
    let available = parse_pr_nodes(body, "/data/available/nodes")?;
    let requested_page = page_info(body, "requested");
    let available_page = page_info(body, "available");
    let truncated = requested_page.has_next_page || available_page.has_next_page;
    Ok(ReviewSearchResult {
        requested,
        available,
        rate: parse_rate(body),
        truncated,
        requested_page,
        available_page,
    })
}

/// Parse the full `gh api graphql` response body.
///
/// GraphQL errors are classified (RATE_LIMITED vs the rest); nodes that are
/// not PullRequests (empty inline-fragment objects) are skipped.
pub fn parse_search_response(body: &Value) -> Result<(Vec<RawPr>, Option<RateLimitInfo>), GhError> {
    check_graphql_errors(body)?;
    Ok((
        parse_pr_nodes(body, "/data/search/nodes")?,
        parse_rate(body),
    ))
}

pub fn parse_alias_response(
    body: &Value,
    alias: &str,
) -> Result<(Vec<RawPr>, PageInfo, Option<RateLimitInfo>), GhError> {
    check_graphql_errors(body)?;
    Ok((
        parse_pr_nodes(body, &format!("/data/{alias}/nodes"))?,
        page_info(body, alias),
        parse_rate(body),
    ))
}

/// Fail on GraphQL errors, except a tracked PR GitHub can no longer resolve:
/// that node comes back `null` and is reported as inaccessible, not as a
/// failed refresh.
fn check_graphql_errors(body: &Value) -> Result<(), GhError> {
    if let Some(errors) = body.get("errors").and_then(Value::as_array) {
        let errors: Vec<&Value> = errors
            .iter()
            .filter(|error| {
                !(error.get("type").and_then(Value::as_str) == Some("NOT_FOUND")
                    && matches!(
                        error.pointer("/path/0").and_then(Value::as_str),
                        Some("tracked" | "trackedStatus")
                    ))
            })
            .collect();
        if !errors.is_empty() {
            // Only a transport without headers gets here (the `gh` CLI):
            // an HTTP response was already classified with its reset.
            if super::response::graphql_rate_limited(body) {
                return Err(GhError::RateLimited {
                    reset_epoch: None,
                    retry_after_secs: None,
                });
            }
            let msgs = errors
                .iter()
                .map(|e| {
                    e.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string()
                })
                .collect();
            return Err(GhError::GraphqlErrors(msgs));
        }
    }

    Ok(())
}

fn parse_pr_nodes(body: &Value, pointer: &str) -> Result<Vec<RawPr>, GhError> {
    let nodes = body
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| GhError::Parse(format!("missing {pointer}")))?;

    let mut prs = Vec::with_capacity(nodes.len());
    for node in nodes {
        // Non-PR search hits surface as `{}` through the inline fragment; a
        // pull request the token may not read at all comes back `null`.
        if node.is_null() || node.as_object().is_some_and(|o| o.is_empty()) {
            continue;
        }
        let pr: RawPr = serde_json::from_value(node.clone())
            .map_err(|e| GhError::Parse(format!("bad PullRequest node: {e}")))?;
        prs.push(pr);
    }

    Ok(prs)
}

fn parse_rate(body: &Value) -> Option<RateLimitInfo> {
    body.pointer("/data/rateLimit")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<RateLimitInfo>(v.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_queries_are_resolved_and_involvement_scoped() {
        assert_eq!(
            global_search_string(crate::board::Mode::Authored, "octocat"),
            "is:pr is:open involves:octocat sort:updated-desc"
        );
        assert_eq!(
            global_search_string(crate::board::Mode::Review, "octocat"),
            "is:pr is:open review-requested:octocat -author:octocat sort:updated-desc"
        );
        assert_eq!(
            global_available_search_string("octocat"),
            "is:pr is:open involves:octocat -author:octocat sort:updated-desc"
        );
        assert!(!global_available_search_string("octocat").starts_with("-author:"));
    }

    #[test]
    fn missing_tracked_prs_do_not_fail_the_refresh_but_other_errors_do() {
        let tracked_gone = serde_json::json!({
            "data": {"search": {"nodes": []}, "tracked": [null]},
            "errors": [{"type": "NOT_FOUND", "path": ["tracked", 0],
                        "message": "Could not resolve to a node with the global id of 'PR_x'."}]
        });
        assert!(parse_search_response(&tracked_gone).is_ok());

        let search_broken = serde_json::json!({
            "data": null,
            "errors": [{"type": "NOT_FOUND", "path": ["search"], "message": "nope"}]
        });
        assert_eq!(
            parse_search_response(&search_broken).unwrap_err(),
            GhError::GraphqlErrors(vec!["nope".into()])
        );
    }

    #[test]
    fn scope_repository_lookup_extends_both_initial_operations() {
        for query in [PR_SEARCH_QUERY, REVIEW_SEARCH_QUERY] {
            let extended = with_tracked_nodes(&with_scope_repository(query).unwrap()).unwrap();
            assert_eq!(
                extended
                    .matches("$scopeOwner:String!,$scopeName:String!")
                    .count(),
                1
            );
            assert_eq!(extended.matches("scopeRepository: repository(").count(), 1);
            assert_eq!(extended.matches("tracked: nodes(ids:$tracked)").count(), 1);
            // Both selections live inside the operation, before any fragment.
            let operation_end = extended.find("\n}").unwrap();
            assert!(extended.find("scopeRepository").unwrap() < operation_end);
            assert!(extended.find("tracked: nodes").unwrap() < operation_end);
        }

        let missing = serde_json::json!({
            "data": {"scopeRepository": null, "search": {"nodes": []}},
            "errors": [{"type": "NOT_FOUND", "path": ["scopeRepository"],
                        "message": "Could not resolve to a Repository with the name 'acme/nope'."}]
        });
        assert_eq!(
            scope_repository_error(&missing, "acme/nope"),
            Some(GhError::RepositoryNotFound("acme/nope".into()))
        );
        assert_eq!(
            scope_repository_error(&serde_json::json!({"data": {}}), "acme/widgets"),
            None
        );
    }

    #[test]
    fn pull_request_ids_resolve_and_say_what_is_missing() {
        let found = serde_json::json!({"data": {"repository": {"pullRequest": {"id": "PR_1"}}}});
        assert_eq!(
            parse_pull_request_id(&found, "acme/api", 7).unwrap(),
            "PR_1"
        );

        let no_pr = serde_json::json!({
            "data": {"repository": {"pullRequest": null}},
            "errors": [{"type": "NOT_FOUND", "path": ["repository", "pullRequest"], "message": "x"}]
        });
        assert_eq!(
            parse_pull_request_id(&no_pr, "acme/api", 7).unwrap_err(),
            GhError::PullRequestNotFound("acme/api#7".into())
        );

        let no_repo = serde_json::json!({
            "data": {"repository": null},
            "errors": [{"type": "NOT_FOUND", "path": ["repository"], "message": "x"}]
        });
        assert_eq!(
            parse_pull_request_id(&no_repo, "acme/api", 7).unwrap_err(),
            GhError::RepositoryNotFound("acme/api".into())
        );

        let limited = serde_json::json!({"errors": [{"type": "RATE_LIMITED", "message": "x"}]});
        assert_eq!(
            parse_pull_request_id(&limited, "acme/api", 7).unwrap_err(),
            GhError::RateLimited {
                reset_epoch: None,
                retry_after_secs: None,
            }
        );
        assert!(pull_request_id_query(7).contains("pullRequest(number:7)"));

        let tracked = with_tracked_nodes(TRACKED_ONLY_QUERY).unwrap();
        assert!(tracked.starts_with("query($who:String!,$tracked:[ID!]!){"));
        assert!(!tracked.contains("search("));
    }

    #[test]
    fn tracked_nodes_extend_each_initial_operation_once() {
        for query in [PR_SEARCH_QUERY, REVIEW_SEARCH_QUERY] {
            let extended = with_tracked_nodes(query).unwrap();
            assert_eq!(extended.matches("$tracked:[ID!]!").count(), 1);
            assert_eq!(extended.matches("tracked: nodes(ids:$tracked)").count(), 1);
            assert_eq!(extended.matches("fragment TrackedPr").count(), 1);
            assert!(extended.contains("state merged"));
        }
    }

    #[test]
    fn a_smaller_page_shrinks_only_the_board_searches() {
        let review = with_page_size(REVIEW_SEARCH_QUERY, SMALL_PAGE_SIZE);
        assert_eq!(review.matches("type:ISSUE, first:30").count(), 2);
        assert!(!review.contains("type:ISSUE, first:60"));
        // The per-PR windows keep their bounds.
        assert!(review.contains("reviews(last:60)"));
        assert!(review.contains("reviewThreads(last:100){ totalCount"));
        // All open's requested ids keep theirs.
        let all_open = with_page_size(&with_requested_ids(PR_SEARCH_QUERY).unwrap(), 30);
        assert!(all_open.contains("type:ISSUE, first:30"));
        assert!(all_open.contains("type:ISSUE, first:100"));
        for query in [PR_SEARCH_PAGE_QUERY, REVIEW_BOTH_PAGE_QUERY] {
            assert!(with_page_size(query, 30).contains("first:30, after:"));
            assert_eq!(with_page_size(query, PAGE_SIZE), query);
        }
    }

    #[test]
    fn github_giving_up_on_a_query_is_told_apart_from_other_failures() {
        for timeout in [
            GhError::Http {
                status: 502,
                message: "gh: HTTP 502".into(),
            },
            GhError::Http {
                status: 504,
                message: "GitHub is having trouble (504)".into(),
            },
            GhError::Network(
                "gh: We couldn't respond to your request in time. Sorry about that.".into(),
            ),
            GhError::GraphqlErrors(vec![
                "Something went wrong while executing your query. This may be the result of a timeout, or it could be a GitHub bug.".into(),
            ]),
            GhError::GraphqlErrors(vec!["Query exceeded resource limits".into()]),
        ] {
            assert!(timeout.is_query_timeout(), "{timeout:?}");
        }
        for other in [
            GhError::NotAuthenticated,
            GhError::RateLimited {
                reset_epoch: None,
                retry_after_secs: None,
            },
            GhError::Timeout("gh timed out after 60s — killed".into()),
            GhError::Http {
                status: 403,
                message: "GitHub refused the request (403)".into(),
            },
            // The status decides, whatever the sentence says.
            GhError::Http {
                status: 500,
                message: "GitHub is having trouble (500): 504 upstream".into(),
            },
            GhError::GraphqlErrors(vec!["Could not resolve to a Repository".into()]),
            GhError::Parse("missing /data/search/nodes".into()),
        ] {
            assert!(!other.is_query_timeout(), "{other:?}");
        }
    }
}
