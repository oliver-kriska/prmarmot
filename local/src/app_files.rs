//! The desktop app's writes to its files: Settings and remembered state into
//! `config.toml` (comments and formatting kept, written atomically), the
//! one-time copy of the old `prboard` data directories, and the update
//! bookkeeping paths. The read model is [`crate::config`]. This lives here,
//! not in the GPUI binary, so its tests run with the fast suites and in CI.

use std::io;
use std::path::{Path, PathBuf};

use prmarmot_core::board::BoardScope;
use prmarmot_core::layout::{SectionKind, SectionOrder};

use crate::config::{config_path, load_reporting, FileConfig};

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
    /// `Some` writes `section_order`, and the default order removes it;
    /// `None` leaves whatever the file says as it was written.
    pub section_order: Option<SectionOrder>,
    /// `Some` writes `details_position`, and `auto` (the default) removes it.
    pub details_position: Option<crate::config::DetailsPosition>,
}

/// Save editable settings while preserving comments and unrelated keys.
/// Existing malformed or unreadable files are rejected, never replaced.
pub fn save_settings(update: &SettingsUpdate) -> Result<(), String> {
    save_settings_at(&config_path(), update)
}

fn save_settings_at(path: &Path, update: &SettingsUpdate) -> Result<(), String> {
    edit_config_at(path, Validate::Settings, |doc| apply_settings(doc, update))
}

fn apply_settings(doc: &mut toml_edit::DocumentMut, update: &SettingsUpdate) -> Result<(), String> {
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
                let table = table_mut(doc, "issue_link")
                    .ok_or_else(|| "issue_link in the config file is not a table".to_owned())?;
                table.insert("pattern", toml_edit::value(pattern.as_str()));
                table.insert("url_template", toml_edit::value(template.as_str()));
            }
            None => {
                doc.remove("issue_link");
            }
        }
    }
    if let Some(order) = &update.section_order {
        if order.is_default() {
            doc.remove("section_order");
        } else {
            doc["section_order"] =
                toml_edit::value(order.keys().into_iter().collect::<toml_edit::Array>());
        }
    }
    if let Some(position) = update.details_position {
        if position == crate::config::DetailsPosition::default() {
            doc.remove("details_position");
        } else {
            doc["details_position"] = toml_edit::value(position.key());
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
    Ok(())
}

/// How much of the existing file must make sense before it is rewritten.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Validate {
    /// Valid TOML is enough: remembering a window size or a view must not
    /// fail because some other value is one the app ignores.
    Toml,
    /// The Settings dialog also requires every value to be one the app reads,
    /// since it shows and saves them.
    Settings,
}

/// Read `config.toml`, let `edit` change it, and write it back atomically,
/// keeping comments and formatting. A file that exists but can't be read,
/// isn't TOML, or (for Settings) holds values the app rejects is never
/// replaced: the person's own edits win. Nothing is written when `edit`
/// changed nothing.
fn edit_config_at(
    path: &Path,
    validate: Validate,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>,
) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    if validate == Validate::Settings {
        toml::from_str::<FileConfig>(&text)
            .map_err(|e| format!("{} has invalid settings: {e}", path.display()))?;
    }
    edit(&mut doc)?;
    let updated = doc.to_string();
    if updated == text {
        return Ok(());
    }
    write_atomic(path, &updated).map_err(|e| format!("could not save {}: {e}", path.display()))
}

/// Replace `path` with `contents` through a temporary file and a rename, so a
/// crash or a full disk mid-write never leaves half a config. A symlinked
/// config (a dotfiles checkout) is written through to its target, never
/// replaced by a plain file.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    use std::io::Write;
    let target = match std::fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        ".{}.tmp-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        if let Ok(metadata) = std::fs::metadata(&target) {
            std::fs::set_permissions(&temp, metadata.permissions())?;
        }
        std::fs::rename(&temp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The table under `key`, created when absent. `None` when the file already
/// has `key` as something else (`window = 900`), which is left alone rather
/// than overwritten or indexed into.
fn table_mut<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    key: &str,
) -> Option<&'a mut dyn toml_edit::TableLike> {
    if doc.get(key).is_none() {
        doc[key] = toml_edit::table();
    }
    doc.get_mut(key)?.as_table_like_mut()
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

