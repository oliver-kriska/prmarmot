//! v1 transport: shell out to `gh api graphql` (the prototype's and gh-dash's
//! model). `gh` owns auth, token refresh, enterprise hosts. Calls block — run
//! them on a background thread/executor, never the UI thread.

use std::collections::HashSet;
use std::io;
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::Value;

use super::{GhError, GithubTransport};

/// A hung `gh` (network black hole) must never freeze the data layer: with
/// no timeout, `syncing` stays true forever and the refresh dedup silently
/// blocks every future fetch including the `r` key. Kill and surface it.
const GRAPHQL_TIMEOUT: Duration = Duration::from_secs(60);
const QUICK_TIMEOUT: Duration = Duration::from_secs(30);
/// Checking for a login reads a local file or the keychain; it should never
/// take long, and a session waits on it before the first fetch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_DISCOVERED_REPOS: usize = 1_000;
const REPOS_PER_PAGE: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDiscovery {
    pub repos: Vec<String>,
    pub truncated: bool,
}

/// What `gh` may print before the rest is dropped: the HTTP transport's
/// response cap, far above the largest board.
const MAX_GH_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// `Command::output()` with a deadline and a cap on the output.
fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, GhError> {
    run_with_timeout(cmd, timeout, true)
}

/// [`output_with_timeout`]; without `capture_stdout` the child's stdout goes
/// nowhere and `Output::stdout` is empty.
fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
    capture_stdout: bool,
) -> Result<Output, GhError> {
    match crate::process::run_bounded(cmd, timeout, MAX_GH_OUTPUT_BYTES, capture_stdout) {
        Ok(Some(output)) => Ok(output),
        Ok(None) => Err(GhError::Timeout(format!(
            "gh timed out after {}s — killed",
            timeout.as_secs()
        ))),
        Err(error) => Err(classify_spawn_error(&error)),
    }
}

/// Locate `gh`. Apps launched from Spotlight/Finder inherit a minimal PATH
/// (`/usr/bin:/bin`) that misses Homebrew, so a bare "gh" only works from a
/// terminal — fall back to the standard install locations.
pub fn resolve_gh_path() -> String {
    if which_on_path("gh") {
        return "gh".into();
    }
    for candidate in [
        "/opt/homebrew/bin/gh", // macOS arm64 Homebrew
        "/usr/local/bin/gh",    // macOS x86_64 Homebrew / manual installs
        "/home/linuxbrew/.linuxbrew/bin/gh",
    ] {
        if std::path::Path::new(candidate).is_file() {
            return candidate.into();
        }
    }
    "gh".into() // let spawn fail with NotInstalled
}

fn which_on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

pub struct GhCliTransport {
    gh_path: String,
}

impl GhCliTransport {
    pub fn new() -> Self {
        Self {
            gh_path: resolve_gh_path(),
        }
    }
}

impl Default for GhCliTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl GithubTransport for GhCliTransport {
    fn graphql(&self, query: &str, variables: &[(&str, &str)]) -> Result<Value, GhError> {
        self.graphql_with_ids(query, variables, &[])
    }

    fn graphql_with_ids(
        &self,
        query: &str,
        variables: &[(&str, &str)],
        ids: &[String],
    ) -> Result<Value, GhError> {
        let mut cmd = Command::new(&self.gh_path);
        cmd.args(["api", "graphql", "-f"])
            .arg(format!("query={query}"));
        for (k, v) in variables {
            cmd.arg("-f").arg(format!("{k}={v}"));
        }
        for id in ids {
            cmd.arg("-F").arg(format!("tracked[]={id}"));
        }
        let out = output_with_timeout(&mut cmd, GRAPHQL_TIMEOUT)?;
        graphql_outcome(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        )
    }
}

/// What one `gh api graphql` run answered. gh prints the response body to
/// stdout even when it exits non-zero, and that body is one of two things:
/// a GraphQL answer carrying `errors[]` (kept, so the caller reads them), or
/// an HTTP failure's own body, `{"message": …}` (a 502 when GitHub gave up on
/// the query, 401 bad credentials, a 403 rate limit). The second is no
/// GraphQL answer; it is classified from stderr, where gh names the message
/// and `(HTTP <status>)`, so a timeout still reads as one.
fn graphql_outcome(success: bool, stdout: &str, stderr: &str) -> Result<Value, GhError> {
    if let Ok(body) = serde_json::from_str::<Value>(stdout.trim()) {
        if success || body.get("data").is_some() || body.get("errors").is_some() {
            return Ok(body);
        }
        if stderr.trim().is_empty() {
            let message = body.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(classify_failure(message));
        }
        return Err(classify_failure(stderr));
    }
    if !success {
        return Err(classify_failure(stderr));
    }
    Err(GhError::Parse(format!(
        "gh returned non-JSON output: {}",
        stdout.chars().take(200).collect::<String>()
    )))
}

