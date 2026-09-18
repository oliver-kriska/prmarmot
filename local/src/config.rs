//! The read side of `~/.config/prmarmot/config.toml` (or `$XDG_CONFIG_HOME`),
//! shared by the desktop app and the CLI. Writing the file back (settings,
//! repo/theme/view/window) stays in the app, which owns `toml_edit`.
//!
//! Precedence is CLI > environment > config; when no scope is configured,
//! PR Marmot shows all repositories involving the signed-in user.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use prmarmot_core::board::{BoardConfig, BoardScope, IssueLinkRule};
use prmarmot_core::github::rate_limit::{DEFAULT_REFRESH_SECS, MIN_REFRESH_SECS};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct FileConfig {
    /// Default repo (`owner/name`) when neither `--repo` nor `PRMARMOT_REPO` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// `all` or `repo`. Absent keeps old configs compatible: a saved repo is
    /// specific, while a clean config defaults to all repositories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Entries for the repo picker; the active repo is always included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_repos: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_secs: Option<u64>,
    /// `system` | `light` | `dark`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// `authored` | `review` — the view to open with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_reviewers: Vec<String>,
    #[serde(default = "default_true")]
    pub notifications: bool,
    #[serde(default = "default_true")]
    pub notification_sound: bool,
    #[serde(default)]
    pub notify_all_needs_action: bool,
    #[serde(default = "default_true")]
    pub dock_badge: bool,
    /// Check GitHub's latest stable release on launch, at most once per day.
    #[serde(default = "default_true")]
    pub automatic_update_checks: bool,
    /// Days a PR may wait for a reviewer before it counts as stale
    /// (`is:stale`, `--stale`); at least 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_after_days: Option<u64>,
    // TOML requires plain keys before tables, so these come last.
    /// `[repo_reviewers]`: suggestions per owner (`"acme"`) or repository
    /// (`"acme/api"`), used before `default_reviewers`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repo_reviewers: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_link: Option<IssueLinkSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSection>,
    /// `[auth]`: which GitHub host to talk to and how to sign in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthSection>,
}

fn default_true() -> bool {
    true
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            repo: None,
            scope: None,
            repos: Vec::new(),
            pinned_repos: Vec::new(),
            refresh_secs: None,
            theme: None,
            view: None,
            default_reviewers: Vec::new(),
            repo_reviewers: BTreeMap::new(),
            issue_link: None,
            window: None,
            notifications: true,
            notification_sound: true,
            notify_all_needs_action: false,
            dock_badge: true,
            automatic_update_checks: true,
            stale_after_days: None,
            auth: None,
        }
    }
}

/// `[auth]` in the config file. Absent means github.com in `auto` mode, whose
/// order is `session::sign_in_used`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AuthSection {
    /// `github.com`, or a GitHub Enterprise Server hostname.
    pub host: Option<String>,
    /// The OAuth client ID for this host. Public by design — the device flow
    /// has no client secret. GHES instances need their own registration.
    pub client_id: Option<String>,
    /// `auto` | `gh` | `device` | `token`.
    pub mode: Option<String>,
    /// `auto` | `keychain` | `file`.
    pub store: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WindowSection {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssueLinkSection {
    pub pattern: String,
    pub url_template: String,
}

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prmarmot")
        .join("config.toml")
}

/// The config file as parsed. A missing file is the clean default; an
/// unreadable or invalid one is an error so each front end can say so.
pub fn try_load() -> Result<FileConfig, String> {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileConfig::default()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    toml::from_str(&text).map_err(|e| format!("ignoring invalid {}: {e}", path.display()))
}

/// Missing file → defaults; unparseable file → defaults with a warning
/// (a typo in the config must never make the app unlaunchable).
pub fn load() -> FileConfig {
    try_load().unwrap_or_else(|warning| {
        eprintln!("prmarmot: {warning}");
        FileConfig::default()
    })
}

pub const MAX_PINNED_REPOS: usize = 12;

