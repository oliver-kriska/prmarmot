//! What happened since a moment — the standup report behind
//! `prmarmot-cli report --since`: the PRs involving you that merged and the
//! PRs you opened, from one bounded search operation with two aliases. What
//! still needs you comes from the ordinary board fetch, so the report never
//! reads a PR twice and costs one small request on top of a refresh.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::board::BoardScope;
use crate::github::query::{check_graphql_errors, parse_rate, Login, Nodes};
use crate::github::rate_limit::RateLimitInfo;
use crate::github::states::PrState;
use crate::github::{GhError, GithubTransport};

/// Rows each alias asks for; `issueCount` says how many more there were.
/// A standup covers days, not months: past this the report says it is
/// truncated rather than paging.
pub const REPORT_PAGE_SIZE: u8 = 100;

/// Plain fields only (no nested connections beyond the author), so each
/// alias costs about one point whatever the window.
pub const REPORT_QUERY: &str = r#"query($merged:String!, $opened:String!) {
  rateLimit { limit cost remaining resetAt }
  merged: search(query:$merged, type:ISSUE, first:100) { issueCount nodes { ... on PullRequest { number title url state createdAt mergedAt repository { nameWithOwner } author { __typename login } } } }
  opened: search(query:$opened, type:ISSUE, first:100) { issueCount nodes { ... on PullRequest { number title url state createdAt mergedAt repository { nameWithOwner } author { __typename login } } } }
}"#;

/// GitHub's search accepts a full timestamp with an offset, so a report
/// since 09:00 does not also include yesterday evening.
fn since_qualifier(since: DateTime<Utc>) -> String {
    since.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

fn scope_prefix(scope: &BoardScope) -> String {
    match scope {
        BoardScope::AllRepositories => String::new(),
        BoardScope::Repository(repo) => format!("repo:{repo} "),
    }
}

/// PRs involving you (authored, reviewed, commented, assigned) merged since
/// `since`. The resolved login, never `@me` (see `query::global_search_string`).
pub fn merged_search_string(scope: &BoardScope, who: &str, since: DateTime<Utc>) -> String {
    format!(
        "{}is:pr is:merged merged:>={} involves:{who} sort:updated-desc",
        scope_prefix(scope),
        since_qualifier(since)
    )
}

/// PRs you opened since `since`, whatever became of them.
pub fn opened_search_string(scope: &BoardScope, who: &str, since: DateTime<Utc>) -> String {
    format!(
        "{}is:pr created:>={} author:{who} sort:created-desc",
        scope_prefix(scope),
        since_qualifier(since)
    )
}

/// One PR as the report lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportPr {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub state: PrState,
    pub created_at: Option<String>,
    pub merged_at: Option<String>,
}

/// Both lists, in GitHub's order, and how many each search matched in all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportFetch {
    pub merged: Vec<ReportPr>,
    pub opened: Vec<ReportPr>,
    /// What the searches matched; more than the list holds means truncated.
    pub merged_total: usize,
    pub opened_total: usize,
    pub rate: Option<RateLimitInfo>,
}

