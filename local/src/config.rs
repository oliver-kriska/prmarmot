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
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct FileConfig {
    /// Default repo (`owner/name`) when neither `--repo` nor `PRMARMOT_REPO` is set.
    pub repo: Option<String>,
    /// `all` or `repo`. Absent keeps old configs compatible: a saved repo is
    /// specific, while a clean config defaults to all repositories.
    pub scope: Option<String>,
    /// Entries for the repo picker; the active repo is always included.
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub pinned_repos: Vec<String>,
    pub refresh_secs: Option<u64>,
    /// `system` | `light` | `dark`.
    pub theme: Option<String>,
    /// `authored` | `review` — the view to open with.
    pub view: Option<String>,
    #[serde(default)]
    pub default_reviewers: Vec<String>,
    /// `[repo_reviewers]`: suggestions per owner (`"acme"`) or repository
    /// (`"acme/api"`), used before `default_reviewers`.
    #[serde(default)]
    pub repo_reviewers: BTreeMap<String, Vec<String>>,
    pub issue_link: Option<IssueLinkSection>,
    pub window: Option<WindowSection>,
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
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WindowSection {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Deserialize)]
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
