//! Machine-local PR Marmot data shared by the desktop app and `prmarmot-cli`:
//! the config file's read model and the persisted attention state (watches,
//! snoozes, change snapshots), plus the desktop app's file writes and its
//! Homebrew/installer update check, kept here so their tests need no GPUI.
//! `prmarmot-core` stays free of filesystem code.

pub mod app_files;
pub mod attention_state;
pub mod auth;
pub mod config;
pub mod panic_log;
#[cfg(feature = "http")]
pub mod session;
pub mod small_pages;
#[cfg(feature = "http")]
pub mod updates;