fn classify_spawn_error(e: &io::Error) -> GhError {
    if e.kind() == io::ErrorKind::NotFound {
        GhError::NotInstalled
    } else {
        GhError::Network(e.to_string())
    }
}

fn classify_failure(stderr: &str) -> GhError {
    let s = stderr.to_lowercase();
    if s.contains("gh auth login")
        || s.contains("not logged in")
        || s.contains("authentication")
        || s.contains("bad credentials")
        || s.contains("(http 401)")
    {
        GhError::NotAuthenticated
    } else if s.contains("rate limit") || s.contains("rate_limited") {
        // `gh api` prints no response headers on this path, so there is no
        // reset or retry-after to carry: the caller waits the one-minute floor.
        GhError::RateLimited {
            reset_epoch: None,
            retry_after_secs: None,
        }
    } else {
        let message: String = stderr.trim().chars().take(300).collect();
        match http_status(stderr) {
            Some(status) => GhError::Http { status, message },
            None => GhError::Network(message),
        }
    }
}

/// The status in gh's `HTTP 502` / `(HTTP 502)`, read once here so nothing
/// downstream parses the sentence again.
fn http_status(stderr: &str) -> Option<u16> {
    stderr.split("HTTP ").skip(1).find_map(|rest| {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (digits.len() == 3).then(|| digits.parse().ok()).flatten()
    })
}

/// Whether `gh` holds a login for `host`. `gh auth token` reads it locally, so
/// this makes no network call, and its output is discarded, so the token never
/// enters this process. `NotInstalled` when there is no `gh` at all.
pub fn has_login(host: &str) -> Result<bool, GhError> {
    let host = super::normalize_host(host);
    let out = run_with_timeout(
        Command::new(resolve_gh_path()).args(["auth", "token", "--hostname", &host]),
        PROBE_TIMEOUT,
        false,
    )?;
    Ok(out.status.success())
}

/// The authenticated user's login (resolves what the prototype calls `@me`).
pub fn current_login() -> Result<String, GhError> {
    run_gh_line(Command::new(resolve_gh_path()).args(["api", "user", "--jq", ".login"]))
}

/// [`current_login`] answered from `gh`'s response cache when it asked in the
/// last ten minutes. The cache key includes the token, so `gh auth switch` or
/// a new login asks GitHub again.
pub fn current_login_cached() -> Result<String, GhError> {
    run_gh_line(
        Command::new(resolve_gh_path()).args(["api", "user", "--cache", "10m", "--jq", ".login"]),
    )
}

/// Every repository affiliation visible to the authenticated user, newest
/// activity first. Unlike `gh repo list`, this includes collaborations and
/// organization membership. Pagination and the memory bound are explicit.
pub fn list_repos() -> Result<RepoDiscovery, GhError> {
    let gh_path = resolve_gh_path();
    discover_repos_with(|page| fetch_repo_page(&gh_path, page))
}

/// Injectable pagination core used by tests and alternative transports.
pub fn discover_repos_with<F>(mut fetch_page: F) -> Result<RepoDiscovery, GhError>
where
    F: FnMut(usize) -> Result<Value, GhError>,
{
    let mut repos = Vec::new();
    let mut seen = HashSet::new();
    // Bound calls as well as memory: repos can move between pages as they
    // receive pushes, so repeated pages must not keep discovery alive forever.
    for page in 1..=MAX_DISCOVERED_REPOS.div_ceil(REPOS_PER_PAGE) {
        let value = fetch_page(page)?;
        let items = value
            .as_array()
            .ok_or_else(|| GhError::Parse(format!("user/repos page {page} was not an array")))?;
        for item in items {
            let name = item
                .get("full_name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GhError::Parse(format!("user/repos page {page} item missing full_name"))
                })?;
            if seen.insert(name.to_lowercase()) {
                repos.push(name.to_string());
                if repos.len() == MAX_DISCOVERED_REPOS {
                    return Ok(RepoDiscovery {
                        repos,
                        truncated: items.len() == REPOS_PER_PAGE,
                    });
                }
            }
        }
        if items.len() < REPOS_PER_PAGE {
            return Ok(RepoDiscovery {
                repos,
                truncated: false,
            });
        }
    }
    Ok(RepoDiscovery {
        repos,
        truncated: true,
    })
}

