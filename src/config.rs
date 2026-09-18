//! File config at `~/.config/prmarmot/config.toml` (or `$XDG_CONFIG_HOME`).
//!
//! This is what makes Spotlight/Finder launches work: those carry no shell
//! environment. Precedence is CLI > environment > config; when no scope is
//! configured, the app opens all repositories involving the signed-in user.

use std::io;
use std::path::{Path, PathBuf};

use prmarmot_core::board::BoardScope;

pub use prmarmot_local::config::{
    config_path, load, load_reporting, normalized_pins, resolve_scope, FileConfig, MAX_PINNED_REPOS,
};

/// Editable values from the Settings dialog. `None` means an environment
/// variable owns that field, so saving must leave its TOML value untouched.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SettingsUpdate {
    pub default_reviewers: Option<Vec<String>>,
    pub refresh_secs: Option<u64>,
    pub theme: Option<String>,
    /// `Some(None)` removes `[issue_link]`; `None` preserves it.
    pub issue_link: Option<Option<(String, String)>>,
    pub notifications: Option<bool>,
    pub notification_sound: Option<bool>,
    pub notify_all_needs_action: Option<bool>,
    pub dock_badge: Option<bool>,
    pub automatic_update_checks: Option<bool>,
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
    for (key, value) in [
        ("notifications", update.notifications),
        ("notification_sound", update.notification_sound),
        ("notify_all_needs_action", update.notify_all_needs_action),
        ("dock_badge", update.dock_badge),
        ("automatic_update_checks", update.automatic_update_checks),
    ] {
        if let Some(value) = value {
            doc[key] = toml_edit::value(value);
        }
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("could not save {}: {e}", path.display()))
}

/// Copy legacy directories once, leaving the originals untouched. Stage the
/// entire copy before making it visible so a failed copy is retryable.
pub fn migrate_legacy_data() -> io::Result<()> {
    let config = config_path();
    let state = update_paths();
    for path in [&config, &state.check_state] {
        let root = path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing data root"))?;
        if migrate_directory(root)? {
            eprintln!(
                "prmarmot: copied legacy data into {}",
                root.join("prmarmot").display()
            );
        }
    }
    Ok(())
}

