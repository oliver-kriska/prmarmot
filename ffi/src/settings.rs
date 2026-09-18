//! The desktop's `config.toml`, readable and writable from the iPad.
//!
//! The point is a round trip: a file exported here opens on the desktop, and a
//! desktop file imported here means the same thing, because both go through
//! `prmarmot_local::config::FileConfig` — the type the desktop parses. Keys
//! this front end has no use for (the window size) are carried through rather
//! than dropped, so importing and re-exporting does not quietly delete them.

use std::collections::{BTreeMap, HashMap};

use prmarmot_local::config::{AuthSection, FileConfig, IssueLinkSection, WindowSection};

use crate::error::FfiError;
use crate::types::IssueLink;

/// `[auth]`: which GitHub host to talk to and how to sign in.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AuthConfig {
    pub host: Option<String>,
    /// Public by design — the device flow has no client secret.
    pub client_id: Option<String>,
    /// `auto` | `gh` | `device` | `token`.
    pub mode: Option<String>,
    /// `auto` | `keychain` | `file`.
    pub store: Option<String>,
}

/// `[window]`. The iPad has no window to remember; it carries the desktop's
/// through so a round trip does not lose it.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WindowConfig {
    pub width: f32,
    pub height: f32,
}

/// Every key of `config.toml`, in the order the file writes them.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct AppConfig {
    pub repo: Option<String>,
    /// `all` or `repo`.
    pub scope: Option<String>,
    pub repos: Vec<String>,
    pub pinned_repos: Vec<String>,
    pub refresh_secs: Option<u64>,
    /// `system` | `light` | `dark`.
    pub theme: Option<String>,
    /// `authored` | `review`.
    pub view: Option<String>,
    pub default_reviewers: Vec<String>,
    /// Suggestions per owner (`"acme"`) or repository (`"acme/api"`).
    pub repo_reviewers: HashMap<String, Vec<String>>,
    pub issue_link: Option<IssueLink>,
    pub notifications: bool,
    pub notification_sound: bool,
    pub notify_all_needs_action: bool,
    pub dock_badge: bool,
    pub automatic_update_checks: bool,
    pub stale_after_days: Option<u64>,
    pub auth: Option<AuthConfig>,
    pub window: Option<WindowConfig>,
}

/// An empty config, with the desktop's defaults for the keys that have one.
#[uniffi::export]
pub fn default_app_config() -> AppConfig {
    AppConfig::from(FileConfig::default())
}

/// Write a config file. The text opens on the desktop.
#[uniffi::export]
pub fn config_to_toml(config: AppConfig) -> Result<String, FfiError> {
    FileConfig::from(config)
        .to_toml_string()
        .map_err(FfiError::invalid)
}

/// Read a config file, e.g. one exported from the desktop.
#[uniffi::export]
pub fn config_from_toml(text: String) -> Result<AppConfig, FfiError> {
    FileConfig::from_toml_str(&text)
        .map(AppConfig::from)
        .map_err(FfiError::invalid)
}

impl From<FileConfig> for AppConfig {
    fn from(file: FileConfig) -> Self {
        Self {
            repo: file.repo,
            scope: file.scope,
            repos: file.repos,
            pinned_repos: file.pinned_repos,
            refresh_secs: file.refresh_secs,
            theme: file.theme,
            view: file.view,
            default_reviewers: file.default_reviewers,
            repo_reviewers: file.repo_reviewers.into_iter().collect(),
            issue_link: file.issue_link.map(|link| IssueLink {
                pattern: link.pattern,
                url_template: link.url_template,
            }),
            notifications: file.notifications,
            notification_sound: file.notification_sound,
            notify_all_needs_action: file.notify_all_needs_action,
            dock_badge: file.dock_badge,
            automatic_update_checks: file.automatic_update_checks,
            stale_after_days: file.stale_after_days,
            auth: file.auth.map(|auth| AuthConfig {
                host: auth.host,
                client_id: auth.client_id,
                mode: auth.mode,
                store: auth.store,
            }),
            window: file.window.map(|window| WindowConfig {
                width: window.width,
                height: window.height,
            }),
        }
    }
}

impl From<AppConfig> for FileConfig {
    fn from(config: AppConfig) -> Self {
        let mut repo_reviewers = BTreeMap::new();
        for (key, value) in config.repo_reviewers {
            repo_reviewers.insert(key, value);
        }
        Self {
            repo: config.repo,
            scope: config.scope,
            repos: config.repos,
            pinned_repos: config.pinned_repos,
            refresh_secs: config.refresh_secs,
            theme: config.theme,
            view: config.view,
            default_reviewers: config.default_reviewers,
            repo_reviewers,
            issue_link: config.issue_link.map(|link| IssueLinkSection {
                pattern: link.pattern,
                url_template: link.url_template,
            }),
            notifications: config.notifications,
            notification_sound: config.notification_sound,
            notify_all_needs_action: config.notify_all_needs_action,
            dock_badge: config.dock_badge,
            automatic_update_checks: config.automatic_update_checks,
            stale_after_days: config.stale_after_days,
            auth: config.auth.map(|auth| AuthSection {
                host: auth.host,
                client_id: auth.client_id,
                mode: auth.mode,
                store: auth.store,
            }),
            window: config.window.map(|window| window.to_section()),
        }
    }
}

impl WindowConfig {
    fn to_section(&self) -> WindowSection {
        WindowSection {
            width: self.width,
            height: self.height,
        }
    }
}