fn fetch_repo_page(gh_path: &str, page: usize) -> Result<Value, GhError> {
    let out = output_with_timeout(
        Command::new(gh_path).args([
            "api",
            "--method",
            "GET",
            "user/repos",
            "-f",
            "affiliation=owner,collaborator,organization_member",
            "-f",
            "per_page=100",
            "-f",
            "sort=pushed",
            "-f",
            &format!("page={page}"),
        ]),
        QUICK_TIMEOUT,
    )?;
    if !out.status.success() {
        return Err(classify_failure(&String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| GhError::Parse(format!("invalid user/repos page {page}: {e}")))
}

fn run_gh_line(cmd: &mut Command) -> Result<String, GhError> {
    let out = output_with_timeout(cmd, QUICK_TIMEOUT)?;
    if !out.status.success() {
        return Err(classify_failure(&String::from_utf8_lossy(&out.stderr)));
    }
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if line.is_empty() {
        return Err(GhError::Parse("empty gh output".into()));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_http_failure_body_is_classified_from_stderr_not_returned_as_an_answer() {
        // gh's real shape for a 404: the JSON body on stdout, exit 1, and the
        // message with its status on stderr.
        let timeout = graphql_outcome(
            false,
            r#"{"message":"We couldn't respond to your request in time. Sorry about that."}"#,
            "gh: We couldn't respond to your request in time. Sorry about that. (HTTP 502)\n",
        )
        .unwrap_err();
        assert!(
            matches!(timeout, GhError::Http { status: 502, .. }),
            "{timeout:?}"
        );
        assert!(timeout.is_query_timeout(), "{timeout:?}");

        let bad_login = graphql_outcome(
            false,
            r#"{"message":"Bad credentials","status":"401"}"#,
            "gh: Bad credentials (HTTP 401)",
        );
        assert_eq!(bad_login, Err(GhError::NotAuthenticated));

        let limited = graphql_outcome(
            false,
            r#"{"message":"API rate limit exceeded for user ID 1."}"#,
            "gh: API rate limit exceeded for user ID 1. (HTTP 403)",
        );
        assert!(matches!(limited, Err(GhError::RateLimited { .. })));

        // No stderr: the body's own message is what there is to go on.
        let quiet = graphql_outcome(false, r#"{"message":"Bad credentials"}"#, "");
        assert_eq!(quiet, Err(GhError::NotAuthenticated));
    }

    #[test]
    fn the_status_is_read_from_ghs_sentence_once() {
        assert_eq!(http_status("gh: Server Error (HTTP 504)"), Some(504));
        assert_eq!(http_status("gh: HTTP 502: Bad Gateway"), Some(502));
        assert_eq!(http_status("HTTP 5021 HTTP 404"), Some(404));
        assert_eq!(http_status("gh: connection refused"), None);
    }

    #[test]
    fn a_graphql_answer_is_returned_whatever_the_exit_code() {
        let errors = r#"{"data":null,"errors":[{"message":"Could not resolve to a Repository"}]}"#;
        assert!(graphql_outcome(false, errors, "gh: Could not resolve").is_ok());
        assert!(graphql_outcome(true, r#"{"data":{}}"#, "").is_ok());
        assert!(matches!(
            graphql_outcome(true, "not json", ""),
            Err(GhError::Parse(_))
        ));
        assert!(matches!(
            graphql_outcome(false, "", "gh: connection refused"),
            Err(GhError::Network(_))
        ));
    }

    #[test]
    fn repo_discovery_paginates_and_deduplicates() {
        let first: Vec<Value> = (0..REPOS_PER_PAGE)
            .map(|n| json!({"full_name": format!("acme/repo-{n}")}))
            .collect();
        let second = json!([
            {"full_name": "acme/repo-0"},
            {"full_name": "other/shared"}
        ]);
        let result = discover_repos_with(|page| match page {
            1 => Ok(Value::Array(first.clone())),
            2 => Ok(second.clone()),
            _ => panic!("unexpected page"),
        })
        .unwrap();
        assert_eq!(result.repos.len(), 101);
        assert_eq!(result.repos.last().unwrap(), "other/shared");
        assert!(!result.truncated);
    }

    #[test]
    fn repo_discovery_propagates_page_error() {
        let err = discover_repos_with(|_| Err(GhError::Network("offline".into()))).unwrap_err();
        assert_eq!(err, GhError::Network("offline".into()));
    }

    #[test]
    fn repo_discovery_reports_its_hard_bound() {
        let result = discover_repos_with(|page| {
            Ok(Value::Array(
                (0..REPOS_PER_PAGE)
                    .map(|n| json!({"full_name": format!("acme/{page}-{n}")}))
                    .collect(),
            ))
        })
        .unwrap();
        assert_eq!(result.repos.len(), MAX_DISCOVERED_REPOS);
        assert!(result.truncated);
    }

    #[test]
    fn repeated_full_pages_cannot_loop_forever() {
        let mut calls = 0;
        let result = discover_repos_with(|_| {
            calls += 1;
            Ok(json!(vec![
                json!({"full_name": "acme/repo"});
                REPOS_PER_PAGE
            ]))
        })
        .unwrap();
        assert_eq!(calls, 10);
        assert_eq!(result.repos, vec!["acme/repo"]);
        assert!(result.truncated);
    }
}
