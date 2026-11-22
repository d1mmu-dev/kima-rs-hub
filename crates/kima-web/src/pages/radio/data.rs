//! The radio page's stations as tiles (upstream `features/home/radioData.ts`): the four quick stations, the mood
//! stations the server lists, its genre and decade stations, each with its colours and the cover the server picked.
//! Pure, tested natively.

use std::collections::HashMap;

use kima_api_types::radio::RowStation;
use unicode_normalization::UnicodeNormalization;

/// A tile's colours: the hover border class (written out in full so Tailwind finds it) and the station's hue (the
/// first colour of upstream's accent gradient, `stationHue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub hover_border: &'static str,
    pub hue: &'static str,
}

const fn style(hover_border: &'static str, hue: &'static str) -> Style {
    Style { hover_border, hue }
}

/// Upstream `DEFAULT_STYLE`.
pub const DEFAULT_STYLE: Style = style("hover:border-[#6b7280]/40", "#6b7280");

/// One station tile (upstream `RadioStation`): what it plays (`kind`, `value`) and how many songs it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioStation {
    pub id: String,
    pub name: String,
    pub description: String,
    pub style: Style,
    pub kind: &'static str,
    pub value: Option<String>,
    pub min_tracks: usize,
}

/// The mood, Wildcard and workout stations play every song that matches, however few, so any song is enough.
const STATION_MIN_TRACKS: usize = 1;

/// Upstream `STATIC_STATIONS`: (type, name, description, style, minTracks).
const STATIC_STATIONS: [(&str, &str, &str, Style, usize); 4] = [
    (
        "all",
        "Shuffle All",
        "Your entire library",
        style("hover:border-brand/40", "#fca208"),
        10,
    ),
    (
        "workout",
        "Workout",
        "High energy tracks",
        style("hover:border-[#ef4444]/40", "#ef4444"),
        STATION_MIN_TRACKS,
    ),
    (
        "discovery",
        "Discovery",
        "Lesser-played gems",
        style("hover:border-[#22c55e]/40", "#22c55e"),
        20,
    ),
    (
        "favorites",
        "Favorites",
        "Most played",
        style("hover:border-[#f43f5e]/40", "#f43f5e"),
        10,
    ),
];

/// Upstream `MOOD_STATION_STYLES`, in its order (the spare genre colours follow this order).
const MOOD_STATION_STYLES: [(&str, Style); 14] = [
    ("calm", style("hover:border-[#5bc2b4]/40", "#5bc2b4")),
    ("energy", style("hover:border-[#e06ab0]/40", "#e06ab0")),
    ("intense", style("hover:border-[#e0685e]/40", "#e0685e")),
    ("happy", style("hover:border-[#f0b25a]/40", "#f0b25a")),
    ("melancholy", style("hover:border-[#b98fe0]/40", "#b98fe0")),
    ("focus-calm", style("hover:border-[#7fa8d8]/40", "#7fa8d8")),
    (
        "focus-energy",
        style("hover:border-[#7fa8d8]/40", "#7fa8d8"),
    ),
    ("feel-good", style("hover:border-[#f0a05a]/40", "#f0a05a")),
    ("late-night", style("hover:border-[#6f6fc4]/40", "#6f6fc4")),
    ("romantic", style("hover:border-[#e0789a]/40", "#e0789a")),
    ("road-trip", style("hover:border-[#e8875a]/40", "#e8875a")),
    ("nostalgic", style("hover:border-[#d8a878]/40", "#d8a878")),
    ("epic", style("hover:border-[#e0685e]/40", "#e0685e")),
    ("wildcard", style("hover:border-[#a3a3a3]/40", "#a3a3a3")),
];

