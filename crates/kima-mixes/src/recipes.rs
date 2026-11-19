//! The recipe engine's pure part (upstream `services/mixes/recipes.ts`): the vendored `recipes.json` (44 recipes) and
//! `moodDefinitions.json`, byte for byte from the pin, the recipe parser with upstream's checks and error texts, and
//! the seeded member draw. The ranking SQL is `kima_db::mixes::recipe` (M5b Task 4).

use crate::artist::cap_by_artist;
use crate::json::{Json, js_number};
use crate::seeded::seeded_order;
use kima_vibe::mood_heads::{MOODS, RETIRED_MOODS};
use std::sync::LazyLock;

/// `backend/src/services/mixes/recipes.json` at the pin, unchanged.
pub const RECIPES_JSON: &str = include_str!("../assets/recipes.json");
/// `backend/src/services/mixes/moodDefinitions.json` at the pin, unchanged.
pub const DEFINITIONS_JSON: &str = include_str!("../assets/moodDefinitions.json");

/// Songs in a daily mix (upstream `helpers.ts` `TRACK_LIMIT`).
pub const TRACK_LIMIT: usize = 20;

/// A song is a member at this many library standard deviations above the average recipe score.
pub const RECIPE_LINE: f64 = 1.25;
/// Upstream's steady beat: Essentia's tempo confidence (0 to 5.32) at least 2. kima's confidence is 0 to 1 from
/// another estimator; the threshold kima applies is [`STEADY_BEAT_KIMA`] (D-262).
pub const STEADY_BEAT: f64 = 2.0;
/// Essentia's highest `bpmConfidence`, the top of upstream's scale.
pub const ESSENTIA_CONFIDENCE_MAX: f64 = 5.32;
/// The steady beat on kima's 0-1 `track_analysis.tempo_confidence`: upstream's threshold rescaled linearly,
/// `2 / 5.32` (about 0.376). Owner decision M5b Q7 = A (D-262); a calibration on a real library is a follow-up.
pub const STEADY_BEAT_KIMA: f64 = STEADY_BEAT / ESSENTIA_CONFIDENCE_MAX;

/// What a component measures: a learned (live) mood, a CLAP reading, or the key's scale (1 minor, 0 major).
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Mood(String),
    Arousal,
    Instrumental,
    Acoustic,
    Minor,
}

impl Signal {
    /// The signal's name in recipes.json (without `low:`).
    pub fn name(&self) -> &str {
        match self {
            Signal::Mood(mood) => mood,
            Signal::Arousal => "arousal",
            Signal::Instrumental => "instrumental",
            Signal::Acoustic => "acoustic",
            Signal::Minor => "minor",
        }
    }

    fn parse(name: &str) -> Option<Signal> {
        match name {
            "arousal" => Some(Signal::Arousal),
            "instrumental" => Some(Signal::Instrumental),
            "acoustic" => Some(Signal::Acoustic),
            "minor" => Some(Signal::Minor),
            mood if is_live_mood(mood) => Some(Signal::Mood(mood.to_string())),
            _ => None,
        }
    }
}

/// Upstream `LIVE_MOODS`: every learned mood but the retired ones (night).
pub fn is_live_mood(name: &str) -> bool {
    MOODS.contains(&name) && !RETIRED_MOODS.contains(&name)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub signal: Signal,
    /// Negative for a `low:` component, which pushes away the songs high in it.
    pub weight: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filters {
    /// Inclusive BPM band; a song passes when its tempo, double or half falls inside, on a steady beat.
    pub bpm: Option<(f64, f64)>,
    /// Inclusive bounds on CLAP's instrumental probability.
    pub instrumental_max: Option<f64>,
    pub instrumental_min: Option<f64>,
    /// Inclusive band of arousal's percentile rank among the recipe's eligible songs.
    pub arousal_pct: Option<(f64, f64)>,
    /// Minor keys only.
    pub minor: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub id: String,
    pub name: String,
    pub components: Vec<Component>,
    pub filters: Filters,
    /// The familiarity boost's share of the score (familiarity.ts), when the recipe carries one.
    pub familiarity: Option<f64>,
}

fn finite_number(v: &Json) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

fn band(value: &Json, where_: &str, min: f64, max: f64) -> Result<(f64, f64), String> {
    let pair = match value {
        Json::Array(items) if items.len() == 2 => items
            .iter()
            .map(finite_number)
            .collect::<Option<Vec<f64>>>(),
        _ => None,
    };
    let Some(pair) = pair else {
        return Err(format!("{where_} must be [low, high]"));
    };
    let (lo, hi) = (pair[0], pair[1]);
    if lo < min || hi > max || lo > hi {
        return Err(format!(
            "{where_} must have {} ≤ low ≤ high ≤ {}",
            js_number(min),
            js_number(max)
        ));
    }
    Ok((lo, hi))
}

fn probability(value: &Json, where_: &str) -> Result<f64, String> {
    match finite_number(value) {
        Some(n) if (0.0..=1.0).contains(&n) => Ok(n),
        _ => Err(format!("{where_} must be a probability")),
    }
}

fn parse_filters(raw: Option<&Json>, where_: &str) -> Result<Filters, String> {
    let Some(raw) = raw else {
        return Ok(Filters::default());
    };
    let Some(entries) = raw.as_object() else {
        return Err(format!("{where_}: filters must be an object"));
    };
    let get = |name: &str| entries.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let unknown: Vec<&str> = entries
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| {
            ![
                "bpm",
                "instrumental_max",
                "instrumental_min",
                "arousal_pct",
                "minor",
            ]
            .contains(k)
        })
        .collect();
    if !unknown.is_empty() {
        return Err(format!("{where_}: unknown filter {}", unknown.join(", ")));
    }
    let mut filters = Filters::default();
    if let Some(bpm) = get("bpm") {
        filters.bpm = Some(band(bpm, &format!("{where_}: bpm"), 0.0, f64::INFINITY)?);
    }
    if let Some(v) = get("instrumental_max") {
        filters.instrumental_max = Some(probability(v, &format!("{where_}: instrumental_max"))?);
    }
    if let Some(v) = get("instrumental_min") {
        filters.instrumental_min = Some(probability(v, &format!("{where_}: instrumental_min"))?);
    }
    if let Some(v) = get("arousal_pct") {
        filters.arousal_pct = Some(band(v, &format!("{where_}: arousal_pct"), 0.0, 1.0)?);
    }
    if let Some(v) = get("minor") {
        if *v != Json::Bool(true) {
            return Err(format!("{where_}: minor must be true when present"));
        }
        filters.minor = true;
    }
    Ok(filters)
}

