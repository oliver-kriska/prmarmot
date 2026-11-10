//! The desktop app's writes to its files: Settings and remembered state into
//! `config.toml` (comments and formatting kept, written atomically), the
//! one-time copy of the old `prboard` data directories, and the update
//! bookkeeping paths. The read model is [`crate::config`]. This lives here,
//! not in the GPUI binary, so its tests run with the fast suites and in CI.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use prmarmot_core::board::BoardScope;
use prmarmot_core::layout::{SectionKind, SectionOrder};

use crate::config::{config_path, load_reporting, FileConfig};

/// Restrict existing board/attention snapshots (including interrupted writes)
/// before the desktop opens them. Readers such as the CLI never call this.
pub fn secure_state_files(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let name = name.strip_prefix('.').unwrap_or(&name);
            if (name.starts_with("board-") || name.starts_with("attention-"))
                && (name.ends_with(".json") || name.ends_with(".tmp"))
                && entry.file_type()?.is_file()
            {
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
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
    /// `Some` writes `colors`, and Classic (the default) removes it.
    pub colors: Option<crate::config::Colors>,
    /// `Some` writes `skin`, and Classic (the default) removes it.
    pub skin: Option<crate::config::Skin>,
}

/// Save editable settings while preserving comments and unrelated keys.
/// Existing malformed or unreadable files are rejected, never replaced.
/// Written after every remembered value queued before it, and on the
/// calling thread, so the dialog can show an error.
pub fn save_settings(update: &SettingsUpdate) -> Result<(), String> {
    writer().with_file(|path| save_settings_at(path, update))
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
    if let Some(colors) = &update.colors {
        if *colors == crate::config::Colors::Classic {
            doc.remove("colors");
        } else {
            doc["colors"] = toml_edit::value(colors.key());
        }
    }
    if let Some(skin) = update.skin {
        if skin == crate::config::Skin::Classic {
            doc.remove("skin");
        } else {
            doc["skin"] = toml_edit::value(skin.key());
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
    let (name, value) = (key.to_owned(), value.to_owned());
    persist(key, move |doc| {
        doc[name.as_str()] = toml_edit::value(value);
        Ok(())
    });
}

pub fn persist_pins(pins: &[String]) {
    let pins = pins.to_vec();
    persist("pinned_repos", move |doc| {
        doc["pinned_repos"] = toml_edit::value(
            pins.iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
        Ok(())
    });
}

pub fn persist_scope(scope: &BoardScope) {
    let scope = scope.clone();
    persist("scope", move |doc| {
        set_scope(doc, &scope);
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
    let host = host.to_owned();
    persist("auth.host", move |doc| set_auth_host(doc, &host));
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
    let (view, kinds) = (view.to_owned(), kinds.to_vec());
    persist(&format!("collapsed_sections.{view}"), move |doc| {
        set_collapsed(doc, &view, &kinds)
    });
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

/// Persist the window size under `[window]`. It is written when the app
/// quits, so this one waits: it and every write queued before it are on disk
/// when it returns (`ConfigWriter::flush`).
pub fn persist_window(width: f32, height: f32) {
    persist("window", move |doc| set_window(doc, width, height));
    flush_config_writes();
}

fn set_window(doc: &mut toml_edit::DocumentMut, width: f32, height: f32) -> Result<(), String> {
    let table = table_mut(doc, "window")
        .ok_or_else(|| "window in the config file is not a table".to_owned())?;
    table.insert("width", toml_edit::value(f64::from(width)));
    table.insert("height", toml_edit::value(f64::from(height)));
    Ok(())
}

/// Remembering where things were is a convenience, never a dependency: the
/// edit is queued for the config writer and the caller carries on; a failure
/// is logged, as it always was.
fn persist(
    key: &str,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String> + Send + 'static,
) {
    writer().submit(key, edit);
}

/// The app's one config writer, started on first use, writing to
/// [`config_path`].
static WRITER: OnceLock<ConfigWriter> = OnceLock::new();

fn writer() -> &'static ConfigWriter {
    WRITER.get_or_init(|| ConfigWriter::new(config_path()))
}

/// Wait until every remembered value queued so far is on disk. Quitting
/// calls it; a process that never queued one returns at once.
pub fn flush_config_writes() {
    if let Some(writer) = WRITER.get() {
        writer.flush();
    }
}

/// [`flush_config_writes`] for a window that is about to read the file: waits
/// at most `timeout`, so a write that hangs (a slow or network home
/// directory) never freezes the window. False when it gave up; the file then
/// may not have the latest queued value yet.
pub fn flush_config_writes_within(timeout: Duration) -> bool {
    WRITER
        .get()
        .is_none_or(|writer| writer.flush_within(timeout))
}

/// One queued edit of the config document.
type ConfigEdit = Box<dyn FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String> + Send>;

/// The longest [`ConfigWriter::flush`] waits, so a quit is never held
/// forever by a write that cannot finish (a hung network home directory).
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// Writes remembered values (view, scope, pins, folds, theme, auth host,
/// window size) to `config.toml` on a thread of its own, so the UI thread
/// never waits on a read, a parse and an `fsync`. Edits are applied in the
/// order they were queued; a key queued again before the last one was
/// written replaces it (and moves to the end), so a burst of view switches is
/// one write of the last view. Each write is the same read-edit-atomic
/// rename as [`edit_config_at`], one per batch. Bounded: one pending edit per
/// key, and the keys are a fixed handful.
pub struct ConfigWriter {
    shared: Arc<WriterShared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct WriterShared {
    path: PathBuf,
    queue: Mutex<WriterQueue>,
    /// Signalled when an edit is queued and when a batch is written.
    changed: Condvar,
    /// Held across each read-edit-write of the file, by the writer thread and
    /// by [`ConfigWriter::with_file`], so two never interleave.
    file: Mutex<()>,
}

#[derive(Default)]
struct WriterQueue {
    /// Edits not yet taken by the writer, oldest first, one per key.
    pending: Vec<(String, ConfigEdit)>,
    /// How many edits were ever queued, coalesced ones included.
    queued: u64,
    /// How many of those have been written (or failed and been logged).
    written: u64,
    shutdown: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ConfigWriter {
    /// A writer for `path`. If its thread can't be started, edits are
    /// written on the caller's thread instead, as before.
    pub fn new(path: PathBuf) -> Self {
        let shared = Arc::new(WriterShared {
            path,
            queue: Mutex::new(WriterQueue::default()),
            changed: Condvar::new(),
            file: Mutex::new(()),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("prmarmot-config".into())
                .spawn(move || run_writer(&shared))
        };
        let thread = match thread {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("prmarmot: writing the config on the UI thread: {e}");
                None
            }
        };
        Self { shared, thread }
    }

    /// Queue `edit` under `key`, replacing an edit of the same key that has
    /// not been written yet.
    pub fn submit(
        &self,
        key: &str,
        edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String> + Send + 'static,
    ) {
        if self.thread.is_none() {
            let _file = lock(&self.shared.file);
            write_batch(&self.shared.path, vec![(key.to_owned(), Box::new(edit))]);
            return;
        }
        let mut queue = lock(&self.shared.queue);
        queue.pending.retain(|(queued, _)| queued != key);
        queue.pending.push((key.to_owned(), Box::new(edit)));
        queue.queued += 1;
        self.shared.changed.notify_all();
    }

    /// Wait until every edit queued before this call is written, or
    /// [`FLUSH_TIMEOUT`] has passed. Returns whether they all were.
    pub fn flush(&self) -> bool {
        self.flush_within(FLUSH_TIMEOUT)
    }

    fn flush_within(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut queue = lock(&self.shared.queue);
        let target = queue.queued;
        while queue.written < target {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            queue = self
                .shared
                .changed
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Run `f` on the file itself, on this thread, after every queued edit
    /// is written and with no other write in between: the Settings save,
    /// whose error the dialog shows.
    pub fn with_file<T>(&self, f: impl FnOnce(&Path) -> T) -> T {
        self.flush();
        let _file = lock(&self.shared.file);
        f(&self.shared.path)
    }
}

impl Drop for ConfigWriter {
    /// Write what is queued, then stop the thread.
    fn drop(&mut self) {
        lock(&self.shared.queue).shutdown = true;
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_writer(shared: &WriterShared) {
    loop {
        let (batch, upto) = {
            let mut queue = lock(&shared.queue);
            while queue.pending.is_empty() && !queue.shutdown {
                queue = shared
                    .changed
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if queue.pending.is_empty() {
                return; // shut down with nothing left to write
            }
            (std::mem::take(&mut queue.pending), queue.queued)
        };
        {
            let _file = lock(&shared.file);
            // A panic in one batch must not leave `flush` waiting on it.
            let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                write_batch(&shared.path, batch);
            }));
            if written.is_err() {
                eprintln!("prmarmot: not saving the config: the writer panicked");
            }
        }
        let mut queue = lock(&shared.queue);
        queue.written = upto;
        shared.changed.notify_all();
    }
}

/// Apply `batch` in order to one read of the file and write it once. An edit
/// that fails is logged and skipped, the others still land; a file that
/// can't be read or isn't TOML is never replaced.
fn write_batch(path: &Path, batch: Vec<(String, ConfigEdit)>) {
    let result = edit_config_at(path, Validate::Toml, |doc| {
        for (_, edit) in batch {
            if let Err(e) = edit(doc) {
                eprintln!("prmarmot: not saving the config: {e}");
            }
        }
        Ok(())
    });
    if let Err(e) = result {
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

    #[cfg(unix)]
    #[test]
    fn old_snapshots_become_private_without_changing_contents_or_following_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = temp_config("private-state");
        secure_state_files(&root).unwrap(); // A clean install creates nothing.
        assert!(!root.exists());
        std::fs::create_dir(&root).unwrap();
        for name in [
            "board-github.com-me.json",
            "attention-github.com-other.json",
            ".board-github.com-me.json.1.tmp",
            "unrelated.json",
        ] {
            let path = root.join(name);
            std::fs::write(&path, b"preserve even corrupt data").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        symlink(
            root.join("unrelated.json"),
            root.join("attention-link.json"),
        )
        .unwrap();
        secure_state_files(&root).unwrap();
        secure_state_files(&root).unwrap(); // Idempotent across launches.
        for name in [
            "board-github.com-me.json",
            "attention-github.com-other.json",
            ".board-github.com-me.json.1.tmp",
            "unrelated.json",
        ] {
            let path = root.join(name);
            let expected = if name == "unrelated.json" {
                0o644
            } else {
                0o600
            };
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                expected
            );
            assert_eq!(std::fs::read(path).unwrap(), b"preserve even corrupt data");
        }
        assert!(std::fs::symlink_metadata(root.join("attention-link.json"))
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(root).unwrap();
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
    fn colors_and_skin_are_written_and_classic_removes_them() {
        use crate::config::{Colors, Skin};
        let path = temp_config("look");
        std::fs::write(
            &path,
            "# mine
theme = \"dark\"\n",
        )
        .unwrap();
        let save = |colors, skin| {
            save_settings_at(
                &path,
                &SettingsUpdate {
                    colors: Some(colors),
                    skin: Some(skin),
                    ..Default::default()
                },
            )
            .unwrap();
            std::fs::read_to_string(&path).unwrap()
        };
        let saved = save(Colors::Custom("Catppuccin Mocha".into()), Skin::Glacier);
        let parsed: FileConfig = toml::from_str(&saved).unwrap();
        assert_eq!(
            crate::config::colors(&parsed),
            Colors::Custom("Catppuccin Mocha".into())
        );
        assert_eq!(crate::config::skin(&parsed), Skin::Glacier);
        assert!(saved.starts_with("# mine\ntheme = \"dark\"\n"), "{saved}");
        let saved = save(Colors::Studio, Skin::Classic);
        assert!(saved.contains("colors = \"studio\""));
        assert!(!saved.contains("skin"));
        let saved = save(Colors::Classic, Skin::Classic);
        assert!(
            !saved.contains("colors") && !saved.contains("skin"),
            "{saved}"
        );
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

    /// A config path in a directory of its own, removed with the directory.
    fn writer_dir(name: &str) -> (PathBuf, PathBuf) {
        let dir = temp_config(&format!("writer-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        (dir, path)
    }

    fn set_x(value: i64) -> impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String> + Send {
        move |doc| {
            doc["x"] = toml_edit::value(value);
            Ok(())
        }
    }

    fn read_x(path: &Path) -> Option<i64> {
        let text = std::fs::read_to_string(path).ok()?;
        text.parse::<toml_edit::DocumentMut>()
            .ok()?
            .get("x")?
            .as_integer()
    }

    #[test]
    fn the_writer_applies_edits_in_the_order_they_were_queued() {
        let (dir, path) = writer_dir("order");
        std::fs::write(&path, "# keep this\n").unwrap();
        let writer = ConfigWriter::new(path.clone());
        // Two keys writing the same value: the later one wins, and a key
        // queued again moves behind the ones queued since.
        writer.submit("a", set_x(1));
        writer.submit("b", set_x(2));
        assert!(writer.flush());
        assert_eq!(read_x(&path), Some(2));
        let file = writer.shared.file.lock().unwrap();
        writer.submit("a", set_x(3));
        writer.submit("b", set_x(4));
        writer.submit("a", set_x(5));
        drop(file);
        assert!(writer.flush());
        assert_eq!(read_x(&path), Some(5));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("# keep this"));
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_writer_keeps_only_the_latest_value_per_key() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (dir, path) = writer_dir("coalesce");
        let writer = ConfigWriter::new(path.clone());
        let runs = Arc::new(AtomicUsize::new(0));
        // Hold the file so nothing is written while the burst is queued.
        let file = writer.shared.file.lock().unwrap();
        for view in ["authored", "review", "all", "review"] {
            let runs = Arc::clone(&runs);
            writer.submit("view", move |doc| {
                runs.fetch_add(1, Ordering::SeqCst);
                doc["view"] = toml_edit::value(view);
                Ok(())
            });
        }
        writer.submit("theme", |doc| {
            doc["theme"] = toml_edit::value("dark");
            Ok(())
        });
        drop(file);
        assert!(writer.flush());
        // The first may have been taken before the file was free; the
        // middle two never run.
        assert!(runs.load(Ordering::SeqCst) <= 2);
        let parsed: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.view.as_deref(), Some("review"));
        assert_eq!(parsed.theme.as_deref(), Some("dark"));
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn flush_waits_for_every_queued_write_and_drop_writes_the_rest() {
        let (dir, path) = writer_dir("flush");
        let writer = ConfigWriter::new(path.clone());
        assert!(writer.flush(), "nothing queued returns at once");
        let file = writer.shared.file.lock().unwrap();
        writer.submit("a", set_x(7));
        // The file is held, so the write can't land in time.
        assert!(!writer.flush_within(Duration::from_millis(50)));
        assert_eq!(read_x(&path), None);
        drop(file);
        assert!(writer.flush());
        assert_eq!(read_x(&path), Some(7));

        // A value queued just before quitting is written by the quit's own
        // synchronous write, or by dropping the writer.
        writer.submit("view", |doc| {
            doc["view"] = toml_edit::value("all");
            Ok(())
        });
        writer.submit("window", |doc| set_window(doc, 900.0, 600.0));
        assert!(writer.flush());
        let parsed: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.view.as_deref(), Some("all"));
        assert_eq!(parsed.window.map(|w| w.width), Some(900.0));
        writer.submit("a", set_x(8));
        drop(writer);
        assert_eq!(read_x(&path), Some(8));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_settings_save_follows_the_queued_writes_and_reports_its_error() {
        let (dir, path) = writer_dir("settings");
        let writer = ConfigWriter::new(path.clone());
        let file = writer.shared.file.lock().unwrap();
        writer.submit("a", set_x(1));
        drop(file);
        let seen = writer.with_file(read_x);
        assert_eq!(seen, Some(1), "the queued edit landed first");
        std::fs::write(&path, "refresh_secs = \"five\"\n").unwrap();
        let result = writer.with_file(|path| save_settings_at(path, &SettingsUpdate::default()));
        assert!(result.is_err());
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failing_edit_or_an_unreadable_file_writes_nothing_wrong() {
        let (dir, path) = writer_dir("failing");
        std::fs::write(&path, "window = 900\n").unwrap();
        let writer = ConfigWriter::new(path.clone());
        let file = writer.shared.file.lock().unwrap();
        writer.submit("window", |doc| set_window(doc, 900.0, 600.0));
        writer.submit("a", set_x(3));
        drop(file);
        assert!(writer.flush());
        // The window edit is refused and logged; the other one lands.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("window = 900"));
        assert_eq!(read_x(&path), Some(3));

        std::fs::write(&path, "not [toml\n").unwrap();
        writer.submit("a", set_x(4));
        assert!(writer.flush());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [toml\n");
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