/// Upstream `GENRE_STYLES`.
const GENRE_STYLES: [(&str, Style); 15] = [
    ("rock", style("hover:border-[#ef4444]/40", "#ef4444")),
    ("pop", style("hover:border-[#facc15]/40", "#facc15")),
    ("hip hop", style("hover:border-[#a855f7]/40", "#a855f7")),
    ("rap", style("hover:border-[#a855f7]/40", "#a855f7")),
    ("electronic", style("hover:border-[#06b6d4]/40", "#06b6d4")),
    ("jazz", style("hover:border-[#f59e0b]/40", "#f59e0b")),
    ("country", style("hover:border-[#fb923c]/40", "#fb923c")),
    ("folk", style("hover:border-[#22c55e]/40", "#22c55e")),
    ("indie", style("hover:border-[#8b5cf6]/40", "#8b5cf6")),
    ("alternative", style("hover:border-[#6366f1]/40", "#6366f1")),
    ("r&b", style("hover:border-[#c084fc]/40", "#c084fc")),
    ("soul", style("hover:border-[#d97706]/40", "#d97706")),
    ("blues", style("hover:border-[#2563eb]/40", "#2563eb")),
    ("punk", style("hover:border-[#84cc16]/40", "#84cc16")),
    ("reggae", style("hover:border-[#4ade80]/40", "#4ade80")),
];

/// Upstream `DECADE_ACCENT_COLORS`, ascending.
const DECADE_STYLES: [(i32, Style); 13] = [
    (1700, style("hover:border-[#92400e]/40", "#92400e")),
    (1800, style("hover:border-[#64748b]/40", "#64748b")),
    (1900, style("hover:border-[#fbbf24]/40", "#fbbf24")),
    (1920, style("hover:border-[#eab308]/40", "#eab308")),
    (1940, style("hover:border-[#f87171]/40", "#f87171")),
    (1950, style("hover:border-[#e11d48]/40", "#e11d48")),
    (1960, style("hover:border-[#f59e0b]/40", "#f59e0b")),
    (1970, style("hover:border-[#f97316]/40", "#f97316")),
    (1980, style("hover:border-[#22d3ee]/40", "#22d3ee")),
    (1990, style("hover:border-[#a855f7]/40", "#a855f7")),
    (2000, style("hover:border-[#3b82f6]/40", "#3b82f6")),
    (2010, style("hover:border-[#14b8a6]/40", "#14b8a6")),
    (2020, style("hover:border-[#f97316]/40", "#f97316")),
];

/// The page's tiles by section, and each tile's cover by station id (upstream `buildRadioRow`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadioRow {
    pub quick: Vec<RadioStation>,
    pub moods: Vec<RadioStation>,
    pub genres: Vec<RadioStation>,
    pub decades: Vec<RadioStation>,
    pub covers: HashMap<String, Option<String>>,
}

impl RadioRow {
    pub fn total(&self) -> usize {
        self.quick.len() + self.moods.len() + self.genres.len() + self.decades.len()
    }
}

/// The quick stations the server lists, in upstream's order.
pub fn quick_stations(listed: &[&str]) -> Vec<RadioStation> {
    STATIC_STATIONS
        .iter()
        .filter(|(kind, ..)| listed.contains(kind))
        .map(
            |&(kind, name, description, style, min_tracks)| RadioStation {
                id: kind.to_owned(),
                name: name.to_owned(),
                description: description.to_owned(),
                style,
                kind,
                value: None,
                min_tracks,
            },
        )
        .collect()
}

/// The server's mood stations as tiles, in its order (upstream `buildMoodStations`).
pub fn mood_stations(stations: &[(String, String, String)]) -> Vec<RadioStation> {
    stations
        .iter()
        .map(|(value, name, description)| RadioStation {
            id: format!("mood-{value}"),
            name: name.clone(),
            description: description.clone(),
            style: MOOD_STATION_STYLES
                .iter()
                .find(|(mood, _)| mood == value)
                .map_or(DEFAULT_STYLE, |(_, style)| *style),
            kind: "mood",
            value: Some(value.clone()),
            min_tracks: STATION_MIN_TRACKS,
        })
        .collect()
}

/// Upstream `buildGenreStations`: the server's spelling for name and value, the genre's colour.
pub fn genre_stations(genres: &[(String, i64)]) -> Vec<RadioStation> {
    genres
        .iter()
        .map(|(genre, count)| RadioStation {
            id: format!("genre-{genre}"),
            name: genre.clone(),
            description: plural(*count, "track"),
            style: genre_style(genre),
            kind: "genre",
            value: Some(genre.clone()),
            min_tracks: 15,
        })
        .collect()
}

