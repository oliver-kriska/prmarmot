//! GitHub access, behind traits so transport and token source are swappable:
//! the `gh` CLI subprocess, direct HTTP from this crate, or — on iOS — a
//! foreign implementation supplied by Swift over the FFI boundary.

pub mod access;
pub mod device_flow;
pub mod gh_cli;
#[cfg(feature = "http")]
pub mod http;
pub mod query;
pub mod rate_limit;
pub mod response;

use std::fmt;

/// Typed `gh` / GraphQL failure modes. Each maps to a specific UI state
/// (see the data-layer research doc §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhError {
    /// `gh` binary not found on PATH.
    NotInstalled,
    /// `gh` present but not authenticated (or token expired).
    NotAuthenticated,
    /// GraphQL RATE_LIMITED error or HTTP 403 rate-limit response.
    RateLimited { reset_epoch: Option<u64> },
    /// GraphQL `errors[]` present without usable data.
    GraphqlErrors(Vec<String>),
    /// The scoped `owner/name` does not exist or the `gh` account cannot see it
    /// (GitHub search would otherwise just return nothing).
    RepositoryNotFound(String),
    /// `owner/name#number` does not exist or the `gh` account cannot see it.
    PullRequestNotFound(String),
    /// All open was asked for across all repositories; it covers one.
    NeedsRepository,
    /// Subprocess / network-level failure (non-zero exit without a parseable body).
    Network(String),
    /// Response body did not match the expected shape.
    Parse(String),
}

impl fmt::Display for GhError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GhError::NotInstalled => write!(
                f,
                "no GitHub sign-in found — run `prmarmot-cli auth login`, or install the GitHub CLI and run `gh auth login`"
            ),
            GhError::NotAuthenticated => write!(
                f,
                "not signed in to GitHub — run `gh auth login`, or `prmarmot-cli auth login`"
            ),
            GhError::RateLimited { reset_epoch } => match reset_epoch {
                Some(t) => write!(f, "GitHub rate limited (resets at epoch {t})"),
                None => write!(f, "GitHub rate limited"),
            },
            GhError::GraphqlErrors(msgs) => {
                write!(
                    f,
                    "GraphQL errors: {}",
                    access::unique_messages(msgs).join("; ")
                )
            }
            GhError::RepositoryNotFound(repo) => write!(
                f,
                "repository {repo} not found, or the gh account can't access it"
            ),
            GhError::PullRequestNotFound(pr) => write!(
                f,
                "pull request {pr} not found, or the gh account can't access it"
            ),
            GhError::NeedsRepository => {
                write!(f, "{}", crate::status::all_open_needs_repository())
            }
            // Neutral wording: this variant now also carries direct-HTTP
            // failures, where naming `gh` would send people the wrong way.
            GhError::Network(msg) => write!(f, "{msg}"),
            GhError::Parse(msg) => write!(f, "unexpected GitHub response: {msg}"),
        }
    }
}

impl std::error::Error for GhError {}

/// One GraphQL call. Implementations must be callable from a background
/// thread; blocking inside is acceptable (the caller runs it off the UI thread).
pub trait GithubTransport: Send + Sync {
    /// Execute `query` with string variables (the `-f key=value` model of
    /// `gh api graphql`). Returns the full response body as JSON.
    fn graphql(
        &self,
        query: &str,
        variables: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError>;

    /// Execute one operation with an additional bounded `[ID!]!` variable.
    /// The default keeps test/custom transports source-compatible when no ids
    /// are requested; production `gh` uses typed `-F tracked[]=...` fields.
    fn graphql_with_ids(
        &self,
        query: &str,
        variables: &[(&str, &str)],
        ids: &[String],
    ) -> Result<serde_json::Value, GhError> {
        if ids.is_empty() {
            self.graphql(query, variables)
        } else {
            Err(GhError::Parse(
                "transport does not support batched node ids".into(),
            ))
        }
    }
}

/// Where the API token comes from. `GhCliTransport` never touches a token
/// (auth lives inside `gh`); the direct-HTTP transport consumes this trait
/// instead, as does the Swift transport on iOS.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> Result<String, GhError>;
}

/// A fixed token: a pasted PAT, or `gh auth token` read once.
pub struct StaticToken(pub String);

impl TokenSource for StaticToken {
    fn token(&self) -> Result<String, GhError> {
        if self.0.trim().is_empty() {
            return Err(GhError::NotAuthenticated);
        }
        Ok(self.0.clone())
    }
}

/// The unauthenticated form POSTs of the OAuth Device Flow (requesting a code,
/// polling for a token, refreshing one). Separate from [`GithubTransport`]
/// because these endpoints take no token — which is exactly why the device
/// flow needs no server and no client secret.
pub trait AuthTransport: Send + Sync {
    /// POST `url` with an `application/x-www-form-urlencoded` body and
    /// `Accept: application/json`; return the parsed response body.
    fn post_form(&self, url: &str, fields: &[(&str, &str)]) -> Result<serde_json::Value, GhError>;
}

