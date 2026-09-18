//! What a token was refused, told apart from a failed refresh.
//!
//! GitHub answers a query that touches a field the token may not read with the
//! rest of `data` intact, that field `null`, and one `FORBIDDEN` error ("Resource
//! not accessible by personal access token") per occurrence. A fine-grained
//! personal access token is the usual cause: GitHub offers it no Checks
//! permission at all and refuses the pull request's head commit — observed on
//! 2026-09-18 as `search.nodes.N.commits.nodes.0`, once per pull request — so
//! the check rollup under it is never read. A team can be refused the same way
//! without the organization's Members permission. Failing the whole refresh on
//! those errors left such a token with no board at all.
//!
//! [`tolerate_access_errors`] runs on the response before it is parsed. For
//! each refusal inside a pull request it writes a marker where the field was
//! (so the row can say "hidden" rather than "none"), counts it once per pull
//! request, and removes the error. Anything else — a refused search, a rate
//! limit, an error with no `data` — is left for the existing error check.

use std::collections::HashSet;

use serde_json::{json, Value};

/// The name a requested team the token cannot see goes by. A team slug is
/// lowercase with hyphens, so this can never be a real team's slug; it takes
/// the slug's place so that "a team was asked" still reads as a request, and
/// the pickup age still pairs the request with its timeline event.
pub const HIDDEN_TEAM: &str = "hidden team";

/// The top-level fields whose items are pull requests: the board searches'
/// aliases and the tracked `nodes(ids:)` list.
const PULL_REQUEST_LISTS: [&str; 4] = ["search", "requested", "available", "tracked"];

/// How many pull requests had something withheld, by what. Each pull request
/// counts at most once per kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccessGaps {
    /// The head commit's check rollup; the row's CI reads as hidden.
    pub ci: usize,
    /// A requested team, in the request list or its timeline event.
    pub teams: usize,
    /// Any other field of the pull request, left empty.
    pub other: usize,
    /// The whole pull request, which is left out.
    pub pull_requests: usize,
}

