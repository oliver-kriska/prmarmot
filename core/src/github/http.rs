//! Direct HTTPS to GitHub, so PR Marmot runs without the `gh` CLI installed.
//!
//! Feature-gated (`http`) on purpose: the iOS app supplies its own
//! `URLSession` transport across the FFI boundary and must not link a second
//! TLS stack, and keeping the dependency optional keeps `prmarmot-core`'s
//! Rust 1.85 floor cheap to hold.
//!
//! Blocking, no async runtime — the "don't pull in a runtime you don't use"
//! guardrail. Callers already run fetches on a background thread.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use super::rate_limit::RateLimitInfo;
use super::{
    graphql_url, normalize_host, rest_url, AuthTransport, GhError, GithubTransport, RestTransport,
    TokenSource,
};

/// GitHub requires a `User-Agent` on every request and answers a missing one
/// with a silent 403 — the classic hand-rolled-client trap.
pub const DEFAULT_USER_AGENT: &str = concat!("prmarmot/", env!("CARGO_PKG_VERSION"));

/// Same ceiling the `gh` subprocess transport uses.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound the response like every other buffer in this project. A board page is
/// a few hundred kilobytes; anything near this is a bug or an attack.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// One HTTPS client for one host. Clone-free: build it once per refresh loop.
pub struct HttpTransport {
    agent: ureq::Agent,
    host: String,
    graphql_url: String,
    token: Option<Arc<dyn TokenSource>>,
}

impl HttpTransport {
    /// An authenticated client for `host` (`github.com` or a GHES hostname).
    pub fn new(host: &str, token: Arc<dyn TokenSource>) -> Self {
        Self {
            agent: agent(DEFAULT_USER_AGENT),
            host: normalize_host(host),
            graphql_url: graphql_url(host),
            token: Some(token),
        }
    }

    /// A client for the device-flow endpoints, which take no token. Keeping
    /// this separate is what breaks the chicken-and-egg between a token source
    /// that refreshes over HTTP and a transport that needs a token.
    pub fn unauthenticated(host: &str) -> Self {
        Self {
            agent: agent(DEFAULT_USER_AGENT),
            host: normalize_host(host),
            graphql_url: graphql_url(host),
            token: None,
        }
    }

    /// Identify as the front end that is actually running (`prmarmot/0.8.1`,
    /// `prmarmot-cli/0.8.1`), not as the core library.
    pub fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.agent = agent(user_agent);
        self
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    fn bearer(&self) -> Result<Option<String>, GhError> {
        match &self.token {
            Some(source) => Ok(Some(source.token()?)),
            None => Ok(None),
        }
    }