/// Upstream `buildDecadeStations`.
pub fn decade_stations(decades: &[(i32, i64)]) -> Vec<RadioStation> {
    decades
        .iter()
        .map(|&(decade, count)| RadioStation {
            id: format!("decade-{decade}"),
            name: decade_name(decade),
            description: plural(count, "track"),
            style: decade_style(decade),
            kind: "decade",
            value: Some(decade.to_string()),
            min_tracks: 15,
        })
        .collect()
}

/// Upstream `buildRadioRow`: sections from the server's reply, covers keyed by station id.
pub fn build_radio_row(entries: &[RowStation]) -> RadioRow {
    let listed: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    let quick = quick_stations(&listed);
    let moods = mood_stations(
        &entries
            .iter()
            .filter(|e| e.kind == "mood")
            .filter_map(|e| {
                Some((
                    e.value.clone()?,
                    e.name.clone().unwrap_or_default(),
                    e.description.clone().unwrap_or_default(),
                ))
            })
            .collect::<Vec<_>>(),
    );
    let genres = genre_stations(
        &entries
            .iter()
            .filter(|e| e.kind == "genre")
            .filter_map(|e| Some((e.value.clone()?, e.count.unwrap_or(0))))
            .collect::<Vec<_>>(),
    );
    // Upstream reads the decade with `Number(value)`; a value that is not a number names no decade here.
    let decades = decade_stations(
        &entries
            .iter()
            .filter(|e| e.kind == "decade")
            .filter_map(|e| {
                Some((
                    e.value.as_deref()?.trim().parse().ok()?,
                    e.count.unwrap_or(0),
                ))
            })
            .collect::<Vec<_>>(),
    );
    let key = |kind: &str, value: Option<&str>| format!("{kind}:{}", value.unwrap_or(""));
    let cover_of: HashMap<String, Option<String>> = entries
        .iter()
        .map(|e| (key(&e.kind, e.value.as_deref()), e.cover.clone()))
        .collect();
    let covers = quick
        .iter()
        .chain(&moods)
        .chain(&genres)
        .chain(&decades)
        .map(|s| {
            let cover = cover_of
                .get(&key(s.kind, s.value.as_deref()))
                .cloned()
                .flatten();
            (s.id.clone(), cover)
        })
        .collect();
    RadioRow {
        quick,
        moods,
        genres,
        decades,
        covers,
    }
}

/// ECMAScript `\s` (no U+0085, unlike Rust's `char::is_whitespace`).
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// A genre name as the server keys it (upstream `genreKey`: NFKC, lower case, separators dropped).
pub fn genre_key(genre: &str) -> String {
    genre
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .chars()
        .filter(|&c| {
            !(js_space(c)
                || matches!(
                    c,
                    '\u{a0}'
                        | '\u{202f}'
                        | '\u{ad}'
                        | '\u{200b}'
                        | '\u{feff}'
                        | '_'
                        | '/'
                        | '.'
                        | '\u{b7}'
                        | '\''
                        | '\u{2018}'
                        | '\u{2019}'
                        | '\u{2010}'
                        ..='\u{2015}' | '\u{2043}' | '\u{2212}' | '-'
                ))
        })
        .collect()
}

/// A genre's colour: its own if it has one, otherwise one of the mood colours (Wildcard's grey aside), picked from
/// its name so it never changes (upstream `getGenreStyle`).
pub fn genre_style(genre: &str) -> Style {
    let key = genre_key(genre);
    if let Some((_, own)) = GENRE_STYLES.iter().find(|(name, _)| genre_key(name) == key) {
        return *own;
    }
    let spare: Vec<Style> = MOOD_STATION_STYLES
        .iter()
        .filter(|(mood, _)| *mood != "wildcard")
        .map(|(_, style)| *style)
        .collect();
    let hash = key
        .chars()
        .fold(0_u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    spare[hash as usize % spare.len()]
}

/// Upstream `getDecadeStyle`: the colour of the latest known decade at or before it.
pub fn decade_style(decade: i32) -> Style {
    DECADE_STYLES
        .iter()
        .rev()
        .find(|(known, _)| decade >= *known)
        .map_or(DEFAULT_STYLE, |(_, style)| *style)
}

/// The twentieth century's decades by two digits ("80s"), every other by four ("1850s", "2010s").
pub fn decade_name(decade: i32) -> String {
    if (1900..2000).contains(&decade) {
        format!("{:02}s", decade % 100)
    } else {
        format!("{decade}s")
    }
}

/// A count as upstream's `toLocaleString()` writes it in English: `1,234,567`.
pub fn group_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    if n < 0 {
        grouped.push('-');
    }
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(d);
    }
    grouped
}

