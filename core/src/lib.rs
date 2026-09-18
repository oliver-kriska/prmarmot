//! Framework-independent core of PR Marmot.
//!
//! Everything here is UI-agnostic and unit-testable without GPUI: the GitHub
//! transport (behind a trait), the GraphQL query + raw response model, and the
//! board derivation (categorization + Note) ported from the shell prototype
//! (`pr-board.sh`), which is the behavioral spec.

#[cfg(all(not(feature = "regex"), not(feature = "small-regex")))]
compile_error!(
    "prmarmot-core needs a regex engine: enable the default `regex` feature, \
     or `small-regex` for the iOS build"
);

pub mod attention;
pub mod board;
pub mod cells;
pub mod detail;
pub mod github;
pub mod layout;
pub mod pickup;
pub mod search;
pub mod share;
pub mod size;
pub mod status;