    fn graphql_body(
        &self,
        query: &str,
        variables: &[(&str, &str)],
        ids: &[String],
    ) -> Result<Value, GhError> {
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
        Ok(Value::Object(body))
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<Value, GhError> {
        let mut request = self
            .agent
            .post(url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = self.bearer()? {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let payload = serde_json::to_string(body)
            .map_err(|e| GhError::Parse(format!("could not encode the request: {e}")))?;
        finish(request.send(payload.as_str()))
    }
}

fn agent(user_agent: &str) -> ureq::Agent {
    ureq::Agent::config_builder()
        // We read the status ourselves: a 403 carries the rate-limit headers
        // and a 401 the sign-in state, and both are information, not panic.
        .http_status_as_error(false)
        .timeout_global(Some(REQUEST_TIMEOUT))
        .user_agent(user_agent)
        .build()
        .new_agent()
}

impl GithubTransport for HttpTransport {
    fn graphql(&self, query: &str, variables: &[(&str, &str)]) -> Result<Value, GhError> {
        self.graphql_with_ids(query, variables, &[])
    }

    fn graphql_with_ids(
        &self,
        query: &str,
        variables: &[(&str, &str)],
        ids: &[String],
    ) -> Result<Value, GhError> {
        let body = self.graphql_body(query, variables, ids)?;
        self.post_json(&self.graphql_url.clone(), &body)
    }
}

impl AuthTransport for HttpTransport {
    fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<Value, GhError> {
        let body = form_encode(fields);
        let request = self
            .agent
            .post(url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded");
        finish(request.send(body.as_str()))
    }
}

impl RestTransport for HttpTransport {
    fn rest_get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, GhError> {
        let mut url = rest_url(&self.host, path);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&form_encode(query));
        }
        let mut request = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = self.bearer()? {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        finish(request.call())
    }
}

/// Status and body of one response, mapped onto the `GhError` variants the UI
/// already renders.
fn finish(sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, GhError> {
    let mut response = sent.map_err(transport_error)?;
    let status = response.status().as_u16();
    let reset_epoch = header_u64(&response, "x-ratelimit-reset");
    let remaining = header_u64(&response, "x-ratelimit-remaining");
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|e| GhError::Network(format!("could not read the response: {e}")))?;

    match status {
        200..=299 => serde_json::from_str(&text).map_err(|e| {
            GhError::Parse(format!(
                "GitHub returned {status} with a body that is not JSON: {e}"
            ))
        }),
        401 => Err(GhError::NotAuthenticated),
        // A 403 or 429 is a rate limit only when GitHub says so; a 403 for a
        // repository the token cannot see must not look like a rate limit.
        403 | 429 if remaining == Some(0) || mentions_rate_limit(&text) => {
            Err(GhError::RateLimited { reset_epoch })
        }
        403 => Err(GhError::Network(format!(
            "GitHub refused the request (403){}",
            detail(&text)
        ))),
        404 => Err(GhError::Network(format!(
            "GitHub returned 404{}",
            detail(&text)
        ))),
        500..=599 => Err(GhError::Network(format!(
            "GitHub is having trouble ({status}){}",
            detail(&text)
        ))),
        other => Err(GhError::Network(format!(
            "GitHub returned {other}{}",
            detail(&text)
        ))),
    }
}

fn transport_error(error: ureq::Error) -> GhError {
    match error {
        ureq::Error::Timeout(_) => GhError::Network(format!(
            "GitHub did not answer within {}s",
            REQUEST_TIMEOUT.as_secs()
        )),
        other => GhError::Network(other.to_string()),
    }
}

fn header_u64(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<u64> {
    response
        .headers()
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn mentions_rate_limit(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("rate limit") || body.contains("rate_limited")
}

/// First line of an error body, bounded — enough to act on, never a wall.
fn detail(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().to_owned());
    let message: String = message.chars().take(200).collect();
    if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    }
}

/// `application/x-www-form-urlencoded`, hand-rolled so the crate does not grow
/// a URL dependency for five short fields.
fn form_encode(fields: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, value) in fields {
        if !out.is_empty() {
            out.push('&');
        }
        percent_encode(key, &mut out);
        out.push('=');
        percent_encode(value, &mut out);
    }
    out
}

fn percent_encode(text: &str, out: &mut String) {
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
}

/// The live budget from a response body, for callers that want it without
/// going through the board parser.
pub fn rate_limit_of(body: &Value) -> Option<RateLimitInfo> {
    serde_json::from_value(body.pointer("/data/rateLimit")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Pat(&'static str);
    impl TokenSource for Pat {
        fn token(&self) -> Result<String, GhError> {
            Ok(self.0.to_owned())
        }
    }

    #[test]
    fn the_user_agent_is_set_because_github_403s_without_one() {
        assert!(DEFAULT_USER_AGENT.starts_with("prmarmot/"));
    }

    #[test]
    fn graphql_bodies_carry_string_variables_and_a_typed_id_array() {
        let transport = HttpTransport::new("github.com", Arc::new(Pat("ghp_x")));
        let body = transport
            .graphql_body("query($q:String!){}", &[("q", "is:open")], &[])
            .unwrap();
        assert_eq!(body["variables"]["q"], "is:open");
        assert!(body["variables"].get("tracked").is_none());

        let tracked = transport
            .graphql_body("query($q:String!){}", &[("q", "is:open")], &["PR_1".into()])
            .unwrap();
        assert_eq!(tracked["variables"]["tracked"], serde_json::json!(["PR_1"]));
    }

    #[test]
    fn the_endpoint_follows_the_host() {
        assert_eq!(
            HttpTransport::unauthenticated("github.com").graphql_url,
            "https://api.github.com/graphql"
        );
        assert_eq!(
            HttpTransport::unauthenticated("https://ghe.acme.test").graphql_url,
            "https://ghe.acme.test/api/graphql"
        );
    }

    #[test]
    fn form_encoding_escapes_the_grant_type_urn() {
        let encoded = form_encode(&[
            ("client_id", "Iv1.abc"),
            ("grant_type", super::super::device_flow::DEVICE_CODE_GRANT),
        ]);
        assert_eq!(
            encoded,
            "client_id=Iv1.abc&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
        );
        assert_eq!(
            form_encode(&[("scope", "repo read:org")]),
            "scope=repo+read%3Aorg"
        );
    }

    #[test]
    fn error_bodies_are_summarized_and_bounded() {
        assert_eq!(
            detail(r#"{"message":"Bad credentials"}"#),
            ": Bad credentials"
        );
        assert_eq!(detail(""), "");
        let long = detail(&"x".repeat(1000));
        assert_eq!(long.chars().count(), 202);
    }

    #[test]
    fn only_a_declared_rate_limit_reads_as_one() {
        assert!(mentions_rate_limit(
            r#"{"message":"API rate limit exceeded"}"#
        ));
        assert!(!mentions_rate_limit(
            r#"{"message":"Resource not accessible"}"#
        ));
    }
}