fn valid_id(id: &str) -> bool {
    // ^[a-z0-9]+(-[a-z0-9]+)*$
    !id.is_empty()
        && id.split('-').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Upstream `parseRecipes`: the recipes file, checked (every id lower-case words joined by hyphens, every component a
/// live mood or a reading or `minor` with a positive weight and no component twice, every filter known and in range),
/// in the file's order. The error texts are upstream's.
pub fn parse_recipes(text: &str) -> Result<Vec<Recipe>, String> {
    let raw = Json::parse(text)?;
    let Some(entries) = raw.as_object() else {
        return Err("Recipes: not an object".into());
    };
    let mut recipes = Vec::with_capacity(entries.len());
    for (id, entry) in entries {
        let where_ = format!("Recipe {id}");
        if !valid_id(id) {
            return Err(format!(
                "{where_}: the id must be lower-case words and hyphens"
            ));
        }
        let Some(fields) = entry.as_object() else {
            return Err(format!("{where_}: not an object"));
        };
        let get = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v);
        let unknown: Vec<&str> = fields
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !["name", "components", "filters", "familiarity"].contains(k))
            .collect();
        if !unknown.is_empty() {
            return Err(format!("{where_}: unknown field {}", unknown.join(", ")));
        }
        let name = match get("name") {
            Some(Json::String(name)) if !name.is_empty() => name.clone(),
            _ => return Err(format!("{where_}: no name")),
        };
        let Some(components) = get("components").and_then(Json::as_object) else {
            return Err(format!("{where_}: components must be an object"));
        };
        let mut parsed: Vec<Component> = Vec::new();
        for (key, weight) in components {
            let (low, signal_name) = match key.strip_prefix("low:") {
                Some(rest) => (true, rest),
                None => (false, key.as_str()),
            };
            let Some(signal) = Signal::parse(signal_name) else {
                return Err(format!("{where_}: unknown component {signal_name}"));
            };
            if parsed.iter().any(|c| c.signal == signal) {
                return Err(format!("{where_}: {signal_name} appears twice"));
            }
            let weight = match finite_number(weight) {
                Some(w) if w > 0.0 => w,
                _ => return Err(format!("{where_}: {key} needs a positive weight")),
            };
            parsed.push(Component {
                signal,
                weight: if low { -weight } else { weight },
            });
        }
        if parsed.is_empty() {
            return Err(format!("{where_}: no components"));
        }
        let familiarity = match get("familiarity") {
            None => None,
            Some(Json::Number(f)) if *f > 0.0 && *f <= 0.5 => Some(*f),
            Some(_) => {
                return Err(format!(
                    "{where_}: familiarity must be above 0 and at most 0.5"
                ));
            }
        };
        let filters = parse_filters(get("filters"), &where_)?;
        recipes.push(Recipe {
            id: id.clone(),
            name,
            components: parsed,
            filters,
            familiarity,
        });
    }
    Ok(recipes)
}

/// The shipped recipes, parsed once; a malformed file is a build defect (upstream throws at load).
pub static RECIPES: LazyLock<Vec<Recipe>> =
    LazyLock::new(|| parse_recipes(RECIPES_JSON).expect("the vendored recipes.json parses"));

/// Upstream `getRecipe`: the recipe with this id; an unknown id is an error, never an empty mix.
pub fn get_recipe(id: &str) -> Result<&'static Recipe, String> {
    RECIPES
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("Unknown recipe: {id}"))
}

/// Upstream `drawMembers`: up to `limit` members in the order of their seeded key, the same draw for one seed
/// whatever order the members come in, at most 2 per artist (relaxed as `cap_by_artist` relaxes it). Returns ids.
pub fn draw_members<T: Clone>(
    members: &[T],
    id_of: impl Fn(&T) -> &str,
    artist_of: impl Fn(&T) -> &str,
    seed: &str,
    limit: usize,
) -> Vec<String> {
    let ordered = seeded_order(members.to_vec(), seed, &id_of);
    cap_by_artist(&ordered, &artist_of, limit)
        .iter()
        .map(|m| id_of(m).to_string())
        .collect()
}