/// The most lines the banner shows. Past that, the last line counts the
/// rest and its tooltip lists them, so a long `[repo_reviewers]` table can't
/// push the board down the window.
pub const MAX_WARNING_LINES: usize = 4;

/// What the settings ignored, for the banner: the whole file, or single
/// values from it or from the environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigWarnings {
    pub file: Option<String>,
    pub values: Vec<String>,
}

impl ConfigWarnings {
    /// Load the file and say what it ignored. Values are checked even when
    /// the file is ignored, because the environment can still set them.
    pub fn load() -> (FileConfig, Self) {
        let (file, warning) = load_reporting();
        let values = crate::config::ignored_values(&file);
        (
            file,
            Self {
                file: warning,
                values,
            },
        )
    }

    pub fn is_empty(&self) -> bool {
        self.file.is_none() && self.values.is_empty()
    }

    /// The banner's lines as (shown, tooltip): each problem's first line,
    /// with its whole text as the tooltip, the ignored file first.
    pub fn banner_lines(&self) -> Vec<(String, String)> {
        let first_line = |text: &str| text.lines().next().unwrap_or(text).trim().to_owned();
        let mut lines: Vec<(String, String)> = self
            .file
            .iter()
            .map(|warning| (config_warning_line(warning), warning.clone()))
            .chain(self.values.iter().map(|warning| {
                (
                    prmarmot_core::status::upper_first(&first_line(warning)),
                    warning.clone(),
                )
            }))
            .collect();
        if lines.len() > MAX_WARNING_LINES {
            let rest = lines.split_off(MAX_WARNING_LINES - 1);
            let tooltip = rest
                .iter()
                .map(|(_, whole)| first_line(whole))
                .collect::<Vec<_>>()
                .join("\n");
            lines.push((format!("…and {} more", rest.len()), tooltip));
        }
        lines
    }
}

/// Persist one top-level string key (repo/theme/view) back to the config
/// file so a closed-and-reopened app comes back the same. `toml_edit` keeps
/// the user's comments and formatting intact; failures are logged, never
/// fatal — persistence is a convenience, not a dependency.
pub fn persist_str(key: &str, value: &str) {
    persist(|doc| {
        doc[key] = toml_edit::value(value);
        Ok(())
    });
}