pub fn normalized_pins(repos: &[String]) -> Vec<String> {
    let mut pins: Vec<String> = Vec::new();
    for repo in repos {
        let repo = repo.trim();
        if !repo.is_empty() && !pins.iter().any(|pin| pin.eq_ignore_ascii_case(repo)) {
            pins.push(repo.to_owned());
            if pins.len() == MAX_PINNED_REPOS {
                break;
            }
        }
    }
    pins
}

pub fn resolve_scope(
    cli: Option<BoardScope>,
    env_repo: Option<String>,
    env_scope: Option<&str>,
    file: &FileConfig,
) -> BoardScope {
    if let Some(scope) = cli {
        return scope;
    }
    if let Some(repo) = env_repo.filter(|repo| !repo.trim().is_empty()) {
        return BoardScope::Repository(repo);
    }
    if env_scope.is_some_and(|scope| scope.eq_ignore_ascii_case("all")) {
        return BoardScope::AllRepositories;
    }
    if env_scope.is_some_and(|scope| scope.eq_ignore_ascii_case("repo")) {
        if let Some(repo) = file.repo.clone().filter(|repo| !repo.trim().is_empty()) {
            return BoardScope::Repository(repo);
        }
    }
    match file.scope.as_deref() {
        Some(scope) if scope.eq_ignore_ascii_case("all") => BoardScope::AllRepositories,
        Some(scope) if scope.eq_ignore_ascii_case("repo") => file
            .repo
            .clone()
            .filter(|repo| !repo.trim().is_empty())
            .map(BoardScope::Repository)
            .unwrap_or(BoardScope::AllRepositories),
        _ => file
            .repo
            .clone()
            .filter(|repo| !repo.trim().is_empty())
            .map(BoardScope::Repository)
            .unwrap_or(BoardScope::AllRepositories),
    }
}

/// Board rules from the file, overridden by `PRMARMOT_DEFAULT_REVIEWERS` (the
/// fallback list only; `[repo_reviewers]` still applies) and
/// `PRMARMOT_ISSUE_PATTERN` + `PRMARMOT_ISSUE_URL_TEMPLATE`. An invalid issue
/// pattern or `[repo_reviewers]` key is dropped and described in the returned
/// warning.
pub fn board_config(file: &FileConfig) -> (BoardConfig, Option<String>) {
    let mut config = BoardConfig::default();
    let mut warnings = Vec::new();
    if !file.default_reviewers.is_empty() {
        config.default_reviewers = file.default_reviewers.clone();
    }
    config.repo_reviewers = repo_reviewers(&file.repo_reviewers, &mut warnings);
    match file.stale_after_days {
        Some(0) => warnings.push("ignoring stale_after_days = 0: use 1 or more".into()),
        Some(days) => config.stale_after_days = days,
        None => {}
    }
    if let Some(reviewers) = std::env::var("PRMARMOT_DEFAULT_REVIEWERS")
        .ok()
        .filter(|v| !v.is_empty())
    {
        config.default_reviewers = reviewers.split(',').map(|s| s.trim().to_string()).collect();
    }
    let issue_rule = match (
        std::env::var("PRMARMOT_ISSUE_PATTERN"),
        std::env::var("PRMARMOT_ISSUE_URL_TEMPLATE"),
    ) {
        (Ok(pattern), Ok(template)) => Some((pattern, template)),
        _ => file
            .issue_link
            .as_ref()
            .map(|l| (l.pattern.clone(), l.url_template.clone())),
    };
    if let Some((pattern, template)) = issue_rule {
        match IssueLinkRule::new(&pattern, &template) {
            Ok(rule) => config.issue_link = Some(rule),
            Err(e) => warnings.push(format!("ignoring bad issue-link pattern: {e}")),
        }
    }
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    (config, warning)
}

