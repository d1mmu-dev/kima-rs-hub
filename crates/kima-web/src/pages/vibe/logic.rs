//! Pure helpers of the vibe page, ported from upstream `features/vibe/{mapUtils,librarySearch,
//! useTrackFinder,MoodLegend,TrackPanel}.ts(x)`: mood names and colours, the legend's counts, the
//! title/artist search over the loaded map, the merged name/sound results, the 2D projection of the
//! map and the selected track's mood bars.

use std::collections::{BTreeMap, HashSet};

use kima_api_types::vibe::{MapTrack, SearchTrack};
use unicode_normalization::UnicodeNormalization;

/// The five moods, in upstream's fixed order (`MOOD_KEYS`).
pub const MOOD_KEYS: [&str; 5] = [
    "moodHappy",
    "moodSad",
    "moodRelaxed",
    "moodAggressive",
    "moodParty",
];

/// Human name for a backend mood key (`moodLabel`); anything unknown reads as "No single mood".
pub fn mood_label(key: &str) -> &'static str {
    match key {
        "moodHappy" => "Upbeat",
        "moodSad" => "Melancholic",
        "moodRelaxed" => "Calm",
        "moodAggressive" => "Intense",
        "moodParty" => "Energetic",
        "unanalyzed" => "Not analysed yet",
        _ => "No single mood",
    }
}

/// The mood's colour (`MOOD_COLORS`), neutral grey for anything unknown.
pub fn mood_rgb(key: &str) -> (u8, u8, u8) {
    match key {
        "moodHappy" => (240, 178, 90),
        "moodSad" => (185, 143, 224),
        "moodRelaxed" => (91, 194, 180),
        "moodAggressive" => (224, 104, 94),
        "moodParty" => (224, 106, 176),
        "unanalyzed" => (96, 96, 104),
        _ => (163, 163, 163),
    }
}

/// True for one of the five moods; false for "no single mood" / "not analysed yet".
pub fn is_mood(key: &str) -> bool {
    MOOD_KEYS.contains(&key)
}

/// The legend: each dominant mood with its track count, the five moods first, then by count
/// (largest first); ties keep first appearance (JavaScript's stable sort).
pub fn mood_counts(tracks: &[MapTrack]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for t in tracks {
        match counts.iter_mut().find(|(k, _)| *k == t.dominant_mood) {
            Some((_, n)) => *n += 1,
            None => counts.push((t.dominant_mood.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| is_mood(&b.0).cmp(&is_mood(&a.0)).then(b.1.cmp(&a.1)));
    counts
}

/// Lower-cased, accents removed (`fold`: NFD, combining marks U+0300–U+036F dropped).
pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect::<String>()
        .to_lowercase()
}

/// `fold("<title> <artist>")` per track, built once per map.
pub fn haystacks(tracks: &[MapTrack]) -> Vec<String> {
    tracks
        .iter()
        .map(|t| fold(&format!("{} {}", t.title, t.artist)))
        .collect()
}

/// Every matching track id (the map highlight), how many there are, and the first `limit`
/// indexes into the track list, title/artist-start matches first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Matches {
    pub ids: HashSet<String>,
    pub count: usize,
    pub top: Vec<usize>,
}

/// `searchLibrary`: every word of the query must appear; fewer than two letters match nothing.
pub fn search_library(
    tracks: &[MapTrack],
    haystacks: &[String],
    query: &str,
    limit: usize,
) -> Matches {
    let query = fold(query);
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.iter().map(|w| w.chars().count()).sum::<usize>() < 2 {
        return Matches::default();
    }
    let first = words[0];
    let spaced = format!(" {first}");
    let (mut starts, mut contains) = (Vec::new(), Vec::new());
    let mut ids = HashSet::new();
    for (i, hay) in haystacks.iter().enumerate() {
        if !words.iter().all(|w| hay.contains(w)) {
            continue;
        }
        let Some(track) = tracks.get(i) else { continue };
        ids.insert(track.id.clone());
        if hay.starts_with(first) || hay.contains(&spaced) {
            if starts.len() < limit {
                starts.push(i);
            }
        } else if contains.len() < limit {
            contains.push(i);
        }
    }
    starts.extend(contains);
    starts.truncate(limit);
    Matches {
        count: ids.len(),
        ids,
        top: starts,
    }
}

/// How a result was found: by its name on the map, or by how it sounds (`/vibe/search`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Name,
    Sound,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub via: Via,
}

