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
pub mod settings;
pub mod signin;
pub mod transport;
pub mod types;

pub use attention::*;
pub use client::*;
pub use error::FfiError;
pub use pure::*;
pub use settings::*;
pub use signin::*;
pub use transport::*;
pub use types::*;

/// The version of `prmarmot-ffi` a build embeds. It moves rarely, so it cannot
/// tell two XCFrameworks apart: `core_build()` can.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Which build of the core this is: what an About screen or a bug report
/// shows so two XCFrameworks can be told apart.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CoreBuild {
    /// The desktop release this core ships in, from the nearest `v*` tag:
    /// "v0.12.1" when built on the tag, "v0.12.1-3-gabc1234" three commits
    /// past it, with "-dirty" when `dirty`. "unknown" outside a git checkout.
    /// This is the version to show; it matches the desktop app's.
    pub release: String,
    /// `core_version()`, the ffi crate's own version, which rarely moves.
    pub version: String,
    /// The short (12-character) commit it was built from, or "unknown" when
    /// it was not built from a git checkout.
    pub commit: String,
    /// `core/`, `local/`, `ffi/` or `Cargo.lock` had uncommitted changes, so
    /// the commit alone does not describe it. `None` when unknown.
    pub dirty: Option<bool>,
    /// Cargo's profile, "debug" or "release".
    pub profile: String,
    /// All of it in one line, e.g. "v0.12.1 (1a2b3c4d5e6f, release)" or
    /// "v0.12.1-3-gabc1234-dirty (abc1234abcde-dirty, debug)".
    pub description: String,
}

/// The build this library is: its version, commit, dirty flag and profile,
/// written into it when it was compiled (`ffi/build.rs`).
#[uniffi::export]
pub fn core_build() -> CoreBuild {
    let release = env!("PRMARMOT_FFI_RELEASE").to_owned();
    let version = core_version();
    let commit = env!("PRMARMOT_FFI_COMMIT").to_owned();
    let dirty = match env!("PRMARMOT_FFI_DIRTY") {
        "dirty" => Some(true),
        "clean" => Some(false),
        _ => None,
    };
    let profile = env!("PRMARMOT_FFI_PROFILE").to_owned();
    let marked = match dirty {
        Some(true) => format!("{commit}-dirty"),
        _ => commit.clone(),
    };
    let description = format!("{release} ({marked}, {profile})");
    CoreBuild {
        release,
        version,
        commit,
        dirty,
        profile,
        description,
    }
}
