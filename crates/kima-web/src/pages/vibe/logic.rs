//! Pure helpers of the vibe page, ported from upstream `features/vibe/{mapUtils,librarySearch,
//! useTrackFinder,MoodLegend,TrackPanel}.ts(x)`: mood names and colours, the legend's counts, the
//! title/artist search over the loaded map, the merged name/sound results, the 2D projection of the
//! map and the selected track's mood bars.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use kima_api_types::vibe::{MapResponse, MapTrack, SearchTrack};
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

/// One SVG path drawing a circle of radius `r` around each point (two arcs per circle), so a layer
/// of thousands of lit points is one element instead of one per point.
pub fn circles_path(points: &[(f64, f64)], r: f64) -> String {
    use std::fmt::Write;
    let mut d = String::with_capacity(points.len() * 40);
    for (x, y) in points {
        let _ = write!(
            d,
            "M{:.1} {:.1}a{r} {r} 0 1 0 {d2} 0a{r} {r} 0 1 0 -{d2} 0",
            x - r,
            y,
            d2 = 2.0 * r
        );
    }
    d
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

/// How long a loaded map is shown without asking the server again (upstream `staleTime`: 1 h).
pub const MAP_FRESH_MS: u64 = 3_600_000;

/// What a page does with the map kept from an earlier visit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheUse {
    /// Nothing kept: load it.
    Missing,
    /// Younger than [`MAP_FRESH_MS`]: show it, ask nothing.
    Fresh,
    /// Older: show it and ask once in the background.
    Stale,
}

pub fn cache_use(fetched_at: Option<u64>, now: u64) -> CacheUse {
    match fetched_at {
        None => CacheUse::Missing,
        Some(at) if now.saturating_sub(at) < MAP_FRESH_MS => CacheUse::Fresh,
        Some(_) => CacheUse::Stale,
    }
}

/// Two answers draw the same map when the layout time, mood signature and size agree; the page then
/// keeps the drawing it has.
pub fn same_map(a: &MapResponse, b: &MapResponse) -> bool {
    a.computed_at == b.computed_at
        && a.mood_signature == b.mood_signature
        && a.track_count == b.track_count
        && a.tracks.len() == b.tracks.len()
}

/// A counter that lets only the latest of several async tasks act: each task takes a number with
/// [`Generation::begin`] and checks [`Generation::is_current`] after every await.
#[derive(Debug, Clone, Default)]
pub struct Generation(Rc<Cell<u32>>);

impl Generation {
    pub fn begin(&self) -> u32 {
        let next = self.0.get().wrapping_add(1);
        self.0.set(next);
        next
    }

    pub fn is_current(&self, generation: u32) -> bool {
        self.0.get() == generation
    }
}

/// Whether the open panel's similar-tracks request and highlight (made for `matches_of`) stay when
/// `selected` becomes the open track: only when it is the same track.
pub fn keeps_matches(matches_of: Option<&str>, selected: Option<&str>) -> bool {
    matches_of.is_some() && matches_of == selected
}

/// Why a sound search gave no tracks: the server has no text model (503, a quiet notice that stops
/// further sound searches on this page) or another error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundFailure {
    Unavailable,
    Error(String),
}

pub fn sound_failure(status: Option<u32>, message: String) -> SoundFailure {
    if status == Some(503) {
        SoundFailure::Unavailable
    } else {
        SoundFailure::Error(message)
    }
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
    fn circles_path_draws_two_arcs_per_point() {
        assert_eq!(circles_path(&[], 6.0), "");
        assert_eq!(
            circles_path(&[(500.0, 250.5)], 6.0),
            "M494.0 250.5a6 6 0 1 0 12 0a6 6 0 1 0 -12 0"
        );
        let two = circles_path(&[(10.0, 20.0), (30.04, 40.06)], 6.0);
        assert_eq!(
            two,
            "M4.0 20.0a6 6 0 1 0 12 0a6 6 0 1 0 -12 0M24.0 40.1a6 6 0 1 0 12 0a6 6 0 1 0 -12 0"
        );
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

    fn map(computed_at: &str, signature: &str, n: usize) -> MapResponse {
        MapResponse {
            tracks: (0..n)
                .map(|i| track(&i.to_string(), "t", "a", "neutral"))
                .collect(),
            track_count: n,
            computed_at: computed_at.into(),
            mood_signature: signature.into(),
        }
    }

    #[test]
    fn a_kept_map_is_fresh_for_an_hour_then_stale() {
        assert_eq!(cache_use(None, 5), CacheUse::Missing);
        assert_eq!(cache_use(Some(1_000), 1_000), CacheUse::Fresh);
        assert_eq!(
            cache_use(Some(1_000), 1_000 + MAP_FRESH_MS - 1),
            CacheUse::Fresh
        );
        assert_eq!(
            cache_use(Some(1_000), 1_000 + MAP_FRESH_MS),
            CacheUse::Stale
        );
        // A clock that went backwards does not keep a map forever, nor panic.
        assert_eq!(cache_use(Some(5_000), 1_000), CacheUse::Fresh);
    }

    #[test]
    fn same_map_compares_layout_time_signature_and_size() {
        let a = map("2026-11-18T00:00:00.000Z", "s", 2);
        assert!(same_map(&a, &map("2026-11-18T00:00:00.000Z", "s", 2)));
        assert!(!same_map(&a, &map("2026-11-18T01:00:00.000Z", "s", 2)));
        assert!(!same_map(&a, &map("2026-11-18T00:00:00.000Z", "t", 2)));
        assert!(!same_map(&a, &map("2026-11-18T00:00:00.000Z", "s", 3)));
    }

    #[test]
    fn only_the_latest_generation_is_current() {
        let g = Generation::default();
        let first = g.begin();
        assert!(g.is_current(first));
        let clone = g.clone();
        let second = clone.begin();
        assert_ne!(first, second);
        assert!(
            !g.is_current(first),
            "a newer task (any clone) retires the older one"
        );
        assert!(g.is_current(second));
    }

    #[test]
    fn matches_stay_only_for_the_same_track() {
        assert!(keeps_matches(Some("a"), Some("a")));
        assert!(!keeps_matches(Some("a"), Some("b")));
        assert!(!keeps_matches(Some("a"), None));
        assert!(!keeps_matches(None, Some("a")));
        assert!(!keeps_matches(None, None));
    }

    #[test]
    fn a_503_sound_search_means_no_text_model() {
        assert_eq!(
            sound_failure(Some(503), "Text embedding service unavailable".into()),
            SoundFailure::Unavailable
        );
        assert_eq!(
            sound_failure(Some(500), "boom".into()),
            SoundFailure::Error("boom".into())
        );
        assert_eq!(
            sound_failure(None, "offline".into()),
            SoundFailure::Error("offline".into())
        );
    }

    #[test]
    fn percent_rounds_like_math_round() {
        assert_eq!(percent(0.424), "42%");
        assert_eq!(percent(0.425), "43%");
        assert_eq!(percent(1.0), "100%");
    }
}
