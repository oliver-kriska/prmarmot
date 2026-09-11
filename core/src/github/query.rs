//! Bounded GraphQL board queries and the raw response model.
//!
//! Never fan out per-PR REST calls. Authored mode uses one search; review mode
//! combines requested and unrequested candidates in one operation. Both return
//! `rateLimit{}` so the UI reports the actual shared budget.

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
    }
}

/// Broad review-queue candidates. GitHub search has no working
/// `no:review-requested` qualifier, so callers filter `reviewRequests.totalCount`
/// after fetching these alongside the requested-review alias.
pub fn available_search_string(repo: &str, who: &str) -> String {
    format!("repo:{repo} is:pr is:open -author:{who}")
}

/// All-repositories queue searches. The resolved login is used deliberately:
/// GitHub's GraphQL search does not expand `@me` consistently.
pub fn global_search_string(mode: crate::board::Mode, who: &str) -> String {
    match mode {
        crate::board::Mode::Authored => format!("is:pr is:open involves:{who}"),
        crate::board::Mode::Review => {
            format!("is:pr is:open review-requested:{who} -author:{who}")
        }
    }
}

/// Global available-review candidates must remain involvement-scoped. A bare
/// `-author:` query would pull arbitrary public PRs from across GitHub.
pub fn global_available_search_string(who: &str) -> String {
    format!("is:pr is:open involves:{who} -author:{who}")
}

/// Prototype authored query extended with native stacks, request totals,
/// pagination visibility, and the live rate-limit budget.
pub const PR_SEARCH_QUERY: &str = r#"query($q:String!,$who:String!){
  search(query:$q, type:ISSUE, first:60){
    pageInfo{ hasNextPage endCursor }
    nodes{ ... on PullRequest {
      id url repository { nameWithOwner } updatedAt headRefOid
      number title isDraft reviewDecision mergeable createdAt
      stack { number size baseRefName }
      stackEntry { position }
      author{ login }
      labels(first:20){ nodes{ name } }
      latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
      reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
      reviews(first:60){ nodes{ author{login} state submittedAt } }
      reviewThreads(first:100){ nodes{ isResolved } }
      commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
    } }
  }
  rateLimit { limit cost remaining resetAt }
}"#;

pub const PR_SEARCH_PAGE_QUERY: &str = r#"query($q:String!,$after:String!,$who:String!){
  search(query:$q, type:ISSUE, first:60, after:$after){
    pageInfo{ hasNextPage endCursor }
    nodes{ ... on PullRequest {
      id url repository { nameWithOwner } updatedAt headRefOid
      number title isDraft reviewDecision mergeable createdAt
      stack { number size baseRefName }
      stackEntry { position }
      author{ login }
      labels(first:20){ nodes{ name } }
      latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
      reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
      reviews(first:60){ nodes{ author{login} state submittedAt } }
      reviewThreads(first:100){ nodes{ isResolved } }
      commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
    } }
  }
  rateLimit { limit cost remaining resetAt }
}"#;

