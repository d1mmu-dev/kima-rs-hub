//! Shared helpers for the integration tests (included with `mod common;`).
#![allow(dead_code)]

use kima_vibe::formula::*;
use std::path::PathBuf;

pub fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A golden f64 stored as 16 hex digits of its IEEE-754 bit pattern.
pub fn from_bits_hex(h: &str) -> f64 {
    f64::from_bits(
        u64::from_str_radix(h, 16).unwrap_or_else(|e| panic!("bad bit pattern {h:?}: {e}")),
    )
}

/// Bit equality, except that any NaN equals any NaN (engines canonicalise NaN payloads differently).
pub fn same_bits(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

/// Distance in units in the last place between two finite, same-signed-or-zero doubles.
pub fn ulps(a: f64, b: f64) -> u64 {
    fn key(x: f64) -> i64 {
        let b = x.to_bits() as i64;
        if b < 0 { i64::MIN - b } else { b }
    }
    key(a).abs_diff(key(b))
}

/// A tiny xorshift generator, so the tests need no dev-dependency and are reproducible.
pub struct Xorshift(pub u64);
impl Xorshift {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub struct Lib {
    pub tracks: Vec<MatchTrack>,
    pub model: GenreModel,
    pub gw: f64,
    pub gw_sq: f64,
    /// n x 2 standardised (valence, arousal), as `buildLibrary` makes them.
    pub feel_z: Vec<f64>,
}

/// A golden library as `buildLibrary` in `tools/vibe-goldens/run.mjs` makes it (rows already in id order).
pub fn load(name: &str) -> Lib {
    let json = fixture("ts-golden-library.json");
    let lib = &json[name];
    let bits = |v: &serde_json::Value| from_bits_hex(v.as_str().unwrap());
    let stats = FeelStats {
        mean: [bits(&lib["feelMean"][0]), bits(&lib["feelMean"][1])],
        sd: [bits(&lib["feelSd"][0]), bits(&lib["feelSd"][1])],
    };
    let f32_of = |v: &serde_json::Value| f64::from(v.as_f64().unwrap() as f32);
    let tracks: Vec<MatchTrack> = lib["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| MatchTrack {
            track_id: t["id"].as_str().unwrap().into(),
            album_id: t["albumId"].as_str().unwrap().into(),
            artist_id: t["artistId"].as_str().unwrap().into(),
            sound: unit(
                &t["sound"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_f64().unwrap())
                    .collect::<Vec<_>>(),
            ),
            feel: feel_vector([f32_of(&t["valence"]), f32_of(&t["arousal"])], &stats),
        })
        .collect();
    let artists: Vec<LibraryArtist> = lib["artistGenres"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, genres)| LibraryArtist {
            artist_id: id.clone(),
            tracks: tracks.iter().filter(|t| &t.artist_id == id).count() as u64,
            genres: genres
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g.as_str().unwrap().to_owned())
                .collect(),
        })
        .collect();
    let (model, spread) = build_genre_model(&artists);
    assert!(same_bits(spread, bits(&lib["genreSpread"])), "genre spread");
    let wg = bits(&lib["genreWeight"]);
    let mut feel_z = Vec::with_capacity(tracks.len() * 2);
    for t in lib["tracks"].as_array().unwrap() {
        for (key, c) in [("valence", 0), ("arousal", 1)] {
            let raw = f32_of(&t[key]);
            feel_z.push(if stats.sd[c] > 0.0 {
                (raw - stats.mean[c]) / stats.sd[c]
            } else {
                0.0
            });
        }
    }
    Lib {
        tracks,
        model,
        gw: wg,
        gw_sq: wg * wg,
        feel_z,
    }
}
