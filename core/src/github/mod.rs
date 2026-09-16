//! GitHub access, behind traits so transport and token source are swappable
//! (v1 = `gh` CLI subprocess; v2 = direct HTTP seeded by `gh auth token`).

pub mod gh_cli;
pub mod query;
pub mod rate_limit;

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
    /// Subprocess / network-level failure (non-zero exit without a parseable body).
    Network(String),
    /// Response body did not match the expected shape.
    Parse(String),
}

impl fmt::Display for GhError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GhError::NotInstalled => {
                write!(
                    f,
                    "gh not found — install it: brew install gh && gh auth login"
                )
            }
            GhError::NotAuthenticated => write!(f, "gh is not authenticated — run: gh auth login"),
            GhError::RateLimited { reset_epoch } => match reset_epoch {
                Some(t) => write!(f, "GitHub rate limited (resets at epoch {t})"),
                None => write!(f, "GitHub rate limited"),
            },
            GhError::GraphqlErrors(msgs) => write!(f, "GraphQL errors: {}", msgs.join("; ")),
            GhError::RepositoryNotFound(repo) => write!(
                f,
                "repository {repo} not found, or the gh account can't access it"
            ),
            GhError::PullRequestNotFound(pr) => write!(
                f,
                "pull request {pr} not found, or the gh account can't access it"
            ),
            GhError::Network(msg) => write!(f, "gh failed: {msg}"),
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

/// Where the API token comes from. The v1 `GhCliTransport` never touches a
/// token (auth lives inside `gh`); the future direct-HTTP transport consumes
/// this trait instead.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> Result<String, GhError>;
}