/// Review mode keeps requested PRs as the first alias so they cannot be
/// starved by broad available candidates. Both aliases are bounded at 60.
pub const REVIEW_SEARCH_QUERY: &str = r#"query($requested:String!,$available:String!,$who:String!){
  requested: search(query:$requested, type:ISSUE, first:60){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  available: search(query:$available, type:ISSUE, first:60){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}
fragment ReviewQueuePr on PullRequest {
  id url repository { nameWithOwner } updatedAt headRefOid
  number title isDraft reviewDecision mergeable createdAt
  stack { number size baseRefName }
  stackEntry { position }
  author{ login }
  labels(first:20){ nodes{ name } }
  latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(first:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(first:100){ nodes{ isResolved } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
}"#;

pub const REVIEW_REQUESTED_PAGE_QUERY: &str = r#"query($requested:String!,$after:String!,$who:String!){
  requested: search(query:$requested, type:ISSUE, first:60, after:$after){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}
fragment ReviewQueuePr on PullRequest {
  id url repository { nameWithOwner } updatedAt headRefOid
  number title isDraft reviewDecision mergeable createdAt
  stack { number size baseRefName } stackEntry { position } author{ login }
  latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
  labels(first:20){ nodes{ name } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(first:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(first:100){ nodes{ isResolved } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
}"#;

pub const REVIEW_AVAILABLE_PAGE_QUERY: &str = r#"query($available:String!,$after:String!,$who:String!){
  available: search(query:$available, type:ISSUE, first:60, after:$after){
    pageInfo{ hasNextPage endCursor }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}
fragment ReviewQueuePr on PullRequest {
  id url repository { nameWithOwner } updatedAt headRefOid
  number title isDraft reviewDecision mergeable createdAt
  stack { number size baseRefName } stackEntry { position } author{ login }
  latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
  labels(first:20){ nodes{ name } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(first:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(first:100){ nodes{ isResolved } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
}"#;

pub const REVIEW_BOTH_PAGE_QUERY: &str = r#"query($requested:String!,$requestedAfter:String!,$available:String!,$availableAfter:String!,$who:String!){
  requested: search(query:$requested, type:ISSUE, first:60, after:$requestedAfter){ pageInfo{ hasNextPage endCursor } nodes{ ...ReviewQueuePr } }
  available: search(query:$available, type:ISSUE, first:60, after:$availableAfter){ pageInfo{ hasNextPage endCursor } nodes{ ...ReviewQueuePr } }
  rateLimit { limit cost remaining resetAt }
}
fragment ReviewQueuePr on PullRequest {
  id url repository { nameWithOwner } updatedAt headRefOid
  number title isDraft reviewDecision mergeable createdAt
  stack { number size baseRefName } stackEntry { position } author{ login }
  latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
  labels(first:20){ nodes{ name } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(first:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(first:100){ nodes{ isResolved } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
}"#;

const TRACKED_FRAGMENT: &str = r#"
fragment TrackedPr on PullRequest {
  id url state merged repository { nameWithOwner } updatedAt headRefOid
  number title isDraft reviewDecision mergeable createdAt
  stack { number size baseRefName } stackEntry { position } author{ login }
  labels(first:20){ nodes{ name } }
  latestReview: reviews(last:1, author:$who, states:[APPROVED,COMMENTED,CHANGES_REQUESTED,DISMISSED]){ nodes{ state submittedAt commit{oid} } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(first:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(first:100){ nodes{ isResolved } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
}"#;

/// Add one bounded `nodes(ids:)` selection to an existing initial refresh
/// operation. This is deliberately not used for Load more: tracked coverage is
/// refreshed once with page one and never fanned out per PR.
pub fn with_tracked_nodes(query: &str) -> Result<String, GhError> {
    let variables_end = query.find("){\n").ok_or_else(|| {
        GhError::Parse("could not extend GraphQL operation with tracked nodes".into())
    })?;
    let mut extended = query.to_owned();
    extended.insert_str(variables_end, ",$tracked:[ID!]!");
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
    extended.insert_str(
        operation_end,
        "  tracked: nodes(ids:$tracked){ ... on PullRequest { ...TrackedPr } }\n",
    );
    extended.push_str(TRACKED_FRAGMENT);
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

#[derive(Debug, Clone, Deserialize)]
pub struct Login {
    pub login: Option<String>,
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
    pub state: String,
    #[serde(default)]
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestReview {
    pub state: String,
    pub submitted_at: Option<String>,
    pub commit: Option<ReviewCommit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewCommit {
    pub oid: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNode {
    pub is_resolved: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCheckRollup {
    #[serde(default)]
    pub state: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    pub name_with_owner: String,
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
    pub state: Option<String>,
    #[serde(default)]
    pub merged: bool,
    pub number: u64,
    pub title: String,
    pub is_draft: bool,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(default)]
    pub mergeable: Option<String>,
    pub created_at: String,
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
    #[serde(default)]
    pub commits: Nodes<CommitNode>,
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

fn check_graphql_errors(body: &Value) -> Result<(), GhError> {
    if let Some(errors) = body.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            let rate_limited = errors
                .iter()
                .any(|e| e.get("type").and_then(Value::as_str) == Some("RATE_LIMITED"));
            if rate_limited {
                return Err(GhError::RateLimited { reset_epoch: None });
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
        // Non-PR search hits surface as `{}` through the inline fragment.
        if node.as_object().is_some_and(|o| o.is_empty()) {
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
            "is:pr is:open involves:octocat"
        );
        assert_eq!(
            global_search_string(crate::board::Mode::Review, "octocat"),
            "is:pr is:open review-requested:octocat -author:octocat"
        );
        assert_eq!(
            global_available_search_string("octocat"),
            "is:pr is:open involves:octocat -author:octocat"
        );
        assert!(!global_available_search_string("octocat").starts_with("-author:"));
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
}