pub fn persist_pins(pins: &[String]) {
    persist(|doc| {
        doc["pinned_repos"] = toml_edit::value(
            pins.iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
        Ok(())
    });
}

pub fn persist_scope(scope: &BoardScope) {
    persist(|doc| {
        set_scope(doc, scope);
        Ok(())
    });
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

fn set_auth_host(doc: &mut toml_edit::DocumentMut, host: &str) -> Result<(), String> {
    // An `[auth]` table rather than an inline one, unless the file already
    // has an `auth` of its own shape.
    table_mut(doc, "auth")
        .ok_or_else(|| "auth in the config file is not a table".to_owned())?
        .insert("host", toml_edit::value(host));
    Ok(())
}

/// Persist which sections `view` shows collapsed under `[collapsed_sections]`.
/// The default (only Snoozed) removes the view's entry, and the table goes
/// once it is empty, so an untouched file stays as it was.
pub fn persist_collapsed(view: &str, kinds: &[SectionKind]) {
    persist(|doc| set_collapsed(doc, view, kinds));
}

fn set_collapsed(
    doc: &mut toml_edit::DocumentMut,
    view: &str,
    kinds: &[SectionKind],
) -> Result<(), String> {
    if kinds == crate::config::DEFAULT_COLLAPSED {
        if let Some(table) = doc
            .get_mut("collapsed_sections")
            .and_then(|item| item.as_table_like_mut())
        {
            table.remove(view);
            if table.is_empty() {
                doc.remove("collapsed_sections");
            }
        }
        return Ok(());
    }
    table_mut(doc, "collapsed_sections")
        .ok_or_else(|| "collapsed_sections in the config file is not a table".to_owned())?
        .insert(
            view,
            toml_edit::value(
                kinds
                    .iter()
                    .map(SectionKind::key)
                    .collect::<toml_edit::Array>(),
            ),
        );
    Ok(())
}

/// Persist the window size under `[window]`.
pub fn persist_window(width: f32, height: f32) {
    persist(|doc| set_window(doc, width, height));
}

fn set_window(doc: &mut toml_edit::DocumentMut, width: f32, height: f32) -> Result<(), String> {
    let table = table_mut(doc, "window")
        .ok_or_else(|| "window in the config file is not a table".to_owned())?;
    table.insert("width", toml_edit::value(f64::from(width)));
    table.insert("height", toml_edit::value(f64::from(height)));
    Ok(())
}

/// Remembering where things were is a convenience, never a dependency: a
/// failure is logged and the app carries on.
fn persist(update: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>) {
    if let Err(e) = edit_config_at(&config_path(), Validate::Toml, update) {
        eprintln!("prmarmot: not saving the config: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{normalized_pins, MAX_PINNED_REPOS};

    #[test]
    fn the_banner_lists_the_ignored_file_first_then_each_value() {
        let warnings = ConfigWarnings {
            file: Some(
                "ignoring invalid /c/config.toml: TOML parse error at line 2, column 1\n  |".into(),
            ),
            values: vec![
                "ignoring stale_after_days = 0: use 1 or more".into(),
                "ignoring bad issue-link pattern \"DEMO-(\": unclosed group\nregex parse error:\n    DEMO-(\n".into(),
            ],
        };
        let lines = warnings.banner_lines();
        let shown: Vec<&str> = lines.iter().map(|(line, _)| line.as_str()).collect();
        assert_eq!(
            shown,
            [
                "config.toml ignored, using default settings — TOML parse error at line 2, column 1",
                "Ignoring stale_after_days = 0: use 1 or more",
                "Ignoring bad issue-link pattern \"DEMO-(\": unclosed group",
            ]
        );
        assert!(lines[2].1.contains("DEMO-("), "the tooltip has it all");
        assert!(ConfigWarnings::default().is_empty());
        assert!(ConfigWarnings::default().banner_lines().is_empty());
    }

    #[test]
    fn a_long_list_is_capped_and_counts_the_rest() {
        let warnings = ConfigWarnings {
            file: None,
            values: (1..=7).map(|n| format!("ignoring key {n}")).collect(),
        };
        let lines = warnings.banner_lines();
        assert_eq!(lines.len(), MAX_WARNING_LINES);
        assert_eq!(lines[2].0, "Ignoring key 3");
        assert_eq!(lines[3].0, "…and 4 more");
        assert_eq!(
            lines[3].1,
            "ignoring key 4\nignoring key 5\nignoring key 6\nignoring key 7"
        );
    }

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
        set_auth_host(&mut doc, "ghe.acme.test").unwrap();
        let out = doc.to_string();
        assert!(out.contains("# mine"), "{out}");
        assert!(out.contains("mode = \"auto\" # keep"), "{out}");
        let file: crate::config::FileConfig = toml::from_str(&out).unwrap();
        assert_eq!(
            file.auth.and_then(|auth| auth.host).as_deref(),
            Some("ghe.acme.test")
        );

        let mut empty = toml_edit::DocumentMut::new();
        set_auth_host(&mut empty, "ghe.acme.test").unwrap();
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
    fn details_position_is_written_and_automatic_removes_it() {
        use crate::config::DetailsPosition;
        let path = temp_config("details-position");
        std::fs::write(&path, "# mine\n[repo_reviewers]\n\"acme\" = [\"olga\"]\n").unwrap();
        let save = |position| {
            save_settings_at(
                &path,
                &SettingsUpdate {
                    details_position: Some(position),
                    ..Default::default()
                },
            )
            .unwrap();
            std::fs::read_to_string(&path).unwrap()
        };
        let saved = save(DetailsPosition::Right);
        let parsed: FileConfig = toml::from_str(&saved).unwrap();
        assert_eq!(
            crate::config::details_position(&parsed),
            DetailsPosition::Right
        );
        assert!(!save(DetailsPosition::Auto).contains("details_position"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn collapsed_sections_are_written_per_view_and_the_default_removes_them() {
        use prmarmot_core::board::Category;
        let mut doc: toml_edit::DocumentMut = "# mine\ntheme = \"light\"\n".parse().unwrap();
        let draft = SectionKind::Category(Category::Draft);
        set_collapsed(&mut doc, "my_prs", &[draft, SectionKind::Snoozed]).unwrap();
        set_collapsed(&mut doc, "review", &[]).unwrap();
        let parsed: FileConfig = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(
            crate::config::collapsed_sections(&parsed, "my_prs"),
            [draft, SectionKind::Snoozed]
        );
        assert!(crate::config::collapsed_sections(&parsed, "review").is_empty());
        set_collapsed(&mut doc, "my_prs", &[SectionKind::Snoozed]).unwrap();
        set_collapsed(&mut doc, "review", &[SectionKind::Snoozed]).unwrap();
        assert_eq!(doc.to_string(), "# mine\ntheme = \"light\"\n");
    }

    #[test]
    fn section_order_is_written_as_keys_and_the_default_removes_it() {
        let path = temp_config("section-order");
        std::fs::write(&path, "# mine\n[repo_reviewers]\n\"acme\" = [\"olga\"]\n").unwrap();
        let (order, _) = SectionOrder::from_keys(&["available", "await"]);
        save_settings_at(
            &path,
            &SettingsUpdate {
                section_order: Some(order.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# mine"), "{saved}");
        // A top-level key, not one inside the trailing table.
        let parsed: FileConfig = toml::from_str(&saved).unwrap();
        assert_eq!(crate::config::section_order(&parsed), order);
        assert_eq!(parsed.repo_reviewers["acme"], ["olga"]);

        // Untouched in Settings: the file keeps what it said.
        save_settings_at(&path, &SettingsUpdate::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);

        save_settings_at(
            &path,
            &SettingsUpdate {
                section_order: Some(SectionOrder::default()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("section_order"));
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
    fn remembering_state_never_replaces_a_file_it_cannot_read() {
        let path = temp_config("unreadable");
        // Not UTF-8: `read_to_string` fails, and the file must survive as is.
        let latin1 = b"# caf\xe9\ntheme = \"dark\"\n".to_vec();
        std::fs::write(&path, &latin1).unwrap();
        let result = edit_config_at(&path, Validate::Toml, |doc| set_window(doc, 900.0, 600.0));
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), latin1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_key_of_another_shape_is_left_alone_instead_of_panicking() {
        for (text, edit) in [
            (
                "window = 900\n",
                Box::new(|doc: &mut toml_edit::DocumentMut| set_window(doc, 900.0, 600.0))
                    as Box<dyn FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>>,
            ),
            (
                "auth = \"device\"\n",
                Box::new(|doc: &mut toml_edit::DocumentMut| set_auth_host(doc, "ghe.acme.test")),
            ),
            (
                "collapsed_sections = 1\n",
                Box::new(|doc: &mut toml_edit::DocumentMut| set_collapsed(doc, "review", &[])),
            ),
        ] {
            let path = temp_config("shape");
            std::fs::write(&path, text).unwrap();
            assert!(
                edit_config_at(&path, Validate::Toml, edit).is_err(),
                "{text}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn an_inline_window_table_is_updated_in_place() {
        let path = temp_config("inline");
        std::fs::write(&path, "window = { width = 1.0, height = 2.0 }\n").unwrap();
        edit_config_at(&path, Validate::Toml, |doc| set_window(doc, 900.0, 600.0)).unwrap();
        let file: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let window = file.window.unwrap();
        assert_eq!((window.width, window.height), (900.0, 600.0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_symlinked_config_is_written_through_to_its_target() {
        let dir = std::env::temp_dir().join(format!("prmarmot-config-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("dotfiles-config.toml");
        let link = dir.join("config.toml");
        std::fs::write(&target, "# from dotfiles\n").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        edit_config_at(&link, Validate::Toml, |doc| {
            doc["view"] = toml_edit::value("review");
            Ok(())
        })
        .unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(written.contains("# from dotfiles"), "{written}");
        assert!(written.contains("view = \"review\""), "{written}");
        std::fs::remove_dir_all(dir).unwrap();
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
