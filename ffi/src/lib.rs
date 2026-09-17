//! `prmarmot-ffi` — PR Marmot's core, as a Swift library.
//!
//! The iPad app is a front end, not a second implementation. Categorization,
//! Note text, section order, pickup age, the size band, the search grammar and
//! the "changed since you looked" rule all stay in `prmarmot-core`, where they
//! are pinned by golden tests against the shell prototype. This crate is the
//! adapter: UniFFI records that mirror `cli/schema/board-v1.schema.json`, two
//! traits Swift implements, and one object that owns a connection.
//!
//! What is deliberately **not** here:
//!
//! - **HTTP.** `prmarmot-core`'s `http` feature is off, so nothing links a
//!   second TLS stack into an iOS binary. Swift's `URLSession` does the I/O
//!   and this crate hands it a finished request.
//! - **Files.** The attention store serializes to bytes; where they live is
//!   Swift's business.
//! - **A clock.** Every entry point that needs "now" takes it as Unix seconds,
//!   which is what `core/clippy.toml` enforces on the other side.
//! - **A runtime.** The bridge between UniFFI's async foreign traits and
//!   core's blocking transport is a worker thread and two channels. See
//!   `transport.rs`.
//!
//! The cycle rule — a Swift transport must not hold its `BoardClient`
//! strongly — is in `ffi/README.md` and repeated on the traits themselves.

uniffi::setup_scaffolding!();

pub mod attention;
pub mod client;
pub mod error;
pub mod pure;
pub mod transport;
pub mod types;

pub use attention::*;
pub use client::*;
pub use error::FfiError;
pub use pure::*;
pub use transport::*;
pub use types::*;

/// The version of `prmarmot-ffi` a build embeds, for a Settings screen and for
/// telling two XCFrameworks apart.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
