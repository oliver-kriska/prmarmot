//! File config at `~/.config/prboard/config.toml` (or `$XDG_CONFIG_HOME`).
//!
//! This is what makes Spotlight/Finder launches work: those carry no shell
//! env and start in `/`, so env vars and cwd-based repo detection both fail
//! there. Precedence everywhere: CLI arg > env var > config file > detection.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    /// Default repo (`owner/name`) when neither `--repo` nor `PRBOARD_REPO` is set.
    pub repo: Option<String>,
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
    pub issue_link: Option<IssueLinkSection>,
    pub window: Option<WindowSection>,
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

/// Editable values from the Settings dialog. `None` means an environment
/// variable owns that field, so saving must leave its TOML value untouched.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SettingsUpdate {
    pub default_reviewers: Option<Vec<String>>,
    pub refresh_secs: Option<u64>,
    pub theme: Option<String>,
    /// `Some(None)` removes `[issue_link]`; `None` preserves it.
    pub issue_link: Option<Option<(String, String)>>,
}

/// Save editable settings while preserving comments and unrelated keys.
/// Existing malformed or unreadable files are rejected, never replaced.
pub fn save_settings(update: &SettingsUpdate) -> Result<(), String> {
    save_settings_at(&config_path(), update)
}

fn save_settings_at(path: &Path, update: &SettingsUpdate) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    toml::from_str::<FileConfig>(&text)
        .map_err(|e| format!("{} has invalid settings: {e}", path.display()))?;

    if let Some(reviewers) = &update.default_reviewers {
        doc["default_reviewers"] = toml_edit::value(
            reviewers
                .iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
    }
    if let Some(secs) = update.refresh_secs {
        doc["refresh_secs"] = toml_edit::value(
            i64::try_from(secs).map_err(|_| "Refresh interval is too large".to_owned())?,
        );
    }
    if let Some(theme) = &update.theme {
        doc["theme"] = toml_edit::value(theme.as_str());
    }
    if let Some(issue_link) = &update.issue_link {
        match issue_link {
            Some((pattern, template)) => {
                doc["issue_link"]["pattern"] = toml_edit::value(pattern.as_str());
                doc["issue_link"]["url_template"] = toml_edit::value(template.as_str());
            }
            None => {
                doc.remove("issue_link");
            }
        }
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("could not save {}: {e}", path.display()))
}

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prboard")
        .join("config.toml")
}

/// Missing file → defaults; unparseable file → defaults with a warning
/// (a typo in the config must never make the app unlaunchable).
pub fn load() -> FileConfig {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return FileConfig::default();
    };
    match toml::from_str(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("prboard: ignoring invalid {}: {e}", path.display());
            FileConfig::default()
        }
    }
}

/// Persist one top-level string key (repo/theme/view) back to the config
/// file so a closed-and-reopened app comes back the same. `toml_edit` keeps
/// the user's comments and formatting intact; failures are logged, never
/// fatal — persistence is a convenience, not a dependency.
pub fn persist_str(key: &str, value: &str) {
    persist(|doc| {
        doc[key] = toml_edit::value(value);
    });
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

pub fn persist_pins(pins: &[String]) {
    persist(|doc| {
        doc["pinned_repos"] = toml_edit::value(
            pins.iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
    });
}

/// Persist the window size under `[window]`.
pub fn persist_window(width: f32, height: f32) {
    persist(|doc| {
        doc["window"]["width"] = toml_edit::value(width as f64);
        doc["window"]["height"] = toml_edit::value(height as f64);
    });
}

fn persist(update: impl FnOnce(&mut toml_edit::DocumentMut)) {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = match text.parse::<toml_edit::DocumentMut>() {
        Ok(doc) => doc,
        Err(e) => {
            // Never clobber a file we can't parse — the user's edits win.
            eprintln!(
                "prboard: not saving into unparseable {}: {e}",
                path.display()
            );
            return;
        }
    };
    update(&mut doc);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        eprintln!("prboard: could not save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "prboard-config-{name}-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    #[test]
    fn pins_are_optional_ordered_unique_and_bounded() {
        let old: FileConfig = toml::from_str("repo = 'acme/api'").unwrap();
        assert!(old.pinned_repos.is_empty());
        let mut input = vec![" acme/api ".into(), "ACME/API".into(), "".into()];
        input.extend((0..20).map(|n| format!("acme/repo{n}")));
        let pins = normalized_pins(&input);
        assert_eq!(pins.len(), MAX_PINNED_REPOS);
        assert_eq!(&pins[..2], &["acme/api", "acme/repo0"]);
        let mut doc = "repo = 'acme/api'\n# keep this\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        doc["pinned_repos"] = toml_edit::value(
            pins.iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
        assert!(doc.to_string().contains("# keep this"));
        let parsed: FileConfig = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(parsed.pinned_repos, pins);
    }

    #[test]
    fn settings_preserve_comments_unrelated_keys_and_disabled_fields() {
        let path = temp_config("preserve");
        std::fs::write(
            &path,
            "# mine\nrepo = 'acme/api'\nrefresh_secs = 99\ntheme = 'dark'\n",
        )
        .unwrap();
        save_settings_at(
            &path,
            &SettingsUpdate {
                default_reviewers: Some(vec!["alice".into()]),
                refresh_secs: None,
                theme: Some("light".into()),
                issue_link: Some(None),
            },
        )
        .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# mine"));
        assert!(saved.contains("repo = 'acme/api'"));
        assert!(saved.contains("refresh_secs = 99"));
        assert!(saved.contains("theme = \"light\""));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn settings_never_overwrite_malformed_files() {
        let path = temp_config("malformed");
        let malformed = "repo = [not toml";
        std::fs::write(&path, malformed).unwrap();
        let result = save_settings_at(&path, &SettingsUpdate::default());
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), malformed);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn settings_reject_invalid_table_types_and_numeric_overflow() {
        let path = temp_config("invalid-types");
        std::fs::write(&path, "issue_link = 42").unwrap();
        assert!(save_settings_at(&path, &SettingsUpdate::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "issue_link = 42");
        std::fs::write(&path, "# preserved\n").unwrap();
        assert!(save_settings_at(
            &path,
            &SettingsUpdate {
                refresh_secs: Some(u64::MAX),
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# preserved\n");
        std::fs::remove_file(path).unwrap();
    }
}