/// `useTrackFinder`: name matches first, then sound matches not already listed, at most `limit`.
pub fn found_tracks(by_name: Vec<Found>, by_sound: &[SearchTrack], limit: usize) -> Vec<Found> {
    let seen: HashSet<String> = by_name.iter().map(|f| f.id.clone()).collect();
    let mut found = by_name;
    found.extend(
        by_sound
            .iter()
            .filter(|t| !seen.contains(&t.id))
            .map(|t| Found {
                id: t.id.clone(),
                title: t.title.clone(),
                artist: t.artist.name.clone(),
                via: Via::Sound,
            }),
    );
    found.truncate(limit);
    found
}

/// Side of the square map drawing (SVG user units) and the margin kept free around the points.
pub const MAP_SIZE: f64 = 1000.0;
const MAP_MARGIN: f64 = 30.0;

/// A map position (`x`, `y` in 0..1 around 0.5) in the drawing: seen from above, `y` up.
pub fn project(x: f64, y: f64) -> (f64, f64) {
    let span = MAP_SIZE - 2.0 * MAP_MARGIN;
    let at = |v: f64| MAP_MARGIN + v.clamp(0.0, 1.0) * span;
    (at(x), MAP_SIZE - at(y))
}

/// The selected track's moods above zero, strongest first, each with its bar width in percent of
/// the strongest (`TrackPanel`).
pub fn mood_bars(moods: &BTreeMap<String, f64>) -> Vec<(String, f64)> {
    let mut bars: Vec<(String, f64)> = moods
        .iter()
        .filter(|(_, v)| **v > 0.0)
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    bars.sort_by(|a, b| b.1.total_cmp(&a.1));
    let strongest = bars.first().map_or(1.0, |b| b.1);
    for bar in &mut bars {
        bar.1 = bar.1 / strongest * 100.0;
    }
    bars
}

