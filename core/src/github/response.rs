//! What a GitHub HTTP response means, independently of who made the request.
//!
//! The desktop app and the CLI go through `ureq` ([`super::http`]); the iPad
//! sends the same request with `URLSession` on the Swift side of the FFI
//! boundary. Both then have to answer the same questions — is this a rate
//! limit or a permission problem, is a 403 fatal, what does the body say —
//! and getting that wrong on one side only is exactly the kind of drift this
//! project keeps out of the front ends. So the rules live here, in one pure
//! function with no HTTP client in sight, and both paths call it.

use serde_json::{Map, Value};

use super::GhError;

/// The JSON body of one GraphQL request: the operation, its string variables,
/// and the optional bounded `tracked: [ID!]!` array.
///
/// Shared for the same reason [`classify`] is: the `tracked` variable has to
/// be a typed array rather than a string, and a front end that got that wrong
/// would fail only on the "what became of this PR" path, which is the one
/// nobody exercises by hand.
pub fn graphql_body(query: &str, variables: &[(&str, &str)], ids: &[String]) -> Value {
    let mut vars = Map::new();
    for (key, value) in variables {
        vars.insert((*key).to_owned(), Value::String((*value).to_owned()));
    }
    if !ids.is_empty() {
        vars.insert(
            "tracked".to_owned(),
            Value::Array(ids.iter().cloned().map(Value::String).collect()),
        );
    }
    let mut body = Map::new();
    body.insert("query".to_owned(), Value::String(query.to_owned()));
    body.insert("variables".to_owned(), Value::Object(vars));
    Value::Object(body)
}

/// The parts of a response that decide what it means: the status line and the
/// two rate-limit headers. Everything else is in the body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseMeta {
    pub status: u16,
    /// `x-ratelimit-remaining`, when present and numeric.
    pub remaining: Option<u64>,
    /// `x-ratelimit-reset`, when present and numeric (Unix seconds).
    pub reset_epoch: Option<u64>,
}

impl ResponseMeta {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            ..Default::default()
        }
    }

    /// Fill the rate-limit fields from raw header values, ignoring anything
    /// unparseable — a malformed header must never be the reason a fetch fails.
    pub fn with_headers<'a>(
        mut self,
        headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        for (name, value) in headers {
            let value = value.trim().parse().ok();
            match name.to_ascii_lowercase().as_str() {
                "x-ratelimit-remaining" => self.remaining = value,
                "x-ratelimit-reset" => self.reset_epoch = value,
                _ => {}
            }
        }
        self
    }
}

/// Turn a finished response into the parsed JSON body, or the error it means.
///
/// The wording is the user-facing wording: these strings reach a footer and a
/// terminal, so they say what happened rather than naming a status code alone.
pub fn classify(meta: ResponseMeta, body: &str) -> Result<Value, GhError> {
    let status = meta.status;
    match status {
        200..=299 => serde_json::from_str(body).map_err(|e| {
            GhError::Parse(format!(
                "GitHub returned {status} with a body that is not JSON: {e}"
            ))
        }),
        401 => Err(GhError::NotAuthenticated),
        // A 403 or 429 is a rate limit only when GitHub says so; a 403 for a
        // repository the token cannot see must not look like a rate limit.
        403 | 429 if meta.remaining == Some(0) || mentions_rate_limit(body) => {
            Err(GhError::RateLimited {
                reset_epoch: meta.reset_epoch,
            })
        }
        403 => Err(GhError::Network(format!(
            "GitHub refused the request (403){}",
            detail(body)
        ))),
        404 => Err(GhError::Network(format!(
            "GitHub returned 404{}",
            detail(body)
        ))),
        500..=599 => Err(GhError::Network(format!(
            "GitHub is having trouble ({status}){}",
            detail(body)
        ))),
        other => Err(GhError::Network(format!(
            "GitHub returned {other}{}",
            detail(body)
        ))),
    }
}

fn mentions_rate_limit(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("rate limit") || body.contains("rate_limited")
}

