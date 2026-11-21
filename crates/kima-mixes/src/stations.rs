//! The mood stations (upstream `services/mixes/stations.ts`): the 13 that play a recipe's members and Wildcard, the
//! songs with no mood tag, each with a subtitle generated from its recipe.

use crate::recipes::{Component, RECIPES, Recipe, Signal};
use std::sync::LazyLock;

/// A station whose songs come from the listener's own plays (nightPlays.ts); its recipe is only the cold-start
/// fallback, so its subtitle says what it really is.
#[derive(Debug, Clone, PartialEq)]
pub struct Personal {
    pub kind: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MoodStation {
    /// The radio route's `value` for `type=mood`.
    pub value: &'static str,
    pub name: &'static str,
    /// The recipe the station plays; none for Wildcard.
    pub recipe: Option<&'static str>,
    pub personal: Option<Personal>,
}

/// What an app's station value names (upstream `StationLookup`).
#[derive(Debug, Clone, PartialEq)]
pub enum StationLookup {
    Station {
        value: String,
        station: &'static MoodStation,
    },
    /// A station that no longer exists: the route answers an empty list, not an error.
    Retired {
        value: String,
    },
    Unknown {
        value: String,
    },
}

/// A station as `GET /library/radio/stations` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct StationListing {
    pub value: &'static str,
    pub name: &'static str,
    pub description: String,
}

/// The stations in their approved order; five share their mix's recipe.
const STATIONS: [(&str, &str, Option<&str>); 14] = [
    ("calm", "Calm", Some("calm-station")),
    ("energy", "Energy", Some("energy-station")),
    ("intense", "Intense", Some("intense-station")),
    ("happy", "Happy", Some("happy")),
    ("melancholy", "Melancholy", Some("melancholy-station")),
    ("focus-calm", "Focus: Calm", Some("focus-calm-station")),
    (
        "focus-energy",
        "Focus: Energy",
        Some("focus-energy-station"),
    ),
    ("feel-good", "Feel-good", Some("feel-good-station")),
    ("late-night", "Late Night", Some("late-night")),
    ("romantic", "Romantic", Some("romantic-station")),
    ("road-trip", "Road Trip", Some("road-trip")),
    ("nostalgic", "Nostalgic", Some("nostalgic")),
    ("epic", "Epic", Some("epic")),
    ("wildcard", "Wildcard", None),
];

const LATE_NIGHT: Personal = Personal {
    kind: "night",
    description: "The songs you play after dark, and songs like them",
};

const WILDCARD_SUBTITLE: &str = "Songs with no mood tag";

/// Station values that no longer exist (Summer; the 3AM Thoughts and Friday Night mixes' stations).
const RETIRED_STATIONS: [&str; 3] = ["summer", "3am-thoughts", "friday-night"];
/// Old values that now name another station: Focus split into Focus: Calm and Focus: Energy.
const STATION_ALIASES: [(&str, &str); 1] = [("focus", "focus-calm")];

/// A part the subtitle names: what the recipe leans toward, except arousal, which reads either way.
fn named(c: &Component) -> bool {
    c.weight > 0.0 || c.signal == Signal::Arousal
}

fn part_words(c: &Component) -> String {
    if c.signal == Signal::Arousal {
        return if c.weight > 0.0 {
            "high energy"
        } else {
            "low energy"
        }
        .to_string();
    }
    c.signal.name().replace('_', " ")
}

/// Upstream `stationSubtitle`: the recipe's three heaviest named parts, ties in the recipe's own order, as one line
/// with its first letter in upper case.
pub fn station_subtitle(recipe: &Recipe) -> Result<String, String> {
    let mut parts: Vec<(usize, &Component)> = recipe
        .components
        .iter()
        .enumerate()
        .filter(|(_, c)| named(c))
        .collect();
    parts.sort_by(|a, b| {
        b.1.weight
            .abs()
            .partial_cmp(&a.1.weight.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let line = parts
        .iter()
        .take(3)
        .map(|(_, c)| part_words(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut chars = line.chars();
    match chars.next() {
        None => Err(format!(
            "Recipe {}: no part leans toward anything, so no subtitle",
            recipe.id
        )),
        Some(first) => Ok(first.to_uppercase().chain(chars).collect()),
    }
}

/// The shipped stations, each checked once (unique value, known recipe with a subtitle); a bad list is a build defect.
pub static MOOD_STATIONS: LazyLock<Vec<MoodStation>> = LazyLock::new(|| {
    let mut stations: Vec<MoodStation> = Vec::with_capacity(STATIONS.len());
    for (value, name, recipe) in STATIONS {
        assert!(
            !stations.iter().any(|s| s.value == value),
            "Mood station {value} appears twice"
        );
        if let Some(id) = recipe {
            let recipe = RECIPES
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("Mood station {value}: unknown recipe {id}"));
            station_subtitle(recipe).expect("every station recipe has a subtitle");
        }
        stations.push(MoodStation {
            value,
            name,
            recipe,
            personal: (value == "late-night").then_some(LATE_NIGHT),
        });
    }
    stations
});

/// Upstream `resolveMoodStation`: the station an app's value names, read in lower case, an old renamed value naming
/// its successor; a retired value is its own answer.
pub fn resolve_mood_station(requested: &str) -> StationLookup {
    let lower = requested.to_lowercase();
    let value = STATION_ALIASES
        .iter()
        .find(|(old, _)| *old == lower)
        .map_or(lower.clone(), |(_, new)| new.to_string());
    if RETIRED_STATIONS.contains(&value.as_str()) {
        return StationLookup::Retired { value };
    }
    match MOOD_STATIONS.iter().find(|s| s.value == value) {
        Some(station) => StationLookup::Station { value, station },
        None => StationLookup::Unknown { value },
    }
}

/// Upstream `moodStationList`: the stations as the apps list them, each with its subtitle.
pub fn mood_station_list() -> Vec<StationListing> {
    MOOD_STATIONS
        .iter()
        .map(|s| StationListing {
            value: s.value,
            name: s.name,
            description: match (&s.personal, s.recipe) {
                (Some(personal), _) => personal.description.to_string(),
                (None, None) => WILDCARD_SUBTITLE.to_string(),
                (None, Some(id)) => {
                    station_subtitle(RECIPES.iter().find(|r| r.id == id).expect("checked"))
                        .expect("checked")
                }
            },
        })
        .collect()
}