impl AccessGaps {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Both counts together, for a Load more page added to a board.
    pub fn plus(self, other: Self) -> Self {
        Self {
            ci: self.ci + other.ci,
            teams: self.teams + other.teams,
            other: self.other + other.other,
            pull_requests: self.pull_requests + other.pull_requests,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Gap {
    Ci,
    Team,
    Other,
    PullRequest,
}

/// Mark every refused pull-request field in `body` and drop its error; see the
/// module docs. Returns what was withheld. A body without a `data` object is
/// left exactly as it came.
pub fn tolerate_access_errors(body: &mut Value) -> AccessGaps {
    let mut gaps = AccessGaps::default();
    if !body.get("data").is_some_and(Value::is_object) {
        return gaps;
    }
    let Some(errors) = body.get("errors").and_then(Value::as_array).cloned() else {
        return gaps;
    };

    let mut counted: HashSet<(Vec<Value>, Gap)> = HashSet::new();
    let mut kept = Vec::new();
    for error in errors {
        let Some((node, field)) = refused_pull_request_field(&error) else {
            kept.push(error);
            continue;
        };
        let data = body.get_mut("data").expect("checked above");
        let gap = if field.is_empty() {
            Gap::PullRequest
        } else if mark_ci_hidden(data, &node, &field) {
            Gap::Ci
        } else if mark(
            data,
            &node,
            &field,
            "requestedReviewer",
            json!({ "__typename": "Team", "slug": HIDDEN_TEAM }),
        ) {
            Gap::Team
        } else {
            clear_null(data, &node, &field);
            Gap::Other
        };
        if counted.insert((node, gap)) {
            match gap {
                Gap::Ci => gaps.ci += 1,
                Gap::Team => gaps.teams += 1,
                Gap::Other => gaps.other += 1,
                Gap::PullRequest => gaps.pull_requests += 1,
            }
        }
    }

    let object = body.as_object_mut().expect("has a data object");
    if kept.is_empty() {
        object.remove("errors");
    } else {
        object.insert("errors".into(), Value::Array(kept));
    }
    gaps
}

/// A refusal inside a pull request, split into the path of the pull request
/// (`["search", "nodes", 3]`, `["tracked", 0]`) and the path of the field
/// within it (empty when the pull request itself was refused).
fn refused_pull_request_field(error: &Value) -> Option<(Vec<Value>, Vec<Value>)> {
    let refused = error.get("type").and_then(Value::as_str) == Some("FORBIDDEN")
        || error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.starts_with("Resource not accessible by"));
    if !refused {
        return None;
    }
    let path = error.get("path")?.as_array()?;
    let list = path.first()?.as_str()?;
    if !PULL_REQUEST_LISTS.contains(&list) {
        return None;
    }
    // The first index in the path is the pull request's place in its list.
    let index = path.iter().position(Value::is_u64)?;
    Some((path[..=index].to_vec(), path[index + 1..].to_vec()))
}

/// The board selects nothing under `commits` but the head commit's check
/// rollup, so a refusal anywhere in it — the commit node, the commit, the
/// rollup — means the CI cannot be read. The whole selection is replaced by a
/// head commit whose rollup is marked hidden.
fn mark_ci_hidden(data: &mut Value, node: &[Value], field: &[Value]) -> bool {
    if field.first().and_then(Value::as_str) != Some("commits") {
        return false;
    }
    let Some(pull_request) = walk(data, node.iter()).and_then(Value::as_object_mut) else {
        return false;
    };
    pull_request.insert(
        "commits".into(),
        json!({ "nodes": [{ "commit": { "statusCheckRollup": { "hidden": true } } }] }),
    );
    true
}

/// Replace the field named `segment` on `field`'s path with `marker`, when the
/// path passes through it and everything above it is still there. GitHub nulls
/// the nearest nullable field, which for a requested reviewer is that field
/// itself, so an error pointing further down (at a team's `slug`) still marks
/// the field that holds it.
fn mark(data: &mut Value, node: &[Value], field: &[Value], segment: &str, marker: Value) -> bool {
    let Some(at) = field.iter().position(|step| step.as_str() == Some(segment)) else {
        return false;
    };
    let Some(parent) = walk(data, node.iter().chain(&field[..at])) else {
        return false;
    };
    match parent.as_object_mut() {
        Some(object) => {
            object.insert(segment.to_owned(), marker);
            true
        }
        None => false,
    }
}

/// Remove the first `null` on the field's path, so that a list or an optional
/// field falls back to its default instead of failing to parse. A required
/// field GitHub withheld still fails to parse, and says which field it was.
fn clear_null(data: &mut Value, node: &[Value], field: &[Value]) {
    let Some(mut current) = walk(data, node.iter()) else {
        return;
    };
    for step in field {
        let next = match (current, step) {
            (Value::Object(object), Value::String(key)) => {
                if object.get(key).is_some_and(Value::is_null) {
                    object.remove(key);
                    return;
                }
                object.get_mut(key)
            }
            (Value::Array(items), Value::Number(index)) => index
                .as_u64()
                .and_then(|index| items.get_mut(usize::try_from(index).ok()?)),
            _ => None,
        };
        match next {
            Some(next) => current = next,
            None => return,
        }
    }
}

fn walk<'a, 'p>(
    mut current: &'a mut Value,
    path: impl Iterator<Item = &'p Value>,
) -> Option<&'a mut Value> {
    for step in path {
        current = match step {
            Value::String(key) => current.as_object_mut()?.get_mut(key)?,
            Value::Number(index) => {
                let index = usize::try_from(index.as_u64()?).ok()?;
                current.as_array_mut()?.get_mut(index)?
            }
            _ => return None,
        };
        if current.is_null() {
            return None;
        }
    }
    Some(current)
}

/// The same refusal message once, however many times GitHub repeated it.
pub fn unique_messages(messages: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    messages
        .iter()
        .map(String::as_str)
        .filter(|message| seen.insert(*message))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSED: &str = "Resource not accessible by personal access token";

    fn refused(path: Value) -> Value {
        json!({ "type": "FORBIDDEN", "path": path, "message": REFUSED })
    }

    fn pull_request(number: u64) -> Value {
        json!({
            "number": number,
            "reviewRequests": { "totalCount": 1, "nodes": [{ "requestedReviewer": null }] },
            // What a fine-grained token gets: the head commit node itself is null.
            "commits": { "nodes": [null] },
            "labels": null,
        })
    }

    #[test]
    fn a_fine_grained_token_keeps_its_board_and_the_refusals_are_counted_once_per_pr() {
        let mut body = json!({
            "data": { "search": { "nodes": [pull_request(1), pull_request(2)] } },
            "errors": [
                // The shape GitHub returned to a fine-grained token on 2026-09-18.
                refused(json!(["search", "nodes", 0, "commits", "nodes", 0])),
                // A refusal further down the same selection means the same.
                refused(json!(["search", "nodes", 1, "commits", "nodes", 0, "commit", "statusCheckRollup"])),
                refused(json!(["search", "nodes", 1, "reviewRequests", "nodes", 0, "requestedReviewer", "slug"])),
                refused(json!(["search", "nodes", 1, "labels"])),
                refused(json!(["search", "nodes", 1, "labels"])),
            ],
        });

        let gaps = tolerate_access_errors(&mut body);

        assert_eq!(
            gaps,
            AccessGaps {
                ci: 2,
                teams: 1,
                other: 1,
                pull_requests: 0
            }
        );
        assert!(body.get("errors").is_none(), "every refusal was tolerated");
        for node in body["data"]["search"]["nodes"].as_array().unwrap() {
            assert_eq!(
                node.pointer("/commits/nodes/0/commit/statusCheckRollup"),
                Some(&json!({ "hidden": true }))
            );
        }
        let second = &body["data"]["search"]["nodes"][1];
        assert_eq!(
            second.pointer("/reviewRequests/nodes/0/requestedReviewer/slug"),
            Some(&json!(HIDDEN_TEAM))
        );
        assert!(
            second.get("labels").is_none(),
            "a null list falls back to empty"
        );
    }

    #[test]
    fn a_refused_pull_request_is_counted_and_its_error_dropped() {
        let mut body = json!({
            "data": { "tracked": [null], "search": { "nodes": [null] } },
            "errors": [refused(json!(["search", "nodes", 0])), refused(json!(["tracked", 0]))],
        });
        let gaps = tolerate_access_errors(&mut body);
        assert_eq!(gaps.pull_requests, 2);
        assert!(body.get("errors").is_none());
    }

    #[test]
    fn anything_but_a_refused_pull_request_field_is_left_for_the_error_check() {
        let untouched = [
            // No data: the whole query failed.
            json!({ "data": null, "errors": [refused(json!(["search"]))] }),
            // A refused search, not a field of a pull request in it.
            json!({ "data": { "search": null }, "errors": [refused(json!(["search"]))] }),
            // The scoped repository has its own error.
            json!({ "data": { "scopeRepository": null },
                    "errors": [refused(json!(["scopeRepository"]))] }),
            // Other kinds of error inside a pull request.
            json!({ "data": { "search": { "nodes": [{}] } },
                    "errors": [{ "type": "RATE_LIMITED", "path": ["search", "nodes", 0],
                                 "message": "API rate limit exceeded" }] }),
            json!({ "data": { "search": { "nodes": [{}] } },
                    "errors": [{ "path": ["search", "nodes", 0, "title"], "message": "boom" }] }),
        ];
        for original in untouched {
            let mut body = original.clone();
            assert!(tolerate_access_errors(&mut body).is_empty());
            assert_eq!(body, original);
        }
    }

    #[test]
    fn the_other_errors_stay_when_some_are_tolerated() {
        let mut body = json!({
            "data": { "search": { "nodes": [pull_request(1)] } },
            "errors": [
                refused(json!(["search", "nodes", 0, "commits", "nodes", 0])),
                { "type": "INTERNAL", "message": "something else" },
            ],
        });
        let gaps = tolerate_access_errors(&mut body);
        assert_eq!(gaps.ci, 1);
        assert_eq!(
            body["errors"],
            json!([{ "type": "INTERNAL", "message": "something else" }])
        );
    }

    #[test]
    fn a_message_github_repeats_is_reported_once() {
        let repeated = vec![REFUSED.to_owned(), REFUSED.to_owned(), "other".to_owned()];
        assert_eq!(unique_messages(&repeated), vec![REFUSED, "other"]);
    }
}
