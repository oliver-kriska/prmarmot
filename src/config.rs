//! The config file at `~/.config/prmarmot/config.toml` (or `$XDG_CONFIG_HOME`).
//!
//! This is what makes Spotlight/Finder launches work: those carry no shell
//! environment. Precedence is CLI > environment > config; when no scope is
//! configured, the app opens all repositories involving the signed-in user.
//! The read model is `prmarmot_local::config`; the app's writes, the legacy
//! migration and the update paths are `prmarmot_local::app_files`, where their
//! tests run without GPUI.

pub use prmarmot_local::app_files::*;
pub use prmarmot_local::config::{
    config_path, load, normalized_pins, resolve_scope, FileConfig, MAX_PINNED_REPOS,
};
