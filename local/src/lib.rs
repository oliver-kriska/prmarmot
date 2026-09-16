//! Machine-local PR Marmot data shared by the desktop app and `prmarmot-cli`:
//! the config file's read model and the persisted attention state (watches,
//! snoozes, change snapshots). `prmarmot-core` stays free of filesystem code.

pub mod attention_state;
pub mod config;
