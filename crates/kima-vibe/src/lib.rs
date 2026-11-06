//! Pure arithmetic of the vibe engine (M5). No sqlx, no tokio, no ort.
#![forbid(unsafe_code)]

pub mod asset;
pub mod colour;
pub mod exact_math;
pub mod feel_probe;
pub mod formula;
pub mod map_compute;
pub mod mood_heads;
pub mod neighbours;
pub mod pca;
pub mod prng;
pub mod song_path;
pub mod umap;

/// The CLAP vector width every probe, head and stored vector has.
pub const CLAP_DIMS: usize = 512;
