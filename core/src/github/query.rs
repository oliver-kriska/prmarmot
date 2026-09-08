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

/// Prototype authored query extended with native stacks, request totals,
/// pagination visibility, and the live rate-limit budget.
pub const PR_SEARCH_QUERY: &str = r#"query($q:String!){
  search(query:$q, type:ISSUE, first:60){
    pageInfo{ hasNextPage }
    nodes{ ... on PullRequest {
      number title isDraft reviewDecision mergeable createdAt
      stack { number size baseRefName }
      stackEntry { position }
      author{ login }
      labels(first:20){ nodes{ name } }
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
pub const REVIEW_SEARCH_QUERY: &str = r#"query($requested:String!,$available:String!){
  requested: search(query:$requested, type:ISSUE, first:60){
    pageInfo{ hasNextPage }
    nodes{ ...ReviewQueuePr }
  }
  available: search(query:$available, type:ISSUE, first:60){
    pageInfo{ hasNextPage }
    nodes{ ...ReviewQueuePr }
  }
  rateLimit { limit cost remaining resetAt }
}
fragment ReviewQueuePr on PullRequest {
  number title isDraft reviewDecision mergeable createdAt
  stack { number size baseRefName }
  stackEntry { position }
  author{ login }
  labels(first:20){ nodes{ name } }
  reviewRequests(first:15){ totalCount nodes{ requestedReviewer{ __typename ... on User{login} ... on Team{slug} } } }
  reviews(first:60){ nodes{ author{login} state submittedAt } }
  reviewThreads(first:100){ nodes{ isResolved } }
  commits(last:1){ nodes{ commit{ statusCheckRollup{ state } } } }
}"#;

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

/// One PR as returned by the search query. Field names follow GitHub's schema;
/// the product view-model lives in [`crate::board::BoardRow`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPr {
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
    pub review_threads: Nodes<ThreadNode>,
    #[serde(default)]
    pub commits: Nodes<CommitNode>,
}

#[derive(Debug, Clone)]
pub struct ReviewSearchResult {
    pub requested: Vec<RawPr>,
    pub available: Vec<RawPr>,
    pub rate: Option<RateLimitInfo>,
    pub truncated: bool,
}

pub fn parse_review_response(body: &Value) -> Result<ReviewSearchResult, GhError> {
    check_graphql_errors(body)?;
    let requested = parse_pr_nodes(body, "/data/requested/nodes")?;
    let available = parse_pr_nodes(body, "/data/available/nodes")?;
    let truncated = body
        .pointer("/data/requested/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || body
            .pointer("/data/available/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    Ok(ReviewSearchResult {
        requested,
        available,
        rate: parse_rate(body),
        truncated,
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
