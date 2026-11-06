//! Pure arithmetic of the vibe engine (M5). No sqlx, no tokio, no ort.
#![forbid(unsafe_code)]

pub mod asset;
pub mod exact_math;
pub mod feel_probe;
pub mod formula;
pub mod mood_heads;
pub mod neighbours;
pub mod prng;
pub mod song_path;

/// The CLAP vector width every probe, head and stored vector has.
pub const CLAP_DIMS: usize = 512;