/// GitHub's own `message` from a JSON error body, first line only and
/// bounded — enough to act on, never a wall. Anything else, such as the HTML
/// page a proxy returns with a 502, adds nothing, so the status line stands
/// alone rather than showing markup to the reader.
pub fn detail(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    let message: String = message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(200)
        .collect();
    if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(status: u16) -> ResponseMeta {
        ResponseMeta::new(status)
    }

    #[test]
    fn a_request_body_carries_string_variables_and_a_typed_id_array() {
        let body = graphql_body("query($q:String!){}", &[("q", "is:open")], &[]);
        assert_eq!(body["query"], "query($q:String!){}");
        assert_eq!(body["variables"]["q"], "is:open");
        assert!(body["variables"].get("tracked").is_none());

        let tracked = graphql_body("query{}", &[], &["PR_1".into(), "PR_2".into()]);
        assert_eq!(
            tracked["variables"]["tracked"],
            serde_json::json!(["PR_1", "PR_2"])
        );
    }

    #[test]
    fn a_successful_body_comes_back_parsed() {
        let body = classify(meta(200), r#"{"data":{"search":{}}}"#).unwrap();
        assert!(body.pointer("/data/search").is_some());
    }

    #[test]
    fn success_with_a_non_json_body_is_a_parse_error_naming_the_status() {
        let error = classify(meta(200), "<html>nope</html>").unwrap_err();
        assert!(
            matches!(&error, GhError::Parse(message) if message.contains("200")),
            "{error:?}"
        );
    }

    #[test]
    fn unauthorized_is_the_sign_in_error_and_nothing_else() {
        assert_eq!(
            classify(meta(401), r#"{"message":"Bad credentials"}"#).unwrap_err(),
            GhError::NotAuthenticated
        );
    }

    #[test]
    fn a_rate_limit_needs_evidence_and_carries_the_reset() {
        let headers = [
            ("X-RateLimit-Remaining", "0"),
            ("x-ratelimit-reset", "1750"),
        ];
        let limited = ResponseMeta::new(403).with_headers(headers);
        assert_eq!(
            classify(limited, "{}").unwrap_err(),
            GhError::RateLimited {
                reset_epoch: Some(1750)
            }
        );

        // Same status, no evidence: a permission problem, not a wait.
        let refused = classify(meta(403), r#"{"message":"Resource not accessible"}"#).unwrap_err();
        assert!(
            matches!(&refused, GhError::Network(m) if m.contains("403") && m.contains("not accessible")),
            "{refused:?}"
        );

        // The body alone is enough evidence, as GitHub's secondary limits are.
        assert!(matches!(
            classify(
                meta(429),
                r#"{"message":"You have exceeded a secondary rate limit"}"#
            )
            .unwrap_err(),
            GhError::RateLimited { .. }
        ));
    }

    #[test]
    fn a_malformed_rate_limit_header_is_ignored_rather_than_fatal() {
        let meta = ResponseMeta::new(403).with_headers([
            ("x-ratelimit-remaining", "not a number"),
            ("x-ratelimit-reset", ""),
        ]);
        assert_eq!(meta.remaining, None);
        assert_eq!(meta.reset_epoch, None);
    }

    #[test]
    fn server_trouble_and_anything_unexpected_read_as_network_errors() {
        let error = classify(meta(502), "<html>502 Bad Gateway</html>").unwrap_err();
        assert!(
            matches!(&error, GhError::Network(m) if m.contains("having trouble (502)")),
            "{error:?}"
        );
        let error = classify(meta(418), "{}").unwrap_err();
        assert!(
            matches!(&error, GhError::Network(m) if m.contains("418")),
            "{error:?}"
        );
    }

    #[test]
    fn the_detail_is_githubs_message_bounded() {
        assert_eq!(
            detail(r#"{"message":"Bad credentials"}"#),
            ": Bad credentials"
        );
        assert_eq!(detail("   "), "");
        let long = format!(r#"{{"message":"{}"}}"#, "x".repeat(500));
        assert_eq!(detail(&long).chars().count(), 202);
        assert_eq!(
            detail(r#"{"message":"Server Error\nretry later"}"#),
            ": Server Error"
        );
    }

    #[test]
    fn a_body_that_is_not_githubs_json_message_leaves_the_status_line_alone() {
        let error = classify(meta(502), "<html>upstream is unhappy</html>").unwrap_err();
        assert_eq!(
            error,
            GhError::Network("GitHub is having trouble (502)".into())
        );
        let error = classify(meta(503), "Service Unavailable").unwrap_err();
        assert_eq!(
            error,
            GhError::Network("GitHub is having trouble (503)".into())
        );
        let error = classify(meta(500), r#"{"documentation_url":"x"}"#).unwrap_err();
        assert_eq!(
            error,
            GhError::Network("GitHub is having trouble (500)".into())
        );
    }

    #[test]
    fn githubs_json_message_follows_the_status_line() {
        let error = classify(meta(502), r#"{"message":"Server Error"}"#).unwrap_err();
        assert_eq!(
            error,
            GhError::Network("GitHub is having trouble (502): Server Error".into())
        );
        let error = classify(meta(404), r#"{"message":"Not Found"}"#).unwrap_err();
        assert_eq!(
            error,
            GhError::Network("GitHub returned 404: Not Found".into())
        );
    }
}
