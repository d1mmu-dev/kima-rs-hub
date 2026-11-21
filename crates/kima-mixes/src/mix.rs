//! A generated mix as the API sends it (upstream `services/mixes/helpers.ts` `ProgrammaticMix`, `getMixColor`,
//! `albumCoverUrls`, `mixOf`) and the pure arrangements of the analysis mixes (`moodMixes.ts` Key Journey, Tempo
//! Flow).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::seeded::{seeded_order, seeded_random};

/// Upstream `TRACK_LIMIT` / `WEEKLY_TRACK_LIMIT`: tracks in a finished generated mix.
pub const MIX_TRACKS: usize = 20;

/// Upstream `ProgrammaticMix`, in its key order. `extra` carries the keys a saved mood mix adds (`mood`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammaticMix {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub description: String,
    pub track_ids: Vec<Uuid>,
    /// Album cover tokens (the album id when it has a stored image, D-265).
    pub cover_urls: Vec<String>,
    pub track_count: usize,
    pub color: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// What a mix is called and coloured, around the songs drawn for it (upstream `MixSpec` without the seed).
#[derive(Debug, Clone, Copy)]
pub struct MixSpec<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    /// Key into the colour table.
    pub color: &'a str,
}

/// A song as a finished mix reads it: its id and its album's cover token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixSong {
    pub id: Uuid,
    pub cover: Option<String>,
}

/// Upstream `mixOf`: a mix of `songs`, in that order.
pub fn mix_of(id: String, spec: MixSpec<'_>, songs: &[MixSong]) -> ProgrammaticMix {
    ProgrammaticMix {
        id,
        kind: spec.kind.to_string(),
        name: spec.name.to_string(),
        description: spec.description.to_string(),
        track_ids: songs.iter().map(|s| s.id).collect(),
        cover_urls: album_cover_urls(songs),
        track_count: songs.len(),
        color: mix_color(spec.color).to_string(),
        extra: serde_json::Map::new(),
    }
}

/// Upstream `albumCoverUrls`: up to four different album covers, in track order, coverless albums skipped.
pub fn album_cover_urls(songs: &[MixSong]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cover in songs.iter().filter_map(|s| s.cover.as_ref()) {
        if !out.contains(cover) {
            out.push(cover.clone());
        }
    }
    out.truncate(4);
    out
}