/// `[repo_reviewers]` with keys lowercased and checked to be `owner` or
/// `owner/name`, and blank usernames dropped.
fn repo_reviewers(
    entries: &BTreeMap<String, Vec<String>>,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, Vec<String>> {
    let mut resolved = BTreeMap::new();
    for (key, reviewers) in entries {
        let key = key.trim().to_ascii_lowercase();
        let segments: Vec<&str> = key.split('/').collect();
        let valid = segments.len() <= 2
            && segments
                .iter()
                .all(|segment| !segment.is_empty() && !segment.contains(char::is_whitespace));
        if !valid {
            warnings.push(format!(
                "ignoring [repo_reviewers] key {key:?}: use \"owner\" or \"owner/name\""
            ));
            continue;
        }
        let reviewers = reviewers
            .iter()
            .map(|reviewer| reviewer.trim().to_owned())
            .filter(|reviewer| !reviewer.is_empty())
            .collect();
        resolved.insert(key, reviewers);
    }
    resolved
}

/// Refresh interval: `PRMARMOT_REFRESH_SECS` > config file > default, always
/// clamped to the hard floor.
pub fn refresh_interval(config_secs: Option<u64>) -> Duration {
    let secs = std::env::var("PRMARMOT_REFRESH_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .or(config_secs)
        .unwrap_or(DEFAULT_REFRESH_SECS)
        .max(MIN_REFRESH_SECS);
    Duration::from_secs(secs)
}

/// How PR Marmot gets a GitHub token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// A token this machine stored, else the `gh` CLI. The default, and what
    /// every existing install keeps doing until someone signs in directly.
    #[default]
    Auto,
    /// The `gh` CLI owns authentication (the original behaviour).
    Gh,
    /// A device-flow token stored by `prmarmot-cli auth login`.
    Device,
    /// A pasted personal access token: `PRMARMOT_TOKEN`, else the stored one.
    Token,
}

impl AuthMode {
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "gh" | "cli" => Some(Self::Gh),
            "device" | "oauth" => Some(Self::Device),
            "token" | "pat" => Some(Self::Token),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gh => "gh",
            Self::Device => "device",
            Self::Token => "token",
        }
    }
}

/// Everything needed to obtain a token, resolved across CLI, environment and
/// config file. Unknown words in the file or the environment are reported and
/// then ignored — a typo must never make the app unlaunchable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSettings {
    pub host: String,
    pub client_id: String,
    pub mode: AuthMode,
    pub store: crate::auth::StoreKind,
    /// `PRMARMOT_TOKEN`, when set: a token passed in for this run only and
    /// never written to the store.
    pub inline_token: Option<String>,
}

impl AuthSettings {
    /// True while the GitHub App has not been registered, so the device flow
    /// cannot work and callers should steer the user to the token path.
    pub fn client_id_is_placeholder(&self) -> bool {
        prmarmot_core::github::device_flow::is_placeholder_client_id(&self.client_id)
    }

    /// The same settings for another host, as the sign-in screen's Enterprise
    /// field needs. A client ID that came with the old host (PR Marmot's own,
    /// or none) follows the new host; one the user configured is kept.
    pub fn for_host(&self, host: &str) -> AuthSettings {
        use prmarmot_core::github::device_flow::{default_client_id, PLACEHOLDER_CLIENT_ID};
        let host = prmarmot_core::github::normalize_host(host);
        let inherited = self.client_id_is_placeholder()
            || default_client_id(&self.host) == Some(self.client_id.as_str());
        let client_id = if inherited {
            default_client_id(&host)
                .unwrap_or(PLACEHOLDER_CLIENT_ID)
                .to_owned()
        } else {
            self.client_id.clone()
        };
        AuthSettings {
            host,
            client_id,
            ..self.clone()
        }
    }
}