/// Upstream `plural`: `1 track`, `1,234 tracks`.
pub fn plural(n: i64, word: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("{} {word}{s}", group_thousands(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genre(g: &str) -> RadioStation {
        genre_stations(&[(g.to_owned(), 20)]).remove(0)
    }

    fn moods(list: &[(&str, &str, &str)]) -> Vec<RadioStation> {
        mood_stations(
            &list
                .iter()
                .map(|(v, n, d)| ((*v).to_owned(), (*n).to_owned(), (*d).to_owned()))
                .collect::<Vec<_>>(),
        )
    }

    fn entry(kind: &str, value: Option<&str>, cover: Option<&str>) -> RowStation {
        RowStation {
            kind: kind.into(),
            value: value.map(Into::into),
            name: None,
            description: None,
            count: None,
            cover: cover.map(Into::into),
        }
    }

    #[test]
    fn every_spelling_of_a_genre_gets_the_genres_colour() {
        let hip_hop = genre("hip hop");
        for spelling in [
            "hip-hop",
            "hiphop",
            "Hip_Hop",
            "hip\u{2010}hop",
            "hip\u{a0}hop",
            "hip\u{ad}hop",
            "hip\u{2212}hop",
            "\u{ff28}\u{ff29}\u{ff30}\u{ff0d}\u{ff28}\u{ff2f}\u{ff30}",
        ] {
            assert_eq!(genre(spelling).style, hip_hop.style, "{spelling:?}");
        }
        assert_ne!(hip_hop.style, genre("not a listed genre").style);
    }

    #[test]
    fn a_genre_station_keeps_the_servers_spelling() {
        let s = genre("Hip-Hop");
        assert_eq!(s.name, "Hip-Hop");
        assert_eq!((s.kind, s.value.as_deref()), ("genre", Some("Hip-Hop")));
        assert_eq!(s.id, "genre-Hip-Hop");
        assert_eq!(s.description, "20 tracks");
        assert_eq!(s.min_tracks, 15);
    }

    #[test]
    fn genres_without_a_colour_get_a_spare_one_stable_per_spelling_and_spread() {
        for g in [
            "metalcore",
            "synth-pop",
            "alternative rock",
            "deathcore",
            "shoegaze",
            "metal",
            "classical",
        ] {
            assert_ne!(genre(g).style, DEFAULT_STYLE, "{g}");
        }
        assert_eq!(genre("Synth Pop").style, genre("synth-pop").style);
        let spread: std::collections::HashSet<_> = [
            "metalcore",
            "synth-pop",
            "alternative rock",
            "deathcore",
            "shoegaze",
            "progressive rock",
            "alternative metal",
            "indie rock",
        ]
        .iter()
        .map(|g| genre(g).style.hue)
        .collect();
        assert!(spread.len() >= 5, "{spread:?}");
    }

    #[test]
    fn genre_key_keeps_nel_unlike_rust_whitespace() {
        // ECMAScript `\s` does not include U+0085.
        assert_eq!(genre_key("a\u{85}b"), "a\u{85}b");
        assert_eq!(genre_key(" Hip\u{feff}-Hop\u{3000}"), "hiphop");
    }

    #[test]
    fn mood_stations_are_the_servers_in_its_order_with_any_song_enough() {
        let tiles = moods(&[
            ("calm", "Calm", "Calm, dreamy, low energy"),
            ("wildcard", "Wildcard", "Songs with no mood tag"),
            ("brand-new", "Brand New", "Epic, high energy"),
        ]);
        let shown: Vec<_> = tiles
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str(), t.kind, t.value.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("mood-calm", "Calm", "mood", Some("calm")),
                ("mood-wildcard", "Wildcard", "mood", Some("wildcard")),
                ("mood-brand-new", "Brand New", "mood", Some("brand-new")),
            ]
        );
        assert_eq!(tiles[0].style.hue, "#5bc2b4");
        // A station the page has no colours for plays in the default ones.
        assert_eq!(tiles[2].style, DEFAULT_STYLE);
        assert!(tiles.iter().all(|t| t.min_tracks == 1));
        assert_eq!(quick_stations(&["workout"])[0].min_tracks, 1);
    }

    #[test]
    fn every_listed_mood_station_has_its_own_colours() {
        // The server's 14 stations (kima-api-types fixture `m5b_radio_stations_response.json`).
        for value in [
            "calm",
            "energy",
            "intense",
            "happy",
            "melancholy",
            "focus-calm",
            "focus-energy",
            "feel-good",
            "late-night",
            "romantic",
            "road-trip",
            "nostalgic",
            "epic",
            "wildcard",
        ] {
            let tile = moods(&[(value, value, value)]).remove(0);
            assert_ne!(tile.style, DEFAULT_STYLE, "{value}");
        }
    }

    #[test]
    fn decades_are_named_and_coloured_like_upstream() {
        assert_eq!(decade_name(1980), "80s");
        assert_eq!(decade_name(1900), "00s");
        assert_eq!(decade_name(2010), "2010s");
        assert_eq!(decade_name(1850), "1850s");
        assert_eq!(decade_style(1995).hue, "#a855f7");
        assert_eq!(decade_style(1930).hue, "#eab308");
        assert_eq!(decade_style(2030).hue, "#f97316");
        assert_eq!(decade_style(1600), DEFAULT_STYLE);
        let d = decade_stations(&[(1990, 23)]).remove(0);
        assert_eq!(
            (d.id.as_str(), d.name.as_str(), d.description.as_str()),
            ("decade-1990", "90s", "23 tracks")
        );
        assert_eq!(
            (d.kind, d.value.as_deref(), d.min_tracks),
            ("decade", Some("1990"), 15)
        );
    }

    #[test]
    fn plural_groups_thousands_and_drops_the_s_for_one() {
        assert_eq!(plural(1, "track"), "1 track");
        assert_eq!(plural(0, "track"), "0 tracks");
        assert_eq!(plural(1234567, "track"), "1,234,567 tracks");
    }

    #[test]
    fn the_row_is_split_into_sections_with_covers_by_station_id() {
        let mut calm = entry("mood", Some("calm"), Some("c"));
        calm.name = Some("Calm".into());
        calm.description = Some("Calm, dreamy".into());
        let mut rock = entry("genre", Some("rock"), Some("r"));
        rock.count = Some(20);
        let mut nineties = entry("decade", Some("1990"), Some("n"));
        nineties.count = Some(23);
        let row = build_radio_row(&[
            entry("favorites", None, None),
            entry("all", None, Some("a")),
            entry("discovery", None, Some("d")),
            calm,
            rock,
            nineties,
        ]);
        let names = |list: &[RadioStation]| list.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        // Quick stations in upstream's order, only the listed ones (Workout is not listed: no mood statistics).
        assert_eq!(names(&row.quick), ["Shuffle All", "Discovery", "Favorites"]);
        assert_eq!(names(&row.moods), ["Calm"]);
        assert_eq!(names(&row.genres), ["rock"]);
        assert_eq!(names(&row.decades), ["90s"]);
        assert_eq!(row.total(), 6);
        assert_eq!(row.covers["all"].as_deref(), Some("a"));
        assert_eq!(row.covers["favorites"], None);
        assert_eq!(row.covers["mood-calm"].as_deref(), Some("c"));
        assert_eq!(row.covers["genre-rock"].as_deref(), Some("r"));
        assert_eq!(row.covers["decade-1990"].as_deref(), Some("n"));
    }

    #[test]
    fn the_captured_row_builds_its_tiles() {
        let raw = include_str!(
            "../../../../kima-api-types/tests/fixtures/m5b_radio_row_all_response.json"
        );
        let reply: kima_api_types::radio::RadioRowReply = serde_json::from_str(raw).unwrap();
        let row = build_radio_row(&reply.stations);
        assert_eq!(row.quick.len(), 3);
        assert!(
            row.moods.is_empty(),
            "no mood statistics: no mood tiles (D-266)"
        );
        assert_eq!(row.total(), reply.stations.len());
        assert!(row.covers.values().all(Option::is_some));
    }
}
