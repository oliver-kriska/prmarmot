//! Framework-independent update checking and Homebrew upgrade helper protocol.
//!
//! This module is deliberately blocking. Call release checks and helper work on
//! a background executor; no GPUI type or application identity lives here.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use prmarmot_core::github::http::HttpTransport;
use prmarmot_core::github::RestTransport;
use serde::{Deserialize, Serialize};

pub const AUTOMATIC_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const RELEASE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
pub const UPGRADE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub const REOPEN_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_DIAGNOSTIC_BYTES: usize = 1_024;
pub const MAX_STATE_BYTES: u64 = 16 * 1_024;
const HELPER_POLL_INTERVAL: Duration = Duration::from_millis(200);
const PARENT_EXIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_CASK_VERSIONS: usize = 64;
const MAX_APPS_PER_CASK_VERSION: usize = 16;
const MAX_COMMAND_OUTPUT_BYTES: usize = MAX_DIAGNOSTIC_BYTES * 4;
const SYSTEM_KILL_PATH: &str = "/bin/kill";
pub const UPDATE_HELPER_FLAG: &str = "--update-helper";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StableVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl StableVersion {
    /// Parse a stable semantic-version triplet. A release tag may have one
    /// leading `v`; prerelease/build metadata and leading zeroes are rejected.
    pub fn parse(value: &str) -> Result<Self, UpdateError> {
        let value = value.strip_prefix('v').unwrap_or(value);
        let mut parts = value.split('.');
        let major = parse_version_part(parts.next(), value)?;
        let minor = parse_version_part(parts.next(), value)?;
        let patch = parse_version_part(parts.next(), value)?;
        if parts.next().is_some() {
            return Err(UpdateError::InvalidVersion(value.to_owned()));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for StableVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_part(part: Option<&str>, whole: &str) -> Result<u64, UpdateError> {
    let part = part.ok_or_else(|| UpdateError::InvalidVersion(whole.to_owned()))?;
    if part.is_empty()
        || !part.bytes().all(|byte| byte.is_ascii_digit())
        || (part.len() > 1 && part.starts_with('0'))
    {
        return Err(UpdateError::InvalidVersion(whole.to_owned()));
    }
    part.parse()
        .map_err(|_| UpdateError::InvalidVersion(whole.to_owned()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    github_repo: String,
}

impl ReleaseIdentity {
    pub fn new(github_repo: impl Into<String>) -> Result<Self, UpdateError> {
        let github_repo = github_repo.into();
        let mut parts = github_repo.split('/');
        let valid_part = |part: Option<&str>| {
            part.is_some_and(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            })
        };
        if !valid_part(parts.next()) || !valid_part(parts.next()) || parts.next().is_some() {
            return Err(UpdateError::InvalidIdentity(github_repo));
        }
        Ok(Self { github_repo })
    }

    pub fn github_repo(&self) -> &str {
        &self.github_repo
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: StableVersion,
    pub tag: String,
    pub page_url: String,
}

pub trait ReleaseSource: Send + Sync {
    fn latest_stable(&self, identity: &ReleaseIdentity) -> Result<ReleaseInfo, UpdateError>;
}

#[derive(Debug, Clone)]
pub struct GhReleaseSource {
    gh_path: PathBuf,
    timeout: Duration,
}

impl GhReleaseSource {
    pub fn new(gh_path: impl Into<PathBuf>) -> Self {
        Self {
            gh_path: gh_path.into(),
            timeout: RELEASE_CHECK_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl ReleaseSource for GhReleaseSource {
    fn latest_stable(&self, identity: &ReleaseIdentity) -> Result<ReleaseInfo, UpdateError> {
        // `releases/latest` excludes drafts and prereleases. Keep both flags in
        // the response contract as a second line of defence against API drift.
        let endpoint = format!("repos/{}/releases/latest", identity.github_repo());
        let output = run_output_with_timeout(
            Command::new(&self.gh_path).args([
                "api",
                "--method",
                "GET",
                &endpoint,
                "--jq",
                r#"[.tag_name,.html_url,(.draft|tostring),(.prerelease|tostring)]|@tsv"#,
            ]),
            self.timeout,
        )?;
        if !output.status.success() {
            return Err(UpdateError::CommandFailed(bounded_text(&output.stderr)));
        }
        if output.stdout.len() > MAX_DIAGNOSTIC_BYTES * 4 {
            return Err(UpdateError::InvalidRelease("response was too large".into()));
        }
        parse_release_line(&String::from_utf8_lossy(&output.stdout), identity)
    }
}

/// The release check without the GitHub CLI: one unauthenticated REST GET.
/// GitHub allows 60 of those an hour per address and the check runs at most
/// once a day, so no token is needed.
pub struct HttpReleaseSource<T> {
    rest: T,
}

impl HttpReleaseSource<HttpTransport> {
    pub fn github(user_agent: &str) -> Self {
        Self::new(HttpTransport::unauthenticated("github.com").with_user_agent(user_agent))
    }
}

impl<T: RestTransport> HttpReleaseSource<T> {
    pub fn new(rest: T) -> Self {
        Self { rest }
    }
}

impl<T: RestTransport> ReleaseSource for HttpReleaseSource<T> {
    fn latest_stable(&self, identity: &ReleaseIdentity) -> Result<ReleaseInfo, UpdateError> {
        let path = format!("repos/{}/releases/latest", identity.github_repo());
        let body = self
            .rest
            .rest_get(&path, &[])
            .map_err(|error| UpdateError::Request(bounded_string(error.to_string())))?;
        // The four fields the `gh` path asks for, checked by the same parser.
        let text = |name: &str| body.get(name).and_then(serde_json::Value::as_str);
        let flag = |name: &str| body.get(name).and_then(serde_json::Value::as_bool);
        let (Some(tag), Some(url), Some(draft), Some(prerelease)) = (
            text("tag_name"),
            text("html_url"),
            flag("draft"),
            flag("prerelease"),
        ) else {
            return Err(UpdateError::InvalidRelease(
                "the release is missing its tag, page, or flags".into(),
            ));
        };
        if tag.contains(['\t', '\n', '\r']) || url.contains(['\t', '\n', '\r']) {
            return Err(UpdateError::InvalidRelease(
                "expected one release record".into(),
            ));
        }
        parse_release_line(&format!("{tag}\t{url}\t{draft}\t{prerelease}"), identity)
    }
}

/// Ask `first`, and `then` when it can't answer. The app puts the GitHub CLI
/// first and GitHub directly second, so an install without `gh`, or with `gh`
/// signed out, still hears about releases.
pub struct FallbackReleaseSource<A, B> {
    first: A,
    then: B,
}

impl<A, B> FallbackReleaseSource<A, B> {
    pub fn new(first: A, then: B) -> Self {
        Self { first, then }
    }
}

impl<A: ReleaseSource, B: ReleaseSource> ReleaseSource for FallbackReleaseSource<A, B> {
    fn latest_stable(&self, identity: &ReleaseIdentity) -> Result<ReleaseInfo, UpdateError> {
        self.first
            .latest_stable(identity)
            .or_else(|_| self.then.latest_stable(identity))
    }
}

fn parse_release_line(
    output: &str,
    identity: &ReleaseIdentity,
) -> Result<ReleaseInfo, UpdateError> {
    let line = output.trim_end_matches(['\r', '\n']);
    if line.contains('\n') || line.contains('\r') {
        return Err(UpdateError::InvalidRelease(
            "expected one release record".into(),
        ));
    }
    let fields: Vec<_> = line.split('\t').collect();
    if fields.len() != 4 || fields[2] != "false" || fields[3] != "false" {
        return Err(UpdateError::InvalidRelease(
            "latest release was draft, prerelease, or malformed".into(),
        ));
    }
    let tag = fields[0];
    let version = StableVersion::parse(tag)?;
    let expected = format!(
        "https://github.com/{}/releases/tag/{tag}",
        identity.github_repo()
    );
    if fields[1] != expected {
        return Err(UpdateError::InvalidRelease(
            "release page did not match the configured repository and tag".into(),
        ));
    }
    Ok(ReleaseInfo {
        version,
        tag: tag.to_owned(),
        page_url: fields[1].to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckResult {
    Available {
        version: StableVersion,
        tag: String,
        page_url: String,
    },
    UpToDate {
        latest: StableVersion,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    pub last_attempt_unix: Option<i64>,
    pub last_result: Option<CheckResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomaticCheck {
    Disabled,
    NotDue(Option<CheckResult>),
    InProgressElsewhere,
    Completed {
        result: CheckResult,
        /// A completed network result remains useful if only the final state
        /// write failed. The attempt timestamp was durably claimed first.
        persistence_error: Option<String>,
    },
}

pub struct UpdateChecker<S> {
    state_path: PathBuf,
    source: S,
}

impl<S: ReleaseSource> UpdateChecker<S> {
    pub fn new(state_path: impl Into<PathBuf>, source: S) -> Self {
        Self {
            state_path: state_path.into(),
            source,
        }
    }

    /// Claim and perform one automatic check. The attempt timestamp is written
    /// atomically before network I/O, so failures and restarts are rate-limited.
    /// A future timestamp (clock rollback) suppresses checks until a full day
    /// after that timestamp rather than accidentally issuing another request.
    pub fn automatic_check(
        &self,
        enabled: bool,
        now_unix: i64,
        current_version: &str,
        identity: &ReleaseIdentity,
    ) -> Result<AutomaticCheck, UpdateError> {
        if !enabled {
            return Ok(AutomaticCheck::Disabled);
        }
        let current = StableVersion::parse(current_version)?;
        let _lock = match FileLock::acquire(check_lock_path(&self.state_path))? {
            Some(lock) => lock,
            None => return Ok(AutomaticCheck::InProgressElsewhere),
        };
        let mut state = load_update_state(&self.state_path)?;
        if !automatic_check_due(state.last_attempt_unix, now_unix) {
            return Ok(AutomaticCheck::NotDue(
                state
                    .last_result
                    .map(|result| recompare_cached_result(result, current)),
            ));
        }

        state.last_attempt_unix = Some(now_unix);
        save_toml_atomic(&self.state_path, &state)?;

        let result = match self.source.latest_stable(identity) {
            Ok(release) if release.version > current => CheckResult::Available {
                version: release.version,
                tag: bounded_string(release.tag),
                page_url: bounded_string(release.page_url),
            },
            Ok(release) => CheckResult::UpToDate {
                latest: release.version,
            },
            Err(error) => CheckResult::Failed {
                message: bounded_string(error.to_string()),
            },
        };
        state.last_result = Some(result.clone());
        let persistence_error = save_toml_atomic(&self.state_path, &state)
            .err()
            .map(|error| bounded_string(error.to_string()));
        Ok(AutomaticCheck::Completed {
            result,
            persistence_error,
        })
    }
}

fn recompare_cached_result(result: CheckResult, current: StableVersion) -> CheckResult {
    match result {
        CheckResult::Available {
            version,
            tag,
            page_url,
        } if version > current => CheckResult::Available {
            version,
            tag,
            page_url,
        },
        CheckResult::Available { version, .. } => CheckResult::UpToDate { latest: version },
        other => other,
    }
}

pub fn automatic_check_due(last_attempt_unix: Option<i64>, now_unix: i64) -> bool {
    match last_attempt_unix {
        None => true,
        Some(last) if now_unix < last => false,
        Some(last) => now_unix.saturating_sub(last) >= AUTOMATIC_CHECK_INTERVAL.as_secs() as i64,
    }
}

pub fn load_update_state(path: &Path) -> Result<UpdateState, UpdateError> {
    match read_bounded(path)? {
        Some(contents) => toml::from_str(&contents)
            .map_err(|error| UpdateError::State(format!("invalid state: {error}"))),
        None => Ok(UpdateState::default()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChannel {
    Homebrew {
        brew_path: PathBuf,
        cask_root: PathBuf,
    },
    Direct,
}

/// Locate Homebrew from PATH or the same standard prefixes used for `gh`.
pub fn resolve_brew_path() -> PathBuf {
    resolve_program(
        "brew",
        &[
            "/opt/homebrew/bin/brew",
            "/usr/local/bin/brew",
            "/home/linuxbrew/.linuxbrew/bin/brew",
        ],
    )
}

pub fn detect_current_install_channel(
    cask_token: &str,
    runner: &dyn CommandRunner,
) -> Result<InstallChannel, UpdateError> {
    let current_exe =
        std::env::current_exe().map_err(|error| UpdateError::Io(error.to_string()))?;
    detect_install_channel(&current_exe, &resolve_brew_path(), cask_token, runner)
}

/// Homebrew is selected only when the canonicalized running executable is in
/// the cask tree or below a bounded `version/*.app` entry from that tree. A
/// direct app remains Direct when the same cask is merely installed alongside.
pub fn detect_install_channel(
    current_exe: &Path,
    brew_path: &Path,
    cask_token: &str,
    runner: &dyn CommandRunner,
) -> Result<InstallChannel, UpdateError> {
    validate_cask_token(cask_token)?;
    if !brew_path.is_file() {
        return Ok(InstallChannel::Direct);
    }
    let output = runner.run(
        brew_path,
        &[OsString::from("--prefix")],
        RELEASE_CHECK_TIMEOUT,
    )?;
    if !output.success {
        return Ok(InstallChannel::Direct);
    }
    let prefix_text = output.stdout.trim();
    if prefix_text.is_empty() || prefix_text.contains(['\r', '\n']) {
        return Ok(InstallChannel::Direct);
    }
    let cask_root = PathBuf::from(prefix_text).join("Caskroom").join(cask_token);
    let Ok(canonical_root) = fs::canonicalize(&cask_root) else {
        return Ok(InstallChannel::Direct);
    };
    let Ok(canonical_exe) = fs::canonicalize(current_exe) else {
        return Ok(InstallChannel::Direct);
    };
    if canonical_exe.is_file()
        && (canonical_exe.starts_with(&canonical_root)
            || cask_apps(&canonical_root).iter().any(|app| {
                fs::canonicalize(app).is_ok_and(|canonical_app| {
                    canonical_app.is_dir() && canonical_exe.starts_with(canonical_app)
                })
            }))
    {
        Ok(InstallChannel::Homebrew {
            brew_path: brew_path.to_owned(),
            cask_root: canonical_root,
        })
    } else {
        Ok(InstallChannel::Direct)
    }
}

/// Return only immediate `version/*.app` entries, with hard traversal bounds.
/// Homebrew casks commonly make the app entry a symlink into `/Applications`.
fn cask_apps(cask_root: &Path) -> Vec<PathBuf> {
    let Ok(versions) = fs::read_dir(cask_root) else {
        return Vec::new();
    };
    let mut apps = Vec::new();
    for version in versions.flatten().take(MAX_CASK_VERSIONS) {
        let Ok(entries) = fs::read_dir(version.path()) else {
            continue;
        };
        apps.extend(
            entries
                .flatten()
                .take(MAX_APPS_PER_CASK_VERSION)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "app")),
        );
    }
    apps
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeIdentity {
    cask_token: String,
    app_name: String,
}

impl UpgradeIdentity {
    pub fn new(
        cask_token: impl Into<String>,
        app_name: impl Into<String>,
    ) -> Result<Self, UpdateError> {
        let cask_token = cask_token.into();
        validate_cask_token(&cask_token)?;
        let app_name = app_name.into();
        if app_name.is_empty() || app_name.len() > 200 || app_name.chars().any(char::is_control) {
            return Err(UpdateError::InvalidIdentity(app_name));
        }
        Ok(Self {
            cask_token,
            app_name,
        })
    }
}

fn validate_cask_token(token: &str) -> Result<(), UpdateError> {
    if token.is_empty()
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'@' | b'+'))
    {
        return Err(UpdateError::InvalidIdentity(token.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperInvocation {
    pub parent_pid: u32,
    pub brew_path: PathBuf,
    pub identity: UpgradeIdentity,
    pub receipt_path: PathBuf,
    pub lock_path: PathBuf,
    pub open_path: PathBuf,
}

impl HelperInvocation {
    /// Fixed positional protocol. Every value is a distinct argv entry; no
    /// shell command is constructed, so paths and app names may contain spaces.
    pub fn cli_args(&self) -> Vec<OsString> {
        vec![
            UPDATE_HELPER_FLAG.into(),
            self.parent_pid.to_string().into(),
            self.brew_path.as_os_str().to_owned(),
            self.identity.cask_token.clone().into(),
            self.identity.app_name.clone().into(),
            self.receipt_path.as_os_str().to_owned(),
            self.lock_path.as_os_str().to_owned(),
            self.open_path.as_os_str().to_owned(),
        ]
    }

    pub fn staged_helper_path(&self) -> PathBuf {
        self.lock_path.with_extension("helper-bin")
    }

    pub fn parse_cli_args<I, T>(args: I) -> Result<Option<Self>, UpdateError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        if args.first().is_none_or(|arg| arg != UPDATE_HELPER_FLAG) {
            return Ok(None);
        }
        if args.len() != 8 {
            return Err(UpdateError::HelperProtocol(
                "update helper expected seven arguments".into(),
            ));
        }
        let text = |index: usize, name: &str| {
            args[index]
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| UpdateError::HelperProtocol(format!("invalid {name}")))
        };
        let parent_pid = text(1, "parent pid")?
            .parse()
            .map_err(|_| UpdateError::HelperProtocol("invalid parent pid".into()))?;
        if parent_pid == 0 {
            return Err(UpdateError::HelperProtocol(
                "parent pid must be positive".into(),
            ));
        }
        Ok(Some(Self {
            parent_pid,
            brew_path: PathBuf::from(&args[2]),
            identity: UpgradeIdentity::new(text(3, "cask token")?, text(4, "app name")?)?,
            receipt_path: PathBuf::from(&args[5]),
            lock_path: PathBuf::from(&args[6]),
            open_path: PathBuf::from(&args[7]),
        }))
    }
}

/// Copy the app executable outside the bundle and spawn that copy as a detached
/// helper. Running the helper from the installed bundle would keep the very
/// binary Homebrew is about to replace alive. The caller must quit only after
/// this succeeds. A create-new lock prevents two upgrade helpers; stale locks
/// with a recorded dead PID are recovered.
pub fn spawn_upgrade_helper(
    installed_executable: &Path,
    invocation: &HelperInvocation,
) -> Result<u32, UpdateError> {
    let mut lock = acquire_helper_lock(&invocation.lock_path)?;
    let staged_helper = invocation.staged_helper_path();
    let _ = fs::remove_file(&staged_helper);
    fs::copy(installed_executable, &staged_helper).map_err(|error| {
        let _ = fs::remove_file(&invocation.lock_path);
        UpdateError::Io(error.to_string())
    })?;
    let child = Command::new(&staged_helper)
        .args(invocation.cli_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            let _ = fs::remove_file(&invocation.lock_path);
            let _ = fs::remove_file(&staged_helper);
            UpdateError::Io(error.to_string())
        })?;
    let pid = child.id();
    // Write through the create-new handle. If a very fast helper already
    // unlinked the lock, this cannot accidentally recreate a stale lock.
    lock.seek(SeekFrom::Start(0))?;
    lock.write_all(pid.to_string().as_bytes())?;
    lock.sync_all()?;
    Ok(pid)
}

fn acquire_helper_lock(path: &Path) -> Result<File, UpdateError> {
    ensure_parent(path)?;
    for attempt in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                let recorded_pid = fs::read_to_string(path)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok());
                // Empty/unreadable means the first spawner may still be between
                // create_new and writing its child PID: fail closed.
                if recorded_pid.is_none_or(|pid| process_is_running(pid).unwrap_or(true)) {
                    return Err(UpdateError::HelperAlreadyRunning);
                }
                fs::remove_file(path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(UpdateError::HelperAlreadyRunning)
            }
            Err(error) => return Err(UpdateError::Io(error.to_string())),
        }
    }
    Err(UpdateError::HelperAlreadyRunning)
}

pub trait ParentWaiter {
    fn wait_for_exit(&self, pid: u32) -> Result<(), UpdateError>;
}

pub struct SystemParentWaiter;

impl ParentWaiter for SystemParentWaiter {
    fn wait_for_exit(&self, pid: u32) -> Result<(), UpdateError> {
        wait_for_exit_with(
            pid,
            Path::new(SYSTEM_KILL_PATH),
            PARENT_EXIT_TIMEOUT,
            HELPER_POLL_INTERVAL,
        )
    }
}

fn wait_for_exit_with(
    pid: u32,
    kill_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(), UpdateError> {
    if pid == 0 {
        return Err(UpdateError::HelperProtocol(
            "parent pid must be positive".into(),
        ));
    }
    let started = Instant::now();
    while process_is_running_with(kill_path, pid)? {
        if started.elapsed() >= timeout {
            return Err(UpdateError::ParentWaitTimedOut(timeout));
        }
        std::thread::sleep(poll_interval);
    }
    Ok(())
}

fn process_is_running(pid: u32) -> Result<bool, UpdateError> {
    process_is_running_with(Path::new(SYSTEM_KILL_PATH), pid)
}

fn process_is_running_with(kill_path: &Path, pid: u32) -> Result<bool, UpdateError> {
    if pid == 0 {
        return Err(UpdateError::HelperProtocol(
            "process pid must be positive".into(),
        ));
    }
    Command::new(kill_path)
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(|error| UpdateError::Io(format!("could not inspect process: {error}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<CommandOutput, UpdateError>;
}

pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<CommandOutput, UpdateError> {
        let output = run_output_with_timeout(Command::new(program).args(args), timeout)?;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: bounded_text(&output.stdout),
            stderr: bounded_text(&output.stderr),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeReceipt {
    pub completed_at_unix: i64,
    pub upgrade_succeeded: bool,
    pub reopen_succeeded: bool,
    pub message: String,
}

/// Helper entry point: wait for the parent, upgrade, write the receipt, and
/// then attempt to reopen. A reopen failure updates the receipt in place.
pub fn run_upgrade_helper(
    invocation: &HelperInvocation,
    runner: &dyn CommandRunner,
    waiter: &dyn ParentWaiter,
) -> Result<UpgradeReceipt, UpdateError> {
    run_upgrade_helper_at(invocation, runner, waiter, unix_now())
}

pub fn run_upgrade_helper_at(
    invocation: &HelperInvocation,
    runner: &dyn CommandRunner,
    waiter: &dyn ParentWaiter,
    completed_at_unix: i64,
) -> Result<UpgradeReceipt, UpdateError> {
    let _lock_cleanup = RemoveOnDrop(invocation.lock_path.clone());
    let _executable_cleanup = RemoveOnDrop(invocation.staged_helper_path());
    waiter.wait_for_exit(invocation.parent_pid)?;
    let upgrade = runner
        .run(
            &invocation.brew_path,
            &[
                "upgrade".into(),
                "--cask".into(),
                invocation.identity.cask_token.clone().into(),
            ],
            UPGRADE_TIMEOUT,
        )
        .unwrap_or_else(|error| CommandOutput {
            success: false,
            code: None,
            stdout: String::new(),
            stderr: bounded_string(error.to_string()),
        });
    let message = if upgrade.success {
        "upgrade completed".to_owned()
    } else {
        format!(
            "upgrade failed ({}): {}",
            upgrade
                .code
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            nonempty_diagnostic(&upgrade)
        )
    };
    let mut receipt = UpgradeReceipt {
        completed_at_unix,
        upgrade_succeeded: upgrade.success,
        reopen_succeeded: true,
        message: bounded_string(message),
    };
    // Persist before launch so the new process cannot race receipt creation.
    save_toml_atomic(&invocation.receipt_path, &receipt)?;

    let reopen = runner.run(
        &invocation.open_path,
        &["-a".into(), invocation.identity.app_name.clone().into()],
        REOPEN_TIMEOUT,
    );
    if !reopen.as_ref().is_ok_and(|output| output.success) {
        let detail = match &reopen {
            Ok(output) => nonempty_diagnostic(output),
            Err(error) => error.to_string(),
        };
        receipt.reopen_succeeded = false;
        receipt.message = bounded_string(format!("{}; reopen failed: {detail}", receipt.message));
        save_toml_atomic(&invocation.receipt_path, &receipt)?;
    }
    Ok(receipt)
}

pub fn load_upgrade_receipt(path: &Path) -> Result<Option<UpgradeReceipt>, UpdateError> {
    match read_bounded(path)? {
        Some(contents) => toml::from_str(&contents)
            .map(Some)
            .map_err(|error| UpdateError::State(format!("invalid receipt: {error}"))),
        None => Ok(None),
    }
}

fn nonempty_diagnostic(output: &CommandOutput) -> String {
    let value = if output.stderr.trim().is_empty() {
        output.stdout.trim()
    } else {
        output.stderr.trim()
    };
    if value.is_empty() {
        "no diagnostic output".into()
    } else {
        bounded_string(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    InvalidVersion(String),
    InvalidIdentity(String),
    InvalidRelease(String),
    CommandFailed(String),
    /// The direct request to GitHub failed (network, rate limit, status).
    Request(String),
    CommandTimedOut(Duration),
    ParentWaitTimedOut(Duration),
    State(String),
    HelperProtocol(String),
    HelperAlreadyRunning,
    Io(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersion(value) => write!(f, "invalid stable version: {value}"),
            Self::InvalidIdentity(value) => write!(f, "invalid update identity: {value}"),
            Self::InvalidRelease(value) => write!(f, "invalid latest release: {value}"),
            Self::CommandFailed(value) => write!(f, "update command failed: {value}"),
            Self::Request(value) => write!(f, "update check request failed: {value}"),
            Self::CommandTimedOut(timeout) => {
                write!(f, "update command timed out after {}s", timeout.as_secs())
            }
            Self::ParentWaitTimedOut(timeout) => {
                write!(f, "parent did not exit after {}s", timeout.as_secs())
            }
            Self::State(value) => write!(f, "update state error: {value}"),
            Self::HelperProtocol(value) => write!(f, "update helper protocol error: {value}"),
            Self::HelperAlreadyRunning => write!(f, "an update helper is already running"),
            Self::Io(value) => write!(f, "update I/O error: {value}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<io::Error> for UpdateError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

fn run_output_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, UpdateError> {
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| UpdateError::Io("missing command stdout pipe".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| UpdateError::Io("missing command stderr pipe".into()))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    for (is_stdout, pipe) in [
        (true, Box::new(stdout) as Box<dyn Read + Send>),
        (false, Box::new(stderr) as Box<dyn Read + Send>),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send((is_stdout, capture_bounded(pipe)));
        });
    }
    drop(sender);

    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut captured_stdout = None;
    let mut captured_stderr = None;
    loop {
        if status.is_none() {
            status = child.try_wait()?;
        }
        while let Ok((is_stdout, captured)) = receiver.try_recv() {
            let captured = captured?;
            if is_stdout {
                captured_stdout = Some(captured);
            } else {
                captured_stderr = Some(captured);
            }
        }
        if let (Some(status), Some(_), Some(_)) = (status, &captured_stdout, &captured_stderr) {
            return Ok(Output {
                status,
                stdout: captured_stdout.take().unwrap(),
                stderr: captured_stderr.take().unwrap(),
            });
        }
        if Instant::now() >= deadline {
            terminate_process_group(&mut child, pid);
            let _ = child.wait();
            return Err(UpdateError::CommandTimedOut(timeout));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn capture_bounded(mut pipe: Box<dyn Read + Send>) -> io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(MAX_COMMAND_OUTPUT_BYTES);
    let mut buffer = [0_u8; 4096];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn terminate_process_group(child: &mut std::process::Child, pid: u32) {
    #[cfg(unix)]
    let group_killed = Command::new(SYSTEM_KILL_PATH)
        .args(["-KILL", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    #[cfg(not(unix))]
    let group_killed = false;

    if !group_killed {
        let _ = child.kill();
    }
}

fn resolve_program(name: &str, candidates: &[&str]) -> PathBuf {
    if let Some(found) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    }) {
        return found;
    }
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn check_lock_path(state_path: &Path) -> PathBuf {
    state_path.with_extension("check.lock")
}

struct FileLock(PathBuf);

impl FileLock {
    fn acquire(path: PathBuf) -> Result<Option<Self>, UpdateError> {
        ensure_parent(&path)?;
        for attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Some(Self(path)));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                    let recorded_pid = fs::read_to_string(&path)
                        .ok()
                        .and_then(|text| text.trim().parse::<u32>().ok());
                    if recorded_pid
                        .is_some_and(|pid| process_is_running(pid).is_ok_and(|running| !running))
                    {
                        fs::remove_file(&path)?;
                        continue;
                    }
                    return Ok(None);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
                Err(error) => return Err(UpdateError::Io(error.to_string())),
            }
        }
        Ok(None)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn save_toml_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), UpdateError> {
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    ensure_parent(path)?;
    let contents = toml::to_string(value).map_err(|error| UpdateError::State(error.to_string()))?;
    if contents.len() as u64 > MAX_STATE_BYTES {
        return Err(UpdateError::State("serialized state was too large".into()));
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok::<_, io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(UpdateError::from)
}

fn read_bounded(path: &Path) -> Result<Option<String>, UpdateError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(UpdateError::Io(error.to_string())),
    };
    if metadata.len() > MAX_STATE_BYTES {
        return Err(UpdateError::State(format!(
            "{} exceeds the {} byte limit",
            path.display(),
            MAX_STATE_BYTES
        )));
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|error| UpdateError::Io(error.to_string()))
}

fn ensure_parent(path: &Path) -> Result<(), UpdateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

fn bounded_text(bytes: &[u8]) -> String {
    bounded_string(String::from_utf8_lossy(bytes).into_owned())
}

fn bounded_string(mut value: String) -> String {
    if value.len() <= MAX_DIAGNOSTIC_BYTES {
        return value;
    }
    let mut end = MAX_DIAGNOSTIC_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push('…');
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    type RecordedCalls = Arc<Mutex<Vec<(PathBuf, Vec<OsString>)>>>;

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "update-engine-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn identity() -> ReleaseIdentity {
        ReleaseIdentity::new("owner/product").unwrap()
    }

    #[test]
    fn stable_versions_cover_boundaries_and_reject_non_stable_input() {
        assert!(StableVersion::parse("v1.10.0").unwrap() > StableVersion::parse("1.9.99").unwrap());
        assert_eq!(StableVersion::parse("0.0.0").unwrap().to_string(), "0.0.0");
        for invalid in [
            "1.2",
            "1.2.3.4",
            "1.2.3-beta",
            "1.2.3+build",
            "01.2.3",
            "v",
            " 1.2.3",
        ] {
            assert!(StableVersion::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn daily_gate_handles_boundary_and_clock_rollback() {
        assert!(automatic_check_due(None, 100));
        assert!(!automatic_check_due(Some(100), 100 + 86_399));
        assert!(automatic_check_due(Some(100), 100 + 86_400));
        assert!(!automatic_check_due(Some(200), 100));
        assert!(!automatic_check_due(Some(i64::MAX), i64::MIN));
    }

    #[derive(Clone)]
    struct FakeSource {
        calls: Arc<Mutex<usize>>,
        result: Result<ReleaseInfo, UpdateError>,
    }

    impl ReleaseSource for FakeSource {
        fn latest_stable(&self, _: &ReleaseIdentity) -> Result<ReleaseInfo, UpdateError> {
            *self.calls.lock().unwrap() += 1;
            self.result.clone()
        }
    }

    fn fake_release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: StableVersion::parse(version).unwrap(),
            tag: format!("v{version}"),
            page_url: format!("https://github.com/owner/product/releases/tag/v{version}"),
        }
    }

    /// GitHub's REST answer, and the paths it was asked for.
    struct FakeRest {
        paths: Mutex<Vec<String>>,
        answer: Result<serde_json::Value, prmarmot_core::github::GhError>,
    }

    impl FakeRest {
        fn answering(answer: Result<serde_json::Value, prmarmot_core::github::GhError>) -> Self {
            Self {
                paths: Mutex::new(Vec::new()),
                answer,
            }
        }
    }

    impl RestTransport for FakeRest {
        fn rest_get(
            &self,
            path: &str,
            query: &[(&str, &str)],
        ) -> Result<serde_json::Value, prmarmot_core::github::GhError> {
            assert!(query.is_empty(), "the release check takes no query");
            self.paths.lock().unwrap().push(path.to_owned());
            self.answer.clone()
        }
    }

    fn release_json(tag: &str, url: &str, draft: bool, prerelease: bool) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "html_url": url,
            "draft": draft,
            "prerelease": prerelease,
            "name": "ignored",
        })
    }

    #[test]
    fn a_release_check_needs_no_github_cli() {
        let source = HttpReleaseSource::new(FakeRest::answering(Ok(release_json(
            "v1.2.3",
            "https://github.com/owner/product/releases/tag/v1.2.3",
            false,
            false,
        ))));
        assert_eq!(
            source.latest_stable(&identity()).unwrap(),
            fake_release("1.2.3")
        );
        assert_eq!(
            *source.rest.paths.lock().unwrap(),
            ["repos/owner/product/releases/latest"]
        );
    }

    #[test]
    fn the_direct_check_refuses_what_the_gh_check_refuses() {
        let page = "https://github.com/owner/product/releases/tag/v1.2.3";
        for (label, body) in [
            ("draft", release_json("v1.2.3", page, true, false)),
            ("prerelease", release_json("v1.2.3", page, false, true)),
            (
                "another repository",
                release_json(
                    "v1.2.3",
                    "https://github.com/else/where/releases/tag/v1.2.3",
                    false,
                    false,
                ),
            ),
            (
                "a tab in the tag",
                release_json("v1.2.3\tx", page, false, false),
            ),
            (
                "no tag",
                serde_json::json!({"html_url": page, "draft": false, "prerelease": false}),
            ),
        ] {
            let source = HttpReleaseSource::new(FakeRest::answering(Ok(body)));
            assert!(
                matches!(
                    source.latest_stable(&identity()),
                    Err(UpdateError::InvalidRelease(_))
                ),
                "{label}"
            );
        }
        let limited = HttpReleaseSource::new(FakeRest::answering(Err(
            prmarmot_core::github::GhError::RateLimited {
                reset_epoch: None,
                retry_after_secs: None,
            },
        )));
        assert!(matches!(
            limited.latest_stable(&identity()),
            Err(UpdateError::Request(_))
        ));
    }

    #[test]
    fn the_github_cli_is_asked_first_and_github_directly_when_it_cannot_answer() {
        let fake = |result: Result<ReleaseInfo, UpdateError>| FakeSource {
            calls: Arc::new(Mutex::new(0)),
            result,
        };
        let gh = fake(Ok(fake_release("1.0.0")));
        let direct = fake(Ok(fake_release("2.0.0")));
        let both = FallbackReleaseSource::new(gh.clone(), direct.clone());
        assert_eq!(
            both.latest_stable(&identity()).unwrap(),
            fake_release("1.0.0")
        );
        assert_eq!(*direct.calls.lock().unwrap(), 0, "gh answered");

        let no_gh = fake(Err(UpdateError::CommandFailed("gh: not found".into())));
        let fallback = FallbackReleaseSource::new(no_gh, direct.clone());
        assert_eq!(
            fallback.latest_stable(&identity()).unwrap(),
            fake_release("2.0.0")
        );
        assert_eq!(*direct.calls.lock().unwrap(), 1);

        let offline = fake(Err(UpdateError::Request("offline".into())));
        let neither = FallbackReleaseSource::new(
            fake(Err(UpdateError::CommandFailed("gh: not found".into()))),
            offline,
        );
        assert_eq!(
            neither.latest_stable(&identity()),
            Err(UpdateError::Request("offline".into()))
        );
    }

    #[test]
    fn disabled_never_requests_and_failure_is_claimed_across_restart() {
        let dir = temp_dir("daily");
        let path = dir.join("updates.toml");
        let calls = Arc::new(Mutex::new(0));
        let source = FakeSource {
            calls: calls.clone(),
            result: Err(UpdateError::CommandFailed("offline".into())),
        };
        let checker = UpdateChecker::new(&path, source.clone());
        assert_eq!(
            checker
                .automatic_check(false, 100, "1.0.0", &identity())
                .unwrap(),
            AutomaticCheck::Disabled
        );
        assert_eq!(*calls.lock().unwrap(), 0);
        assert!(matches!(
            checker
                .automatic_check(true, 100, "1.0.0", &identity())
                .unwrap(),
            AutomaticCheck::Completed {
                result: CheckResult::Failed { .. },
                ..
            }
        ));
        let restarted = UpdateChecker::new(&path, source);
        assert!(matches!(
            restarted
                .automatic_check(true, 200, "1.0.0", &identity())
                .unwrap(),
            AutomaticCheck::NotDue(Some(CheckResult::Failed { .. }))
        ));
        assert_eq!(*calls.lock().unwrap(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persisted_future_attempt_suppresses_request_after_clock_rollback() {
        let dir = temp_dir("future-attempt");
        let path = dir.join("updates.toml");
        save_toml_atomic(
            &path,
            &UpdateState {
                last_attempt_unix: Some(10_000),
                last_result: None,
            },
        )
        .unwrap();
        let calls = Arc::new(Mutex::new(0));
        let checker = UpdateChecker::new(
            &path,
            FakeSource {
                calls: calls.clone(),
                result: Ok(fake_release("2.0.0")),
            },
        );
        assert_eq!(
            checker
                .automatic_check(true, 9_000, "1.0.0", &identity())
                .unwrap(),
            AutomaticCheck::NotDue(None)
        );
        assert_eq!(*calls.lock().unwrap(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn not_due_cached_available_is_recompared_with_current_version() {
        let dir = temp_dir("cached-version");
        let path = dir.join("updates.toml");
        let release = fake_release("2.0.0");
        save_toml_atomic(
            &path,
            &UpdateState {
                last_attempt_unix: Some(100),
                last_result: Some(CheckResult::Available {
                    version: release.version,
                    tag: release.tag,
                    page_url: release.page_url,
                }),
            },
        )
        .unwrap();
        let calls = Arc::new(Mutex::new(0));
        let checker = UpdateChecker::new(
            &path,
            FakeSource {
                calls: calls.clone(),
                result: Ok(fake_release("3.0.0")),
            },
        );

        assert_eq!(
            checker
                .automatic_check(true, 200, "2.0.0", &identity())
                .unwrap(),
            AutomaticCheck::NotDue(Some(CheckResult::UpToDate {
                latest: StableVersion::parse("2.0.0").unwrap(),
            }))
        );
        assert_eq!(*calls.lock().unwrap(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn check_classifies_older_equal_and_newer_releases() {
        for (remote, expected_available) in [("1.9.9", false), ("2.0.0", false), ("2.0.1", true)] {
            let dir = temp_dir(remote);
            let checker = UpdateChecker::new(
                dir.join("state.toml"),
                FakeSource {
                    calls: Arc::new(Mutex::new(0)),
                    result: Ok(fake_release(remote)),
                },
            );
            let outcome = checker
                .automatic_check(true, 10, "2.0.0", &identity())
                .unwrap();
            assert_eq!(
                matches!(
                    outcome,
                    AutomaticCheck::Completed {
                        result: CheckResult::Available { .. },
                        ..
                    }
                ),
                expected_available
            );
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn release_contract_rejects_prerelease_and_wrong_page() {
        assert!(parse_release_line(
            "v1.2.3\thttps://github.com/owner/product/releases/tag/v1.2.3\tfalse\ttrue\n",
            &identity()
        )
        .is_err());
        assert!(parse_release_line(
            "v1.2.3\thttps://example.test/v1.2.3\tfalse\tfalse\n",
            &identity()
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn gh_release_source_uses_a_bounded_fake_command() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("fake-gh");
        let gh = dir.join("gh fake");
        fs::write(
            &gh,
            "#!/bin/sh\nprintf 'v1.2.3\\thttps://github.com/owner/product/releases/tag/v1.2.3\\tfalse\\tfalse\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let release = GhReleaseSource::new(&gh)
            .latest_stable(&identity())
            .unwrap();
        assert_eq!(release.version, StableVersion::parse("1.2.3").unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    struct FakeRunner {
        outputs: Mutex<VecDeque<Result<CommandOutput, UpdateError>>>,
        calls: RecordedCalls,
        waited: Arc<AtomicBool>,
        receipt_before_reopen: Option<PathBuf>,
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            program: &Path,
            args: &[OsString],
            _: Duration,
        ) -> Result<CommandOutput, UpdateError> {
            assert!(
                self.waited.load(Ordering::SeqCst),
                "command ran before parent exit"
            );
            if args.first().is_some_and(|arg| arg == "-a") {
                if let Some(path) = &self.receipt_before_reopen {
                    let receipt = load_upgrade_receipt(path).unwrap().unwrap();
                    assert!(receipt.reopen_succeeded);
                }
            }
            self.calls
                .lock()
                .unwrap()
                .push((program.to_owned(), args.to_vec()));
            self.outputs.lock().unwrap().pop_front().unwrap()
        }
    }

    struct DelayedWaiter(Arc<AtomicBool>);

    impl ParentWaiter for DelayedWaiter {
        fn wait_for_exit(&self, _: u32) -> Result<(), UpdateError> {
            std::thread::sleep(Duration::from_millis(20));
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn command_output(success: bool, code: i32, stderr: &str) -> CommandOutput {
        CommandOutput {
            success,
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn helper_waits_then_reopens_after_failed_upgrade_and_records_failure() {
        let dir = temp_dir("helper-failure");
        let receipt_path = dir.join("receipt.toml");
        let lock_path = dir.join("helper.lock");
        fs::write(&lock_path, "123").unwrap();
        let waited = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = FakeRunner {
            outputs: Mutex::new(VecDeque::from([
                Ok(command_output(false, 1, "tap failed")),
                Err(UpdateError::CommandFailed("open failed".into())),
            ])),
            calls: calls.clone(),
            waited: waited.clone(),
            receipt_before_reopen: Some(receipt_path.clone()),
        };
        let invocation = HelperInvocation {
            parent_pid: 55,
            brew_path: PathBuf::from("/fake path/brew"),
            identity: UpgradeIdentity::new("product", "Product App").unwrap(),
            receipt_path: receipt_path.clone(),
            lock_path: lock_path.clone(),
            open_path: PathBuf::from("/fake path/open"),
        };
        let receipt =
            run_upgrade_helper_at(&invocation, &runner, &DelayedWaiter(waited), 500).unwrap();
        assert!(!receipt.upgrade_succeeded);
        assert!(!receipt.reopen_succeeded);
        assert!(receipt.message.contains("tap failed"));
        assert!(receipt.message.contains("open failed"));
        assert!(!lock_path.exists());
        assert_eq!(load_upgrade_receipt(&receipt_path).unwrap(), Some(receipt));
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].1, ["upgrade", "--cask", "product"]);
        assert_eq!(calls[1].1, ["-a", "Product App"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn successful_reopen_does_not_rewrite_consumed_receipt() {
        struct ConsumingRunner {
            receipt_path: PathBuf,
            calls: Mutex<usize>,
        }

        impl CommandRunner for ConsumingRunner {
            fn run(
                &self,
                _: &Path,
                _: &[OsString],
                _: Duration,
            ) -> Result<CommandOutput, UpdateError> {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                if *calls == 2 {
                    assert!(load_upgrade_receipt(&self.receipt_path).unwrap().is_some());
                    fs::remove_file(&self.receipt_path).unwrap();
                }
                Ok(command_output(true, 0, ""))
            }
        }

        struct ImmediateWaiter;
        impl ParentWaiter for ImmediateWaiter {
            fn wait_for_exit(&self, _: u32) -> Result<(), UpdateError> {
                Ok(())
            }
        }

        let dir = temp_dir("receipt-consumed");
        let invocation = HelperInvocation {
            parent_pid: 55,
            brew_path: PathBuf::from("/brew"),
            identity: UpgradeIdentity::new("product", "Product").unwrap(),
            receipt_path: dir.join("receipt.toml"),
            lock_path: dir.join("helper.lock"),
            open_path: PathBuf::from("/open"),
        };
        let receipt = run_upgrade_helper_at(
            &invocation,
            &ConsumingRunner {
                receipt_path: invocation.receipt_path.clone(),
                calls: Mutex::new(0),
            },
            &ImmediateWaiter,
            500,
        )
        .unwrap();
        assert!(receipt.reopen_succeeded);
        assert!(!invocation.receipt_path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    struct PrefixRunner(PathBuf);

    impl CommandRunner for PrefixRunner {
        fn run(&self, _: &Path, _: &[OsString], _: Duration) -> Result<CommandOutput, UpdateError> {
            Ok(CommandOutput {
                success: true,
                code: Some(0),
                stdout: self.0.display().to_string(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn direct_app_stays_direct_alongside_an_installed_cask() {
        let dir = temp_dir("channels");
        let brew = dir.join("bin/brew");
        fs::create_dir_all(brew.parent().unwrap()).unwrap();
        fs::write(&brew, "fake").unwrap();
        let cask_exe = dir.join("Caskroom/product/1.0/Product.app/Contents/MacOS/product");
        fs::create_dir_all(cask_exe.parent().unwrap()).unwrap();
        fs::write(&cask_exe, "cask").unwrap();
        let direct_exe = dir.join("Applications/Product.app/Contents/MacOS/product");
        fs::create_dir_all(direct_exe.parent().unwrap()).unwrap();
        fs::write(&direct_exe, "direct").unwrap();
        let runner = PrefixRunner(dir.clone());
        assert!(matches!(
            detect_install_channel(&cask_exe, &brew, "product", &runner).unwrap(),
            InstallChannel::Homebrew { .. }
        ));
        assert_eq!(
            detect_install_channel(&direct_exe, &brew, "product", &runner).unwrap(),
            InstallChannel::Direct
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn caskroom_app_symlink_to_applications_is_homebrew() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("cask-symlink");
        let brew = dir.join("bin/brew");
        fs::create_dir_all(brew.parent().unwrap()).unwrap();
        fs::write(&brew, "fake").unwrap();
        let installed_app = dir.join("Applications/Product.app");
        let installed_exe = installed_app.join("Contents/MacOS/product");
        fs::create_dir_all(installed_exe.parent().unwrap()).unwrap();
        fs::write(&installed_exe, "installed").unwrap();
        let cask_version = dir.join("Caskroom/product/2.0.0");
        fs::create_dir_all(&cask_version).unwrap();
        symlink(&installed_app, cask_version.join("Product.app")).unwrap();

        assert!(matches!(
            detect_install_channel(&installed_exe, &brew, "product", &PrefixRunner(dir.clone()))
                .unwrap(),
            InstallChannel::Homebrew { .. }
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_brew_is_direct_without_invoking_command() {
        struct MustNotRun;
        impl CommandRunner for MustNotRun {
            fn run(
                &self,
                _: &Path,
                _: &[OsString],
                _: Duration,
            ) -> Result<CommandOutput, UpdateError> {
                panic!("missing brew must not be invoked")
            }
        }
        assert_eq!(
            detect_install_channel(
                Path::new("/direct/Product.app/Contents/MacOS/product"),
                Path::new("/missing/brew"),
                "product",
                &MustNotRun,
            )
            .unwrap(),
            InstallChannel::Direct
        );
    }

    #[test]
    fn helper_lock_rejects_a_second_process() {
        let dir = temp_dir("duplicate-helper");
        let lock = dir.join("helper.lock");
        fs::write(&lock, std::process::id().to_string()).unwrap();
        assert!(matches!(
            acquire_helper_lock(&lock),
            Err(UpdateError::HelperAlreadyRunning)
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn helper_protocol_preserves_spaces_without_shell_interpolation() {
        let invocation = HelperInvocation {
            parent_pid: 42,
            brew_path: PathBuf::from("/path with spaces/brew"),
            identity: UpgradeIdentity::new("product", "Product App").unwrap(),
            receipt_path: PathBuf::from("/state path/receipt"),
            lock_path: PathBuf::from("/state path/lock"),
            open_path: PathBuf::from("/usr/bin/open"),
        };
        assert_eq!(
            HelperInvocation::parse_cli_args(invocation.cli_args()).unwrap(),
            Some(invocation)
        );
    }

    #[test]
    fn helper_protocol_rejects_zero_parent_pid() {
        let invocation = HelperInvocation {
            parent_pid: 0,
            brew_path: PathBuf::from("/brew"),
            identity: UpgradeIdentity::new("product", "Product").unwrap(),
            receipt_path: PathBuf::from("/receipt"),
            lock_path: PathBuf::from("/lock"),
            open_path: PathBuf::from("/open"),
        };
        assert!(matches!(
            HelperInvocation::parse_cli_args(invocation.cli_args()),
            Err(UpdateError::HelperProtocol(_))
        ));
        assert!(matches!(
            SystemParentWaiter.wait_for_exit(0),
            Err(UpdateError::HelperProtocol(_))
        ));
    }

    #[test]
    fn process_probe_fails_closed_when_kill_is_unavailable() {
        assert!(matches!(
            process_is_running_with(Path::new("/definitely/missing/kill"), 1),
            Err(UpdateError::Io(_))
        ));
        assert!(matches!(
            wait_for_exit_with(
                1,
                Path::new("/definitely/missing/kill"),
                Duration::ZERO,
                Duration::ZERO,
            ),
            Err(UpdateError::Io(_))
        ));
    }

    #[test]
    fn parent_wait_has_a_hard_deadline() {
        assert!(matches!(
            wait_for_exit_with(
                std::process::id(),
                Path::new(SYSTEM_KILL_PATH),
                Duration::ZERO,
                Duration::ZERO,
            ),
            Err(UpdateError::ParentWaitTimedOut(timeout)) if timeout.is_zero()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn command_output_capture_is_bounded() {
        let output = run_output_with_timeout(
            Command::new("/bin/sh").args([
                "-c",
                "i=0; while [ $i -lt 6000 ]; do printf x; i=$((i + 1)); done",
            ]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_COMMAND_OUTPUT_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn command_retains_output_when_pipes_close_at_different_times() {
        let output = run_output_with_timeout(
            Command::new("/bin/sh")
                .args(["-c", "printf early; exec 1>&-; sleep 0.1; printf late >&2"]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"early");
        assert_eq!(output.stderr, b"late");
    }

    #[cfg(unix)]
    #[test]
    fn command_timeout_kills_descendants_holding_output_pipes() {
        let started = Instant::now();
        let result = run_output_with_timeout(
            Command::new("/bin/sh").args(["-c", "sleep 10 & exit 0"]),
            Duration::from_millis(100),
        );
        assert!(matches!(result, Err(UpdateError::CommandTimedOut(_))));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