/// Resolve `[auth]`. `cli_host` and `cli_mode` come from flags and win;
/// then the environment; then the file.
pub fn auth_settings(
    file: &FileConfig,
    cli_host: Option<&str>,
    cli_mode: Option<AuthMode>,
    warnings: &mut Vec<String>,
) -> AuthSettings {
    let section = file.auth.clone().unwrap_or_default();
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let host = cli_host
        .map(str::to_owned)
        .filter(|host| !host.trim().is_empty())
        .or_else(|| env("PRMARMOT_HOST"))
        // GH_HOST is what `gh` itself reads; honouring it keeps one setting
        // for both front doors.
        .or_else(|| env("GH_HOST"))
        .or_else(|| section.host.clone())
        .unwrap_or_else(|| "github.com".into());
    let client_id = env("PRMARMOT_CLIENT_ID")
        .or_else(|| section.client_id.clone())
        .filter(|id| !id.trim().is_empty())
        // github.com uses PR Marmot's own registration unless the file names
        // another; a GHES host has to bring its own.
        .or_else(|| prmarmot_core::github::device_flow::default_client_id(&host).map(str::to_owned))
        .unwrap_or_else(|| prmarmot_core::github::device_flow::PLACEHOLDER_CLIENT_ID.to_owned());
    let word_mode = |word: Option<String>, source: &str, warnings: &mut Vec<String>| {
        word.and_then(|word| match AuthMode::parse(&word) {
            Some(mode) => Some(mode),
            None => {
                warnings.push(format!(
                    "ignoring {source} auth mode {word:?}: use auto, gh, device, or token"
                ));
                None
            }
        })
    };
    let mode = cli_mode
        .or_else(|| word_mode(env("PRMARMOT_AUTH"), "PRMARMOT_AUTH", warnings))
        .or_else(|| word_mode(section.mode.clone(), "[auth] mode", warnings))
        .unwrap_or_default();
    let store = section
        .store
        .clone()
        .and_then(|word| match crate::auth::StoreKind::parse(&word) {
            Some(kind) => Some(kind),
            None => {
                warnings.push(format!(
                    "ignoring [auth] store {word:?}: use auto, keychain, or file"
                ));
                None
            }
        })
        .unwrap_or_default();
    AuthSettings {
        host: prmarmot_core::github::normalize_host(&host),
        client_id,
        mode,
        store,
        inline_token: env("PRMARMOT_TOKEN"),
    }
}