/// The handful of REST reads PR Marmot makes next to GraphQL (repository
/// discovery). Implemented by the direct-HTTP transport; the `gh` path has its
/// own subprocess equivalents in [`gh_cli`].
pub trait RestTransport: Send + Sync {
    /// GET `path` (relative to the host's REST root, e.g. `user/repos`) with
    /// query parameters; return the parsed response body.
    fn rest_get(&self, path: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, GhError>;
}

/// `github.com` for a blank, `api.`-prefixed, or scheme-prefixed host, so a
/// config file and an `GH_HOST` both land on the same canonical name.
pub fn normalize_host(host: &str) -> String {
    let host = host.trim();
    let host = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);
    let host = host.split('/').next().unwrap_or(host).trim_end_matches('.');
    let host = host.strip_prefix("api.").unwrap_or(host);
    if host.is_empty() {
        "github.com".to_owned()
    } else {
        host.to_ascii_lowercase()
    }
}

/// The GraphQL endpoint: `api.github.com` on github.com, `HOST/api/graphql` on
/// a GitHub Enterprise Server instance.
pub fn graphql_url(host: &str) -> String {
    match normalize_host(host).as_str() {
        "github.com" => "https://api.github.com/graphql".to_owned(),
        host => format!("https://{host}/api/graphql"),
    }
}

/// The REST root: `api.github.com` on github.com, `HOST/api/v3` on GHES.
pub fn rest_url(host: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    match normalize_host(host).as_str() {
        "github.com" => format!("https://api.github.com/{path}"),
        host => format!("https://{host}/api/v3/{path}"),
    }
}

/// The signed-in account's login, over whichever transport is configured.
/// The `gh` path keeps using [`gh_cli::current_login`] so it spends REST
/// budget rather than a GraphQL point, exactly as it did before.
pub fn viewer_login(transport: &dyn GithubTransport) -> Result<String, GhError> {
    let body = transport.graphql("query{ viewer { login } }", &[])?;
    if let Some(login) = body
        .pointer("/data/viewer/login")
        .and_then(serde_json::Value::as_str)
        .filter(|login| !login.is_empty())
    {
        return Ok(login.to_owned());
    }
    if let Some(errors) = body.get("errors").and_then(serde_json::Value::as_array) {
        let messages: Vec<String> = errors
            .iter()
            .map(|error| {
                error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown error")
                    .to_owned()
            })
            .collect();
        return Err(GhError::GraphqlErrors(messages));
    }
    Err(GhError::Parse("no viewer login in the response".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_normalize_to_one_spelling() {
        for spelling in [
            "github.com",
            "GitHub.com",
            "api.github.com",
            "https://github.com",
            "https://api.github.com/",
            "  github.com  ",
            "",
        ] {
            assert_eq!(normalize_host(spelling), "github.com", "{spelling:?}");
        }
        assert_eq!(normalize_host("ghe.acme.test"), "ghe.acme.test");
        assert_eq!(normalize_host("https://ghe.acme.test/"), "ghe.acme.test");
    }

    #[test]
    fn a_missing_github_cli_is_not_presented_as_the_only_way_in() {
        // Signing in directly works without gh, so both messages offer it.
        for error in [GhError::NotInstalled, GhError::NotAuthenticated] {
            let text = error.to_string();
            assert!(text.contains("prmarmot-cli auth login"), "{text}");
            assert!(text.contains("gh auth login"), "{text}");
            assert!(!text.contains("brew install gh"), "{text}");
        }
    }

    #[test]
    fn endpoints_follow_the_host() {
        assert_eq!(graphql_url("github.com"), "https://api.github.com/graphql");
        assert_eq!(
            graphql_url("ghe.acme.test"),
            "https://ghe.acme.test/api/graphql"
        );
        assert_eq!(
            rest_url("github.com", "user/repos"),
            "https://api.github.com/user/repos"
        );
        assert_eq!(
            rest_url("ghe.acme.test", "/user/repos"),
            "https://ghe.acme.test/api/v3/user/repos"
        );
    }

    #[test]
    fn viewer_login_reads_the_graphql_shape_and_reports_errors() {
        struct Fixed(serde_json::Value);
        impl GithubTransport for Fixed {
            fn graphql(
                &self,
                _query: &str,
                _variables: &[(&str, &str)],
            ) -> Result<serde_json::Value, GhError> {
                Ok(self.0.clone())
            }
        }
        let ok = Fixed(serde_json::json!({"data": {"viewer": {"login": "octo"}}}));
        assert_eq!(viewer_login(&ok).unwrap(), "octo");

        let bad = Fixed(serde_json::json!({"errors": [{"message": "Bad credentials"}]}));
        assert_eq!(
            viewer_login(&bad).unwrap_err(),
            GhError::GraphqlErrors(vec!["Bad credentials".into()])
        );
    }
}
