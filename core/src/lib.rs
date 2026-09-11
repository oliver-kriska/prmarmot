//! Framework-independent core of PR Marmot.
//!
//! Everything here is UI-agnostic and unit-testable without GPUI: the GitHub
//! transport (behind a trait), the GraphQL query + raw response model, and the
//! board derivation (categorization + Note) ported from the shell prototype
//! (`pr-board.sh`), which is the behavioral spec.

pub mod attention;
pub mod board;
pub mod github;