/// `$XDG_STATE_HOME/prmarmot` (default `~/.local/state/prmarmot`).
pub fn state_root() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prmarmot")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_reviewers_are_normalized_and_bad_keys_warn() {
        let file: FileConfig = toml::from_str(
            r#"
            default_reviewers = ["dana"]
            [repo_reviewers]
            "Acme" = ["olga", " "]
            "acme/API" = ["rita"]
            "quiet/repo" = []
            "a/b/c" = ["nope"]
            "" = ["nope"]
            "#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let resolved = repo_reviewers(&file.repo_reviewers, &mut warnings);
        assert_eq!(
            resolved.keys().collect::<Vec<_>>(),
            ["acme", "acme/api", "quiet/repo"]
        );
        assert_eq!(resolved["acme"], ["olga"]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");

        let config = BoardConfig {
            default_reviewers: file.default_reviewers.clone(),
            repo_reviewers: resolved,
            ..Default::default()
        };
        assert_eq!(config.suggested_reviewers("acme/api"), ["rita"]);
        assert_eq!(config.suggested_reviewers("ACME/web"), ["olga"]);
        assert!(config.suggested_reviewers("quiet/repo").is_empty());
        assert_eq!(config.suggested_reviewers("else/where"), ["dana"]);
    }

    #[test]
    fn stale_after_days_defaults_to_three_and_refuses_zero() {
        let days = |text: &str| {
            let (config, warning) = board_config(&toml::from_str(text).unwrap());
            (config.stale_after_days, warning.is_some())
        };
        assert_eq!(days(""), (3, false));
        assert_eq!(days("stale_after_days = 7"), (7, false));
        assert_eq!(days("stale_after_days = 0"), (3, true));
    }

    #[test]
    fn auth_settings_follow_env_over_file_and_report_typos() {
        let file: FileConfig = toml::from_str(
            r#"
            [auth]
            host = "ghe.acme.test"
            client_id = "ghes-client"
            mode = "device"
            store = "file"
            "#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let resolved = auth_settings(&file, None, None, &mut warnings);
        assert_eq!(resolved.host, "ghe.acme.test");
        assert_eq!(resolved.client_id, "ghes-client");
        assert_eq!(resolved.mode, AuthMode::Device);
        assert_eq!(resolved.store, crate::auth::StoreKind::File);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!resolved.client_id_is_placeholder());

        // A flag beats the file.
        let mut warnings = Vec::new();
        let cli = auth_settings(&file, Some("github.com"), Some(AuthMode::Gh), &mut warnings);
        assert_eq!(cli.host, "github.com");
        assert_eq!(cli.mode, AuthMode::Gh);

        let bad: FileConfig = toml::from_str(
            "[auth]
mode = 'magic'
store = 'vault'",
        )
        .unwrap();
        let mut warnings = Vec::new();
        let fallback = auth_settings(&bad, None, None, &mut warnings);
        assert_eq!(fallback.mode, AuthMode::Auto);
        assert_eq!(fallback.store, crate::auth::StoreKind::Auto);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn a_clean_config_signs_in_to_github_com_with_the_built_in_registration() {
        let mut warnings = Vec::new();
        let resolved = auth_settings(&FileConfig::default(), None, None, &mut warnings);
        assert_eq!(resolved.host, "github.com");
        assert_eq!(resolved.mode, AuthMode::Auto);
        assert_eq!(
            resolved.client_id,
            prmarmot_core::github::device_flow::GITHUB_COM_CLIENT_ID
        );
        assert!(!resolved.client_id_is_placeholder());
        assert!(warnings.is_empty());
    }

    #[test]
    fn an_enterprise_host_has_no_built_in_registration() {
        let file: FileConfig = toml::from_str("[auth]\nhost = 'github.example.com'").unwrap();
        let mut warnings = Vec::new();
        let resolved = auth_settings(&file, None, None, &mut warnings);
        assert_eq!(resolved.host, "github.example.com");
        assert!(
            resolved.client_id_is_placeholder(),
            "a GHES host must bring its own client_id, got {:?}",
            resolved.client_id
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn another_host_takes_its_own_client_id_unless_one_was_configured() {
        use prmarmot_core::github::device_flow::GITHUB_COM_CLIENT_ID;
        let github = AuthSettings {
            host: "github.com".into(),
            client_id: GITHUB_COM_CLIENT_ID.into(),
            mode: AuthMode::Auto,
            store: crate::auth::StoreKind::File,
            inline_token: None,
        };
        let ghe = github.for_host("https://GHE.acme.test/");
        assert_eq!(ghe.host, "ghe.acme.test");
        assert!(
            ghe.client_id_is_placeholder(),
            "github.com's ID stays behind"
        );
        assert_eq!(ghe.mode, AuthMode::Auto);
        assert_eq!(ghe.for_host("github.com").client_id, GITHUB_COM_CLIENT_ID);

        let configured = AuthSettings {
            client_id: "Iv1.acme".into(),
            ..github
        };
        assert_eq!(configured.for_host("ghe.acme.test").client_id, "Iv1.acme");
    }

    #[test]
    fn auth_modes_parse_the_documented_words() {
        assert_eq!(AuthMode::parse("gh"), Some(AuthMode::Gh));
        assert_eq!(AuthMode::parse("Device"), Some(AuthMode::Device));
        assert_eq!(AuthMode::parse("pat"), Some(AuthMode::Token));
        assert_eq!(AuthMode::parse("nonsense"), None);
        assert_eq!(AuthMode::default().word(), "auto");
    }

    #[test]
    fn scope_precedence_and_clean_default_are_explicit() {
        let clean = FileConfig::default();
        assert_eq!(
            resolve_scope(None, None, None, &clean),
            BoardScope::AllRepositories
        );
        let old: FileConfig = toml::from_str("repo = 'acme/legacy'").unwrap();
        assert_eq!(
            resolve_scope(None, None, None, &old),
            BoardScope::Repository("acme/legacy".into())
        );
        let all: FileConfig = toml::from_str("scope = 'all'\nrepo = 'acme/remembered'").unwrap();
        assert_eq!(
            resolve_scope(None, None, None, &all),
            BoardScope::AllRepositories
        );
        assert_eq!(
            resolve_scope(None, Some("env/repo".into()), Some("all"), &all),
            BoardScope::Repository("env/repo".into())
        );
        assert_eq!(
            resolve_scope(
                Some(BoardScope::AllRepositories),
                Some("env/repo".into()),
                Some("repo"),
                &old
            ),
            BoardScope::AllRepositories
        );
    }
}

impl FileConfig {
    /// The whole config as TOML text.
    ///
    /// The desktop edits `config.toml` in place with `toml_edit`, which keeps
    /// the user's comments and ordering; this writes a clean canonical file
    /// instead, for a front end that has no such file to edit (the iPad
    /// exporting its settings). Both are read back by `from_toml_str`.
    pub fn to_toml_string(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|error| error.to_string())
    }

    /// Parse a config file, e.g. one exported from the desktop.
    pub fn from_toml_str(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod toml_round_trip_tests {
    use super::*;

    /// A file with every section set, as the desktop would have written it.
    const FULL: &str = r#"
repo = "acme/widgets"
scope = "repo"
repos = ["acme/widgets", "acme/api"]
pinned_repos = ["acme/widgets"]
refresh_secs = 600
theme = "dark"
view = "authored"
default_reviewers = ["alice", "bob"]
notifications = false
notification_sound = false
notify_all_needs_action = true
dock_badge = false
automatic_update_checks = false
stale_after_days = 5

[repo_reviewers]
acme = ["carol"]
"acme/api" = ["dave"]

[issue_link]
pattern = "PROJ-[0-9]+"
url_template = "https://tracker.example.test/issues/{id}"

[auth]
host = "git.acme.test"
client_id = "Iv1.example"
mode = "device"
store = "file"
"#;

    #[test]
    fn a_config_survives_being_written_and_read_again() {
        let original = FileConfig::from_toml_str(FULL).unwrap();
        let text = original.to_toml_string().unwrap();
        let again = FileConfig::from_toml_str(&text).unwrap();

        assert_eq!(again.repo.as_deref(), Some("acme/widgets"));
        assert_eq!(again.scope.as_deref(), Some("repo"));
        assert_eq!(again.repos, vec!["acme/widgets", "acme/api"]);
        assert_eq!(again.pinned_repos, vec!["acme/widgets"]);
        assert_eq!(again.refresh_secs, Some(600));
        assert_eq!(again.theme.as_deref(), Some("dark"));
        assert_eq!(again.view.as_deref(), Some("authored"));
        assert_eq!(again.default_reviewers, vec!["alice", "bob"]);
        assert!(!again.notifications);
        assert!(!again.notification_sound);
        assert!(again.notify_all_needs_action);
        assert!(!again.dock_badge);
        assert!(!again.automatic_update_checks);
        assert_eq!(again.stale_after_days, Some(5));
        assert_eq!(again.repo_reviewers["acme"], vec!["carol"]);
        assert_eq!(again.repo_reviewers["acme/api"], vec!["dave"]);
        assert_eq!(
            again.issue_link.as_ref().map(|link| link.pattern.as_str()),
            Some("PROJ-[0-9]+")
        );
        let auth = again.auth.as_ref().unwrap();
        assert_eq!(auth.host.as_deref(), Some("git.acme.test"));
        assert_eq!(auth.mode.as_deref(), Some("device"));
    }

    /// The whole reason the fields are ordered the way they are: TOML wants
    /// every plain key before the first table.
    #[test]
    fn the_written_file_puts_tables_last() {
        let text = FileConfig::from_toml_str(FULL)
            .unwrap()
            .to_toml_string()
            .unwrap();
        let first_table = text.find("[repo_reviewers]").unwrap();
        for key in ["repo =", "notifications =", "stale_after_days ="] {
            assert!(
                text.find(key).unwrap() < first_table,
                "{key} is written after a table, which TOML cannot read back"
            );
        }
    }

    #[test]
    fn an_empty_config_writes_nothing_it_does_not_have() {
        let text = FileConfig::default().to_toml_string().unwrap();
        for absent in ["repo =", "repos =", "[auth]", "[issue_link]", "[window]"] {
            assert!(
                !text.contains(absent),
                "empty config wrote {absent}:\n{text}"
            );
        }
        // The booleans always have a value, so they are always written.
        assert!(text.contains("notifications = true"));
    }

    #[test]
    fn nonsense_is_an_error_and_not_a_default_config() {
        assert!(FileConfig::from_toml_str("this is not toml = = =").is_err());
        assert!(FileConfig::from_toml_str("refresh_secs = \"soon\"").is_err());
    }
}
