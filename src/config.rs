//! File config at `~/.config/prboard/config.toml` (or `$XDG_CONFIG_HOME`).
//!
//! This is what makes Spotlight/Finder launches work: those carry no shell
//! env and start in `/`, so env vars and cwd-based repo detection both fail
//! there. Precedence everywhere: CLI arg > env var > config file > detection.

use std::path::PathBuf;

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

#[derive(Debug, Deserialize)]
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
}
