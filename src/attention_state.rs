//! Attention state lives in `prmarmot-local` so the CLI can read the same
//! file; the app remains its only writer.

pub use prmarmot_local::attention_state::*;