/// `Math.round(v * 100)%`.
pub fn percent(v: f64) -> String {
    // Math.round rounds halves up; f64::round rounds them away from zero (same for v >= 0).
    format!("{}%", (v * 100.0 + 0.5).floor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn track(id: &str, title: &str, artist: &str, mood: &str) -> MapTrack {
        serde_json::from_value(json!({
            "id": id, "x": 0.5, "y": 0.5, "z": 0.5, "colour": 0.5, "title": title,
            "artist": artist, "artistId": "ar", "albumId": "al", "coverUrl": null,
            "dominantMood": mood, "moodScore": 0.5, "moods": {}, "energy": null, "valence": null
        }))
        .unwrap()
    }

    fn sound(id: &str) -> SearchTrack {
        serde_json::from_value(json!({
            "id": id, "title": format!("S{id}"), "displayTitle": null, "duration": 1, "trackNo": 1,
            "distance": 0.1, "similarity": 0.9,
            "album": {"id": "al", "title": "A", "coverUrl": null}, "artist": {"id": "ar", "name": "Snd"}
        }))
        .unwrap()
    }

    #[test]
    fn moods_have_upstream_names_and_colours() {
        assert_eq!(mood_label("moodHappy"), "Upbeat");
        assert_eq!(mood_label("moodSad"), "Melancholic");
        assert_eq!(mood_label("moodRelaxed"), "Calm");
        assert_eq!(mood_label("moodAggressive"), "Intense");
        assert_eq!(mood_label("moodParty"), "Energetic");
        assert_eq!(mood_label("unanalyzed"), "Not analysed yet");
        assert_eq!(mood_label("neutral"), "No single mood");
        assert_eq!(mood_label("whatever"), "No single mood");
        assert_eq!(mood_rgb("moodHappy"), (240, 178, 90));
        assert_eq!(mood_rgb("unanalyzed"), (96, 96, 104));
        assert_eq!(mood_rgb("whatever"), (163, 163, 163));
        assert!(is_mood("moodParty"));
        assert!(!is_mood("neutral"));
    }

    #[test]
    fn legend_lists_moods_first_then_by_count() {
        let tracks = vec![
            track("1", "a", "x", "neutral"),
            track("2", "a", "x", "neutral"),
            track("3", "a", "x", "neutral"),
            track("4", "a", "x", "moodSad"),
            track("5", "a", "x", "moodHappy"),
            track("6", "a", "x", "moodHappy"),
            track("7", "a", "x", "unanalyzed"),
            track("8", "a", "x", "moodParty"),
        ];
        let got = mood_counts(&tracks);
        let keys: Vec<(&str, usize)> = got.iter().map(|(k, n)| (k.as_str(), *n)).collect();
        assert_eq!(
            keys,
            [
                ("moodHappy", 2),
                ("moodSad", 1),
                ("moodParty", 1),
                ("neutral", 3),
                ("unanalyzed", 1)
            ]
        );
    }

    #[test]
    fn fold_drops_case_and_accents() {
        assert_eq!(fold("Beyoncé ŻÓŁW"), "beyonce zołw");
        assert_eq!(fold("Ångström"), "angstrom");
    }

    #[test]
    fn library_search_needs_every_word_and_ranks_starts_first() {
        let tracks = vec![
            track("1", "Blue Monday", "New Order", "neutral"),
            track("2", "True Blue", "Madonna", "neutral"),
            track("3", "Something", "Bluebird", "neutral"),
            track("4", "Café blues", "Mário", "neutral"),
            track("5", "Red", "Nobody", "neutral"),
        ];
        let hay = haystacks(&tracks);
        assert_eq!(hay[3], "cafe blues mario");

        let m = search_library(&tracks, &hay, "blue", 8);
        assert_eq!(m.count, 4);
        assert!(m.ids.contains("2") && !m.ids.contains("5"));
        // "blue monday…" starts with it; "true blue", "bluebird" and "blues" contain " blue".
        assert_eq!(m.top, [0, 1, 2, 3]);

        let m = search_library(&tracks, &hay, "BLUE mario", 8);
        assert_eq!(m.top, [3]);

        let m = search_library(&tracks, &hay, "ue", 8);
        // Only containing matches: no word starts with "ue".
        assert_eq!(m.top, [0, 1, 2, 3]);
        assert_eq!(search_library(&tracks, &hay, "ue", 2).top, [0, 1]);

        assert_eq!(search_library(&tracks, &hay, "b", 8), Matches::default());

        // A word start ranks ahead of an earlier track that only contains the word.
        let ranked = vec![
            track("1", "Sunblue", "X", "neutral"),
            track("2", "Blue", "Y", "neutral"),
        ];
        let hay = haystacks(&ranked);
        assert_eq!(search_library(&ranked, &hay, "blue", 8).top, [1, 0]);
        let hay = haystacks(&tracks);
        assert_eq!(search_library(&tracks, &hay, "   ", 8), Matches::default());
    }

    #[test]
    fn sound_matches_follow_name_matches_without_duplicates() {
        let name = |id: &str| Found {
            id: id.into(),
            title: id.into(),
            artist: "n".into(),
            via: Via::Name,
        };
        let got = found_tracks(
            vec![name("a"), name("b")],
            &[sound("b"), sound("c"), sound("d")],
            3,
        );
        let ids: Vec<(&str, Via)> = got.iter().map(|f| (f.id.as_str(), f.via)).collect();
        assert_eq!(ids, [("a", Via::Name), ("b", Via::Name), ("c", Via::Sound)]);
        assert_eq!(got[2].title, "Sc");
        assert_eq!(got[2].artist, "Snd");
    }

    #[test]
    fn projection_keeps_a_margin_and_puts_y_up() {
        assert_eq!(project(0.0, 0.0), (30.0, 970.0));
        assert_eq!(project(1.0, 1.0), (970.0, 30.0));
        assert_eq!(project(0.5, 0.5), (500.0, 500.0));
        // Out-of-range values stay inside the drawing.
        assert_eq!(project(-1.0, 2.0), (30.0, 30.0));
    }

    #[test]
    fn mood_bars_are_relative_to_the_strongest() {
        let moods: BTreeMap<String, f64> = [
            ("moodHappy".to_string(), 0.2),
            ("moodSad".to_string(), 0.8),
            ("moodParty".to_string(), 0.0),
        ]
        .into();
        assert_eq!(
            mood_bars(&moods),
            [
                ("moodSad".to_string(), 100.0),
                ("moodHappy".to_string(), 25.0)
            ]
        );
        assert!(mood_bars(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn percent_rounds_like_math_round() {
        assert_eq!(percent(0.424), "42%");
        assert_eq!(percent(0.425), "43%");
        assert_eq!(percent(1.0), "100%");
    }
}