/// Upstream `MIX_COLORS`, byte for byte.
const MIX_COLORS: &[(&str, &str)] = &[
    (
        "late-night",
        "linear-gradient(to bottom, rgba(30, 27, 75, 0.7), rgba(30, 58, 138, 0.5), rgba(15, 23, 42, 0.4))",
    ),
    (
        "3am-thoughts",
        "linear-gradient(to bottom, rgba(46, 16, 101, 0.7), rgba(88, 28, 135, 0.5), rgba(15, 23, 42, 0.4))",
    ),
    (
        "night-drive",
        "linear-gradient(to bottom, rgba(15, 23, 42, 0.7), rgba(49, 46, 129, 0.5), rgba(88, 28, 135, 0.4))",
    ),
    (
        "chill",
        "linear-gradient(to bottom, rgba(17, 94, 89, 0.6), rgba(22, 78, 99, 0.5), rgba(15, 23, 42, 0.4))",
    ),
    (
        "coffee-shop",
        "linear-gradient(to bottom, rgba(120, 53, 15, 0.6), rgba(68, 64, 60, 0.5), rgba(38, 38, 38, 0.4))",
    ),
    (
        "sunday-morning",
        "linear-gradient(to bottom, rgba(253, 186, 116, 0.4), rgba(252, 211, 77, 0.3), rgba(68, 64, 60, 0.4))",
    ),
    (
        "workout",
        "linear-gradient(to bottom, rgba(153, 27, 27, 0.6), rgba(124, 45, 18, 0.5), rgba(68, 64, 60, 0.4))",
    ),
    (
        "confidence-boost",
        "linear-gradient(to bottom, rgba(194, 65, 12, 0.6), rgba(146, 64, 14, 0.5), rgba(68, 64, 60, 0.4))",
    ),
    (
        "happy",
        "linear-gradient(to bottom, rgba(217, 119, 6, 0.5), rgba(161, 98, 7, 0.4), rgba(68, 64, 60, 0.4))",
    ),
    (
        "summer-vibes",
        "linear-gradient(to bottom, rgba(8, 145, 178, 0.5), rgba(15, 118, 110, 0.4), rgba(30, 58, 138, 0.4))",
    ),
    (
        "golden-hour",
        "linear-gradient(to bottom, rgba(245, 158, 11, 0.5), rgba(234, 88, 12, 0.4), rgba(136, 19, 55, 0.4))",
    ),
    (
        "melancholy",
        "linear-gradient(to bottom, rgba(51, 65, 85, 0.6), rgba(30, 58, 138, 0.5), rgba(17, 24, 39, 0.4))",
    ),
    (
        "sad-girl-sundays",
        "linear-gradient(to bottom, rgba(136, 19, 55, 0.5), rgba(30, 41, 59, 0.5), rgba(59, 7, 100, 0.4))",
    ),
    (
        "heartbreak-hotel",
        "linear-gradient(to bottom, rgba(30, 58, 138, 0.6), rgba(88, 28, 135, 0.5), rgba(15, 23, 42, 0.4))",
    ),
    (
        "dance-floor",
        "linear-gradient(to bottom, rgba(162, 28, 175, 0.6), rgba(131, 24, 67, 0.5), rgba(59, 7, 100, 0.4))",
    ),
    (
        "acoustic",
        "linear-gradient(to bottom, rgba(146, 64, 14, 0.6), rgba(124, 45, 18, 0.5), rgba(68, 64, 60, 0.4))",
    ),
    (
        "instrumental",
        "linear-gradient(to bottom, rgba(91, 33, 182, 0.6), rgba(88, 28, 135, 0.5), rgba(15, 23, 42, 0.4))",
    ),
    (
        "focus-flow",
        "linear-gradient(to bottom, rgba(30, 58, 138, 0.6), rgba(30, 41, 59, 0.5), rgba(17, 24, 39, 0.4))",
    ),
    (
        "road-trip",
        "linear-gradient(to bottom, rgba(194, 65, 12, 0.6), rgba(146, 64, 14, 0.5), rgba(14, 165, 233, 0.4))",
    ),
    (
        "main-character",
        "linear-gradient(to bottom, rgba(245, 158, 11, 0.5), rgba(202, 138, 4, 0.4), rgba(124, 45, 18, 0.4))",
    ),
    (
        "villain-era",
        "linear-gradient(to bottom, rgba(69, 10, 10, 0.7), rgba(17, 24, 39, 0.6), rgba(0, 0, 0, 0.5))",
    ),
    (
        "throwback",
        "linear-gradient(to bottom, rgba(146, 64, 14, 0.5), rgba(124, 45, 18, 0.4), rgba(68, 64, 60, 0.4))",
    ),
    (
        "era",
        "linear-gradient(to bottom, rgba(68, 64, 60, 0.5), rgba(38, 38, 38, 0.4), rgba(39, 39, 42, 0.4))",
    ),
    (
        "genre",
        "linear-gradient(to bottom, rgba(63, 63, 70, 0.5), rgba(30, 41, 59, 0.4), rgba(17, 24, 39, 0.4))",
    ),
    (
        "top-tracks",
        "linear-gradient(to bottom, rgba(6, 95, 70, 0.5), rgba(17, 94, 89, 0.4), rgba(15, 23, 42, 0.4))",
    ),
    (
        "rediscover",
        "linear-gradient(to bottom, rgba(55, 48, 163, 0.5), rgba(76, 29, 149, 0.4), rgba(15, 23, 42, 0.4))",
    ),
    (
        "artist-similar",
        "linear-gradient(to bottom, rgba(107, 33, 168, 0.5), rgba(112, 26, 117, 0.4), rgba(15, 23, 42, 0.4))",
    ),
    (
        "discovery",
        "linear-gradient(to bottom, rgba(2, 132, 199, 0.5), rgba(30, 58, 138, 0.4), rgba(15, 23, 42, 0.4))",
    ),
    (
        "sad",
        "linear-gradient(to bottom, rgba(30, 58, 138, 0.6), rgba(88, 28, 135, 0.5), rgba(15, 23, 42, 0.4))",
    ),
    (
        "party",
        "linear-gradient(to bottom, rgba(162, 28, 175, 0.6), rgba(131, 24, 67, 0.5), rgba(59, 7, 100, 0.4))",
    ),
    (
        "aggressive",
        "linear-gradient(to bottom, rgba(69, 10, 10, 0.7), rgba(17, 24, 39, 0.6), rgba(0, 0, 0, 0.5))",
    ),
];

