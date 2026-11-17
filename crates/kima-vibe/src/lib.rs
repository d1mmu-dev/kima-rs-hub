//! Pure arithmetic of the vibe engine (M5). No sqlx, no tokio, no ort.
#![forbid(unsafe_code)]

pub mod asset;
pub mod cancel;
pub mod colour;
pub mod exact_math;
pub mod feel_probe;
pub mod formula;
pub mod map_compute;
pub mod mood_heads;
pub mod moods;
pub mod neighbours;
pub mod pca;
pub mod prng;
pub mod song_path;
pub mod umap;
pub mod zero_shot;

/// The CLAP vector width every probe, head and stored vector has.
pub const CLAP_DIMS: usize = 512;

/// ECMAScript WhiteSpace + LineTerminator (includes BOM, excludes NEL).
/// Shared by HTTP query validation and the real-recall encoder input gate.
pub fn ecmascript_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
