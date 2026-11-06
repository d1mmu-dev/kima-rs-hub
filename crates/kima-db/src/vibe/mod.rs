//! The vibe engine's storage (M5): matching state, per-track feel, the map, learned moods.
//!
//! Every query that reads vectors goes through [`ELIGIBLE`]; a source-grep test in `tests/vibe_schema.rs` fails on a
//! `track_embeddings` mention anywhere else in this module.

mod eligible;
pub mod map;
pub mod moods;
pub mod path;
pub mod rank;
pub mod state;

pub use eligible::{ELIGIBLE, MATCHING_SOUND, SoundIdentity};