impl ReportFetch {
    pub fn truncated(&self) -> bool {
        self.merged_total > self.merged.len() || self.opened_total > self.opened.len()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReportPr {
    /// Absent on a search node that is not a pull request.
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    repository: Option<RawRepository>,
    #[serde(default)]
    author: Option<Login>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepository {
    name_with_owner: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawSearch {
    #[serde(default)]
    issue_count: usize,
    #[serde(flatten)]
    nodes: Nodes<RawReportPr>,
}

/// The one request: `merged` and `opened` in one operation.
pub fn fetch_report(
    transport: &dyn GithubTransport,
    scope: &BoardScope,
    me: &str,
    since: DateTime<Utc>,
) -> Result<ReportFetch, GhError> {
    let merged = merged_search_string(scope, me, since);
    let opened = opened_search_string(scope, me, since);
    let body = transport.graphql(REPORT_QUERY, &[("merged", &merged), ("opened", &opened)])?;
    parse_report(&body)
}

/// Parse a [`REPORT_QUERY`] response.
pub fn parse_report(body: &Value) -> Result<ReportFetch, GhError> {
    check_graphql_errors(body)?;
    let (merged, merged_total) = alias(body, "merged")?;
    let (opened, opened_total) = alias(body, "opened")?;
    Ok(ReportFetch {
        merged,
        opened,
        merged_total,
        opened_total,
        rate: parse_rate(body),
    })
}

fn alias(body: &Value, name: &str) -> Result<(Vec<ReportPr>, usize), GhError> {
    let raw = body.pointer(&format!("/data/{name}")).cloned();
    let raw: RawSearch = match raw {
        Some(value) if !value.is_null() => serde_json::from_value(value)
            .map_err(|error| GhError::Parse(format!("report `{name}` search: {error}")))?,
        _ => RawSearch::default(),
    };
    let prs: Vec<ReportPr> = raw
        .nodes
        .nodes
        .into_iter()
        .filter_map(|raw| {
            // A search node that is not a pull request has no number.
            let number = raw.number?;
            let repo = raw.repository?.name_with_owner;
            Some(ReportPr {
                repo,
                number,
                title: raw.title,
                url: raw.url,
                author: raw.author.and_then(|author| author.login),
                state: PrState::parse(raw.state.as_deref().unwrap_or("OPEN")),
                created_at: raw.created_at,
                merged_at: raw.merged_at,
            })
        })
        .collect();
    let total = raw.issue_count.max(prs.len());
    Ok((prs, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn since() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 8, 0, 0).unwrap()
    }

    #[test]
    fn the_searches_carry_the_moment_the_scope_and_the_login() {
        assert_eq!(
            merged_search_string(&BoardScope::AllRepositories, "me", since()),
            "is:pr is:merged merged:>=2026-10-01T08:00:00+00:00 involves:me sort:updated-desc"
        );
        assert_eq!(
            opened_search_string(&BoardScope::Repository("acme/api".into()), "me", since()),
            "repo:acme/api is:pr created:>=2026-10-01T08:00:00+00:00 author:me sort:created-desc"
        );
    }

    #[test]
    fn a_report_response_lists_both_aliases_and_says_when_a_search_held_more() {
        let body = json!({
            "data": {
                "rateLimit": { "limit": 5000, "cost": 2, "remaining": 4900, "resetAt": "2026-10-01T09:00:00Z" },
                "merged": {
                    "issueCount": 150,
                    "nodes": [
                        { "number": 42, "title": "Ship it", "url": "https://github.com/acme/api/pull/42",
                          "state": "MERGED", "createdAt": "2026-09-30T10:00:00Z", "mergedAt": "2026-10-01T10:00:00Z",
                          "repository": { "nameWithOwner": "acme/api" }, "author": { "__typename": "User", "login": "me" } },
                        { "id": "I_1", "title": "an issue, not a PR" }
                    ]
                },
                "opened": {
                    "issueCount": 1,
                    "nodes": [
                        { "number": 50, "title": "New", "url": "https://github.com/acme/api/pull/50",
                          "state": "OPEN", "createdAt": "2026-10-01T11:00:00Z", "mergedAt": null,
                          "repository": { "nameWithOwner": "acme/api" }, "author": { "__typename": "User", "login": "me" } }
                    ]
                }
            }
        });
        let report = parse_report(&body).unwrap();
        assert_eq!(report.merged.len(), 1);
        assert_eq!(report.merged[0].number, 42);
        assert_eq!(report.merged[0].state, PrState::Merged);
        assert_eq!(report.merged[0].author.as_deref(), Some("me"));
        assert_eq!(report.merged_total, 150);
        assert_eq!(report.opened[0].number, 50);
        assert_eq!(report.opened[0].state, PrState::Open);
        assert!(report.truncated());
        assert_eq!(report.rate.unwrap().cost, 2);
    }

    #[test]
    fn a_missing_alias_is_an_empty_list_and_an_error_is_reported() {
        let report = parse_report(&json!({ "data": { "merged": null } })).unwrap();
        assert!(report.merged.is_empty() && report.opened.is_empty());
        assert!(!report.truncated());
        let error = parse_report(&json!({
            "errors": [{ "type": "RATE_LIMITED", "message": "slow down" }]
        }));
        assert!(error.is_err());
    }
}