/// Upstream `MIX_COLORS.default`.
const DEFAULT_COLOR: &str =
    "linear-gradient(to bottom, rgba(88, 28, 135, 0.4), rgba(26, 26, 26, 1), transparent)";

/// Upstream `getMixColor`: the type's background, else the default.
pub fn mix_color(kind: &str) -> &'static str {
    MIX_COLORS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(DEFAULT_COLOR, |(_, c)| c)
}

/// The key names Essentia writes (`key` on upstream's `Track`), by pitch class 0 = C .. 11 = B; kima stores the pitch
/// class (`track_analysis.key_tonic`).
pub const ESSENTIA_KEYS: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

/// Upstream Key Journey's `keyOrder`: the circle of fifths, in the spellings it groups by (`spellingsOf` maps
/// Essentia's C# to Db).
pub const KEY_ORDER: [&str; 12] = [
    "C", "G", "D", "A", "E", "B", "F#", "Db", "Ab", "Eb", "Bb", "F",
];

/// The [`KEY_ORDER`] spelling of a pitch class.
pub fn key_order_name(pitch_class: u8) -> Option<&'static str> {
    const BY_PC: [&str; 12] = [
        "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
    ];
    BY_PC.get(usize::from(pitch_class)).copied()
}

/// Upstream `generateKeyJourney`'s walk over the day's draw (`tracks`, in the drawn order, with their key's
/// [`KEY_ORDER`] name): around the circle of fifths, at most two songs per key in the key's own seeded order
/// (`key-journey-<today>-<key>`), up to [`MIX_TRACKS`]. `None` under 15 drawn or 15 walked.
pub fn key_journey<T: Clone>(
    tracks: &[(T, &'static str)],
    id_of: impl Fn(&T) -> String,
    today: &str,
) -> Option<Vec<T>> {
    if tracks.len() < 15 {
        return None;
    }
    let mut journey: Vec<T> = Vec::new();
    for key in KEY_ORDER {
        let in_key: Vec<(String, T)> = tracks
            .iter()
            .filter(|(_, k)| *k == key)
            .map(|(t, _)| (id_of(t), t.clone()))
            .collect();
        if in_key.is_empty() || journey.len() >= MIX_TRACKS {
            continue;
        }
        let count = 2.min(in_key.len()).min(MIX_TRACKS - journey.len());
        let seed = seeded_random(&format!("key-journey-{today}-{key}"));
        let ordered = seeded_order(in_key, seed, |(id, _)| id.as_str());
        journey.extend(ordered.into_iter().take(count).map(|(_, t)| t));
    }
    (journey.len() >= 15).then_some(journey)
}

/// Upstream `generateTempoFlow`'s flow over the day's draw: each band (slow < 100, medium 100-130, fast >= 130 BPM) in
/// the day's seeded order, then 4 slow, 5 medium, 6 fast, 3 more medium and 2 more slow. `None` under 15 drawn or 15
/// in the flow.
pub fn tempo_flow<T: Clone>(
    tracks: &[(T, f64)],
    id_of: impl Fn(&T) -> String,
    seed: u32,
) -> Option<Vec<T>> {
    if tracks.len() < 15 {
        return None;
    }
    let band = |keep: &dyn Fn(f64) -> bool| -> Vec<T> {
        let items: Vec<(String, T)> = tracks
            .iter()
            .filter(|(_, bpm)| keep(*bpm))
            .map(|(t, _)| (id_of(t), t.clone()))
            .collect();
        seeded_order(items, seed, |(id, _)| id.as_str())
            .into_iter()
            .map(|(_, t)| t)
            .collect()
    };
    let slow = band(&|bpm| bpm < 100.0);
    let medium = band(&|bpm| (100.0..130.0).contains(&bpm));
    let fast = band(&|bpm| bpm >= 130.0);
    let part = |v: &[T], from: usize, to: usize| -> Vec<T> {
        v.iter()
            .skip(from)
            .take(to.saturating_sub(from))
            .cloned()
            .collect()
    };
    let mut flow = part(&slow, 0, 4);
    flow.extend(part(&medium, 0, 5));
    flow.extend(part(&fast, 0, 6));
    flow.extend(part(&medium, 5, 8));
    flow.extend(part(&slow, 4, 6));
    (flow.len() >= 15).then_some(flow)
}