fn migrate_directory(root: &Path) -> io::Result<bool> {
    let old = root.join("prboard");
    let new = root.join("prmarmot");
    if new.try_exists()? || !old.try_exists()? {
        return Ok(false);
    }
    let staging = root.join(format!(".prmarmot-migration-{}", std::process::id()));
    std::fs::create_dir(&staging)?;
    let result = (|| {
        copy_directory(&old, &staging)?;
        std::fs::rename(&staging, &new)?;
        Ok(true)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn copy_directory(source: &Path, destination: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        // Process bookkeeping must not cross installations. The update cache
        // also contains release URLs belonging to the old repository identity.
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".lock")
            || name.ends_with(".tmp")
            || name.starts_with("update-helper")
            || name == "updates.toml"
        {
            continue;
        }
        if kind.is_dir() {
            std::fs::create_dir(&target)?;
            copy_directory(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "cannot migrate non-regular entry {}; copy it manually",
                    entry.path().display()
                ),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct UpdatePaths {
    pub check_state: PathBuf,
    pub upgrade_receipt: PathBuf,
    pub helper_lock: PathBuf,
}

/// Update bookkeeping is installation-global and deliberately separate from
/// account-namespaced PR attention state.
pub fn update_paths() -> UpdatePaths {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prmarmot");
    UpdatePaths {
        check_state: root.join("updates.toml"),
        upgrade_receipt: root.join("update-receipt.toml"),
        helper_lock: root.join("update-helper.lock"),
    }
}

/// The banner line for a config file the app ignored: that it was ignored,
/// then why, with no path, so a narrow window still shows where the error
/// is. The path and the TOML excerpt are in the tooltip.
pub fn config_warning_line(warning: &str) -> String {
    let first = warning.lines().next().unwrap_or(warning).trim();
    let why = first.rsplit_once(": ").map_or(first, |(_, why)| why);
    format!("config.toml ignored, using default settings — {why}")
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

pub fn persist_pins(pins: &[String]) {
    persist(|doc| {
        doc["pinned_repos"] = toml_edit::value(
            pins.iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
    });
}

pub fn persist_scope(scope: &BoardScope) {
    persist(|doc| set_scope(doc, scope));
}

fn set_scope(doc: &mut toml_edit::DocumentMut, scope: &BoardScope) {
    match scope {
        BoardScope::AllRepositories => doc["scope"] = toml_edit::value("all"),
        BoardScope::Repository(repo) => {
            doc["scope"] = toml_edit::value("repo");
            doc["repo"] = toml_edit::value(repo.as_str());
        }
    }
}

/// Persist the host signed in to from the sign-in screen under `[auth]`, so the
/// next launch connects there too.
pub fn persist_auth_host(host: &str) {
    persist(|doc| set_auth_host(doc, host));
}

fn set_auth_host(doc: &mut toml_edit::DocumentMut, host: &str) {
    // An `[auth]` table rather than an inline one, unless the file already
    // has an `auth` of its own shape.
    if doc.get("auth").is_none() {
        doc["auth"] = toml_edit::table();
    }
    doc["auth"]["host"] = toml_edit::value(host);
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
                "prmarmot: not saving into unparseable {}: {e}",
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
        eprintln!("prmarmot: could not save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ignored_config_file_says_so_in_one_line() {
        let warning = "ignoring invalid /home/me/.config/prmarmot/config.toml: TOML parse error at line 1, column 16\n  |\n1 | refresh_secs = \"five\"\n  |                ^^^^^^\ninvalid type: string \"five\", expected u64\n";
        assert_eq!(
            config_warning_line(warning),
            "config.toml ignored, using default settings — TOML parse error at line 1, column 16"
        );
        assert_eq!(
            config_warning_line(
                "could not read /home/me/.config/prmarmot/config.toml: Permission denied (os error 13)"
            ),
            "config.toml ignored, using default settings — Permission denied (os error 13)"
        );
    }

    fn temp_config(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "prmarmot-config-{name}-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    #[test]
    fn a_signed_in_host_lands_in_the_auth_table_and_keeps_the_rest() {
        let text = "# mine\ntheme = 'dark'\n\n[auth]\nmode = \"auto\" # keep\n";
        let mut doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        set_auth_host(&mut doc, "ghe.acme.test");
        let out = doc.to_string();
        assert!(out.contains("# mine"), "{out}");
        assert!(out.contains("mode = \"auto\" # keep"), "{out}");
        let file: prmarmot_local::config::FileConfig = toml::from_str(&out).unwrap();
        assert_eq!(
            file.auth.and_then(|auth| auth.host).as_deref(),
            Some("ghe.acme.test")
        );

        let mut empty = toml_edit::DocumentMut::new();
        set_auth_host(&mut empty, "ghe.acme.test");
        assert!(empty.to_string().contains("[auth]"), "{empty}");
    }

    #[test]
    fn migration_preserves_bytes_and_originals_and_never_overwrites_new_data() {
        let root = temp_config("migration");
        let old = root.join("prboard");
        let new = root.join("prmarmot");
        assert!(!migrate_directory(&root).unwrap());
        std::fs::create_dir_all(old.join("nested")).unwrap();
        let config = "# keep my comments\ntheme = 'dark'\nrepo = 'acme/api'\n";
        std::fs::write(old.join("config.toml"), config).unwrap();
        let attention = b"{\"watches\":[\"acme/api#17\"],\"snoozes\":[\"acme/web#42\"]}";
        std::fs::write(old.join("attention-github.com-me.json"), attention).unwrap();
        std::fs::write(old.join("nested/custom.txt"), "preserved").unwrap();
        std::fs::write(old.join("update-helper.lock"), "old lock").unwrap();
        std::fs::write(old.join("updates.toml"), "old release URL").unwrap();
        assert!(migrate_directory(&root).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read(new.join("attention-github.com-me.json")).unwrap(),
            attention
        );
        assert_eq!(
            std::fs::read_to_string(new.join("nested/custom.txt")).unwrap(),
            "preserved"
        );
        assert!(!new.join("update-helper.lock").exists());
        assert!(!new.join("updates.toml").exists());
        assert!(old.join("update-helper.lock").exists());
        assert_eq!(
            std::fs::read_to_string(old.join("config.toml")).unwrap(),
            config
        );
        std::fs::write(new.join("config.toml"), "theme = 'light'").unwrap();
        std::fs::remove_file(new.join("attention-github.com-me.json")).unwrap();
        assert!(!migrate_directory(&root).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            "theme = 'light'"
        );
        assert!(!new.join("attention-github.com-me.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_migration_leaves_no_partial_destination_and_can_be_retried() {
        let root = temp_config("failed-migration");
        let old = root.join("prboard");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "theme = 'dark'").unwrap();
        std::os::unix::fs::symlink("missing-target", old.join("link")).unwrap();
        assert!(migrate_directory(&root).is_err());
        assert!(!root.join("prmarmot").exists());
        assert!(!root
            .join(format!(".prmarmot-migration-{}", std::process::id()))
            .exists());
        std::fs::remove_file(old.join("link")).unwrap();
        assert!(migrate_directory(&root).unwrap());
        assert_eq!(
            std::fs::read_to_string(root.join("prmarmot/config.toml")).unwrap(),
            "theme = 'dark'"
        );
        std::fs::remove_dir_all(root).unwrap();
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
    fn persisted_all_scope_keeps_the_last_repository_for_switch_back() {
        let mut doc = "repo = 'acme/remembered'\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        set_scope(&mut doc, &BoardScope::AllRepositories);
        let parsed: FileConfig = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(parsed.scope.as_deref(), Some("all"));
        assert_eq!(parsed.repo.as_deref(), Some("acme/remembered"));

        set_scope(&mut doc, &BoardScope::Repository("other/repo".into()));
        let parsed: FileConfig = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(parsed.scope.as_deref(), Some("repo"));
        assert_eq!(parsed.repo.as_deref(), Some("other/repo"));
    }

    #[test]
    fn settings_preserve_comments_unrelated_keys_and_disabled_fields() {
        let path = temp_config("preserve");
        std::fs::write(
            &path,
            "# mine\nrepo = 'acme/api'\nrefresh_secs = 99\ntheme = 'dark'\n\n\
             [repo_reviewers]\n\"acme\" = [\"olga\"]\n",
        )
        .unwrap();
        save_settings_at(
            &path,
            &SettingsUpdate {
                default_reviewers: Some(vec!["alice".into()]),
                refresh_secs: None,
                theme: Some("light".into()),
                issue_link: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# mine"));
        assert!(saved.contains("repo = 'acme/api'"));
        assert!(saved.contains("refresh_secs = 99"));
        assert!(saved.contains("theme = \"light\""));
        // New top-level keys must not land inside a trailing table.
        let parsed: FileConfig = toml::from_str(&saved).unwrap();
        assert_eq!(parsed.default_reviewers, ["alice"]);
        assert_eq!(parsed.repo_reviewers["acme"], ["olga"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn automatic_update_checks_default_on_and_persist() {
        let old: FileConfig = toml::from_str("repo = 'acme/api'").unwrap();
        assert!(old.automatic_update_checks);

        let path = temp_config("automatic-updates");
        save_settings_at(
            &path,
            &SettingsUpdate {
                automatic_update_checks: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        let saved: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!saved.automatic_update_checks);
        std::fs::remove_file(path).unwrap();
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
