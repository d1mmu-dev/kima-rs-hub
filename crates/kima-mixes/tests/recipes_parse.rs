//! M5b Task 3: the vendored recipe and definition files, the recipe parser and the mood stations. Ports upstream's
//! `recipes.test.ts` (shipped recipes, refusals, `drawMembers`) and `stations.test.ts` (subtitles, the shipped list,
//! value lookup).

use kima_mixes::artist::MIN_MIX_TRACKS;
use kima_mixes::recipes::{
    DEFINITIONS_JSON, RECIPE_LINE, RECIPES, RECIPES_JSON, Recipe, STEADY_BEAT, Signal,
    draw_members, get_recipe, parse_recipes,
};
use kima_mixes::stations::{
    MOOD_STATIONS, StationLookup, mood_station_list, resolve_mood_station, station_subtitle,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// SHA-256 of `backend/src/services/mixes/{recipes,moodDefinitions}.json` at the pin c7527577.
const RECIPES_SHA256: &str = "39eab1022e00e3221d89006038b91f6ef993379b652252c2e1f309a01403b04d";
const DEFINITIONS_SHA256: &str = "eda320788b03ccbab5985957c81b51c40e15a3dfdbcccd96552b06ebc91b51f0";

fn parts(recipe: &Recipe) -> Vec<(String, f64)> {
    recipe
        .components
        .iter()
        .map(|c| (c.signal.name().to_string(), c.weight))
        .collect()
}

fn p(list: &[(&str, f64)]) -> Vec<(String, f64)> {
    list.iter().map(|(s, w)| (s.to_string(), *w)).collect()
}

/// A one-recipe file `{"test": {"name": "Test", <entry>}}`, keys in the written order.
fn one(entry: &str) -> Result<Vec<Recipe>, String> {
    let sep = if entry.is_empty() { "" } else { ", " };
    parse_recipes(&format!(r#"{{"test": {{"name": "Test"{sep}{entry}}}}}"#))
}

fn recipe(components: &str) -> Recipe {
    one(&format!(r#""components": {components}"#))
        .unwrap()
        .remove(0)
}

#[test]
fn vendored_files_match_pin_sha256() {
    assert_eq!(hex(RECIPES_JSON), RECIPES_SHA256);
    assert_eq!(hex(DEFINITIONS_JSON), DEFINITIONS_SHA256);
}

fn hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn shipped_recipes_line_and_ids() {
    assert_eq!(RECIPE_LINE, 1.25);
    assert_eq!(STEADY_BEAT, 2.0);
    let mut ids: Vec<&str> = RECIPES.iter().map(|r| r.id.as_str()).collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "acoustic",
            "calm",
            "calm-station",
            "chill",
            "coffee-shop",
            "dance-floor",
            "dreamy",
            "energetic",
            "energy-station",
            "epic",
            "feel-good-station",
            "focus",
            "focus-calm-station",
            "focus-energy-station",
            "golden-hour",
            "happy",
            "high-energy",
            "hot-girl-walk",
            "in-my-feelings",
            "instrumental",
            "intense",
            "intense-station",
            "late-night",
            "main-character",
            "melancholy",
            "melancholy-station",
            "midnight-drive",
            "monday-motivation",
            "nostalgic",
            "party",
            "rage-cleaning",
            "road-trip",
            "romantic-station",
            "romanticize",
            "sad-girl-sundays",
            "shower-karaoke",
            "sleep",
            "summer",
            "sunday-morning",
            "that-girl-era",
            "unhinged",
            "upbeat",
            "villain-era",
            "workout",
        ]
    );
    // The file's own order is kept (the first two recipes).
    assert_eq!(RECIPES[0].id, "chill");
    assert_eq!(RECIPES[1].id, "late-night");
}

#[test]
fn shipped_stations_blends() {
    assert_eq!(
        get_recipe("feel-good-station").unwrap().components.len(),
        15
    );
    let fc = get_recipe("focus-calm-station").unwrap();
    assert_eq!(parts(fc), p(&[("focus", 0.75), ("calm", 0.25)]));
    assert_eq!(fc.filters.arousal_pct, Some((0.0, 0.75)));
    let fe = get_recipe("focus-energy-station").unwrap();
    assert_eq!(
        parts(fe),
        p(&[("focus", 0.6), ("energetic", 0.25), ("groovy", 0.15)])
    );
    assert_eq!(fe.filters.arousal_pct, Some((0.5, 1.0)));
    assert!(
        parts(get_recipe("melancholy-station").unwrap()).contains(&("arousal".to_string(), -0.175))
    );
}

#[test]
fn low_components_are_negative_and_filters_keep_their_names() {
    assert_eq!(
        parts(get_recipe("happy").unwrap()),
        p(&[
            ("happy", 40.0),
            ("uplifting", 15.0),
            ("arousal", 15.0),
            ("groovy", 10.0),
            ("minor", -10.0),
            ("instrumental", -10.0)
        ])
    );
    let happy = get_recipe("happy").unwrap();
    assert_eq!(happy.components[0].signal, Signal::Mood("happy".into()));
    assert_eq!(happy.components[2].signal, Signal::Arousal);
    assert_eq!(happy.components[4].signal, Signal::Minor);
    assert_eq!(happy.components[5].signal, Signal::Instrumental);
    let focus = get_recipe("focus").unwrap();
    assert_eq!(parts(focus), p(&[("focus", 1.0), ("instrumental", 1.25)]));
    assert_eq!(focus.filters.arousal_pct, Some((0.0, 0.75)));
    let dance = &get_recipe("dance-floor").unwrap().filters;
    assert_eq!(
        (dance.arousal_pct, dance.bpm),
        (Some((0.5, 1.0)), Some((110.0, 140.0)))
    );
    assert_eq!(
        (dance.instrumental_min, dance.instrumental_max, dance.minor),
        (None, None, false)
    );
    let instrumental = &get_recipe("instrumental").unwrap().filters;
    assert_eq!(
        (
            instrumental.instrumental_min,
            instrumental.arousal_pct,
            instrumental.bpm
        ),
        (Some(0.5), Some((0.0, 0.75)), None)
    );
    let karaoke = &get_recipe("shower-karaoke").unwrap().filters;
    assert_eq!(
        (karaoke.arousal_pct, karaoke.instrumental_max),
        (Some((0.35, 1.0)), Some(0.5))
    );
}

#[test]
fn unknown_recipe_id_is_an_error() {
    assert_eq!(
        get_recipe("no-such-mix").unwrap_err(),
        "Unknown recipe: no-such-mix"
    );
}

#[test]
fn parse_refusals() {
    let cases = [
        (r#""components": {"joyful": 1}"#, "unknown component joyful"),
        (
            r#""components": {"calm": 1, "low:joyful": 1}"#,
            "unknown component joyful",
        ),
        (
            r#""components": {"calm": 1, "low:calm": 1}"#,
            "calm appears twice",
        ),
        (r#""components": {"calm": 0}"#, "needs a positive weight"),
        (r#""components": {"calm": -1}"#, "needs a positive weight"),
        (r#""components": {}"#, "no components"),
        (
            r#""components": {"calm": 1}, "filters": {"weekday": 0}"#,
            "unknown filter weekday",
        ),
        (
            r#""components": {"calm": 1}, "filters": {"bpm": [140, 110]}"#,
            "bpm must have",
        ),
        (
            r#""components": {"calm": 1}, "filters": {"arousal_pct": [0.5, 1.5]}"#,
            "arousal_pct must have",
        ),
        (
            r#""components": {"calm": 1}, "filters": {"instrumental_max": 2}"#,
            "a probability",
        ),
        (
            r#""components": {"calm": 1}, "filters": {"minor": false}"#,
            "minor must be true",
        ),
        (
            r#""components": {"calm": 1}, "weekday": 0"#,
            "unknown field weekday",
        ),
        // The retired night mood, and familiarity bounds.
        (r#""components": {"night": 1}"#, "unknown component night"),
        (
            r#""components": {"calm": 1, "low:night": 1}"#,
            "unknown component night",
        ),
        (
            r#""components": {"calm": 1}, "familiarity": 0"#,
            "familiarity must be above 0 and at most 0.5",
        ),
        (
            r#""components": {"calm": 1}, "familiarity": 0.6"#,
            "familiarity must be above 0 and at most 0.5",
        ),
        (
            r#""components": {"calm": 1}, "familiarity": "0.2""#,
            "familiarity must be",
        ),
    ];
    for (entry, message) in cases {
        let err = one(entry).unwrap_err();
        assert!(err.contains(message), "{entry}: {err}");
        assert!(err.starts_with("Recipe test: "), "{err}");
    }
    // Exact upstream texts for the range checks.
    assert_eq!(
        one(r#""components": {"calm": 1}, "filters": {"bpm": [140, 110]}"#).unwrap_err(),
        "Recipe test: bpm must have 0 ≤ low ≤ high ≤ Infinity"
    );
    assert_eq!(
        one(r#""components": {"calm": 1}, "filters": {"arousal_pct": [0.5]}"#).unwrap_err(),
        "Recipe test: arousal_pct must be [low, high]"
    );
    assert_eq!(
        one(r#""components": {"calm": 1}, "filters": {"weekday": 0, "month": 1}"#).unwrap_err(),
        "Recipe test: unknown filter weekday, month"
    );
}

#[test]
fn familiarity_bound_and_no_shipped_recipe_reads_night() {
    let ok =
        parse_recipes(r#"{"ok": {"name": "Ok", "components": {"calm": 1}, "familiarity": 0.5}}"#)
            .unwrap();
    assert_eq!(ok[0].familiarity, Some(0.5));
    for r in RECIPES.iter() {
        assert!(
            !r.components.iter().any(|c| c.signal.name() == "night"),
            "{}",
            r.id
        );
    }
}

#[test]
fn id_must_be_lower_case_words_and_a_name_is_required() {
    let err = parse_recipes(r#"{"Chill Mix": {"name": "Chill", "components": {"calm": 1}}}"#)
        .unwrap_err();
    assert!(err.contains("the id must be"), "{err}");
    let err = parse_recipes(r#"{"chill": {"components": {"calm": 1}}}"#).unwrap_err();
    assert!(err.contains("no name"), "{err}");
}

fn members(n: usize, artist_of: impl Fn(usize) -> String) -> Vec<(String, String)> {
    (0..n).map(|i| (format!("t{i:03}"), artist_of(i))).collect()
}

fn draw(pool: &[(String, String)], seed: &str, limit: usize) -> Vec<String> {
    draw_members(pool, |m| m.0.as_str(), |m| m.1.as_str(), seed, limit)
}

fn per_artist(pool: &[(String, String)], drawn: &[String]) -> Vec<usize> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for id in drawn {
        let artist = &pool.iter().find(|m| &m.0 == id).unwrap().1;
        match counts.iter_mut().find(|(a, _)| a == artist) {
            Some((_, c)) => *c += 1,
            None => counts.push((artist.clone(), 1)),
        }
    }
    counts.into_iter().map(|(_, c)| c).collect()
}

#[test]
fn draw_members_is_seeded_even_and_capped() {
    let pool = members(40, |i| format!("a{i}"));
    let d = draw(&pool, "seed-1", 20);
    assert_eq!(d.len(), 20);
    let mut rev = pool.clone();
    rev.reverse();
    assert_eq!(draw(&rev, "seed-1", 20), d);
    assert_ne!(draw(&pool, "seed-2", 20), d);

    // 4,000 seeds, each of 40 members drawn about half the time (six SDs).
    let mut counts: HashMap<String, usize> = HashMap::new();
    for s in 0..4000 {
        for id in draw(&pool, &format!("seed-{s}"), 20) {
            *counts.entry(id).or_default() += 1;
        }
    }
    assert_eq!(counts.len(), 40);
    assert!(counts.values().all(|&c| c > 1810 && c < 2190), "{counts:?}");

    // A member joining keeps the others' order.
    let pool = members(30, |i| format!("a{i}"));
    let before = draw(&pool, "seed", 30);
    let mut more = pool.clone();
    more.push(("t999".into(), "a999".into()));
    let after: Vec<String> = draw(&more, "seed", 31)
        .into_iter()
        .filter(|id| id != "t999")
        .collect();
    assert_eq!(after, before);

    // At most 2 per artist while that makes 8; 3 when it does not; no cap when 3 still does not.
    assert_eq!(
        draw(&members(12, |i| format!("a{}", i % 4)), "seed", 20).len(),
        8
    );
    let pool = members(12, |i| format!("a{}", i % 3));
    let d = draw(&pool, "seed", 20);
    assert_eq!(d.len(), 9);
    assert_eq!(per_artist(&pool, &d), [3, 3, 3]);
    assert_eq!(draw(&members(10, |_| "solo".into()), "seed", 20).len(), 10);
    assert_eq!(draw(&members(30, |_| "solo".into()), "seed", 20).len(), 20);
    assert_eq!(
        draw(
            &members(MIN_MIX_TRACKS + 1, |i| format!("a{i}")),
            "seed",
            20
        )
        .len(),
        MIN_MIX_TRACKS + 1
    );

    // A draw of 4 from two artists of ten: two each, on every seed.
    let pool = members(20, |i| format!("a{}", i % 2));
    for s in 0..200 {
        let d = draw(&pool, &format!("seed-{s}"), 4);
        assert_eq!(per_artist(&pool, &d), [2, 2]);
    }
    // Three artists of ten, cap 3: nine songs, each song about 30% of the time.
    let pool = members(30, |i| format!("a{}", i % 3));
    let mut counts: HashMap<String, usize> = HashMap::new();
    for s in 0..4000 {
        let d = draw(&pool, &format!("seed-{s}"), 20);
        assert_eq!(per_artist(&pool, &d), [3, 3, 3]);
        for id in d {
            *counts.entry(id).or_default() += 1;
        }
    }
    assert!(counts.values().all(|&c| c > 1026 && c < 1374), "{counts:?}");
}

#[test]
fn station_subtitle_names_the_three_heaviest_leaning_parts() {
    assert_eq!(
        station_subtitle(&recipe(
            r#"{"low:sleep": 0.175, "low:dark": 0.15, "uplifting": 0.125, "happy": 0.112}"#
        ))
        .unwrap(),
        "Uplifting, happy"
    );
    assert_eq!(
        station_subtitle(&recipe(
            r#"{"calm": 0.1, "low:sleep": 0.5, "dark": 0.2, "epic": 0.15}"#
        ))
        .unwrap(),
        "Dark, epic, calm"
    );
    // Arousal reads either way.
    assert_eq!(
        station_subtitle(&recipe(
            r#"{"low:arousal": 0.4, "calm": 0.3, "dreamy": 0.1}"#
        ))
        .unwrap(),
        "Low energy, calm, dreamy"
    );
    assert_eq!(
        station_subtitle(&recipe(
            r#"{"epic": 0.45, "arousal": 0.2, "uplifting": 0.15}"#
        ))
        .unwrap(),
        "Epic, high energy, uplifting"
    );
    // Underscores as spaces.
    assert_eq!(
        station_subtitle(&recipe(r#"{"aggressive": 0.3, "fun_quirky": 0.2}"#)).unwrap(),
        "Aggressive, fun quirky"
    );
    // Ties in the recipe's order.
    assert_eq!(
        station_subtitle(&recipe(
            r#"{"sad": 2, "romantic": 1, "dreamy": 1, "calm": 1}"#
        ))
        .unwrap(),
        "Sad, romantic, dreamy"
    );
    assert_eq!(
        station_subtitle(&recipe(r#"{"sad": 2, "dreamy": 1, "romantic": 1}"#)).unwrap(),
        "Sad, dreamy, romantic"
    );
    // A changed weight changes the line.
    assert_eq!(
        station_subtitle(&recipe(
            r#"{"summer": 0.35, "happy": 0.2, "groovy": 0.175}"#
        ))
        .unwrap(),
        "Summer, happy, groovy"
    );
    assert_eq!(
        station_subtitle(&recipe(r#"{"summer": 0.35, "happy": 0.2, "groovy": 0.25}"#)).unwrap(),
        "Summer, groovy, happy"
    );
    // Fewer than three; none at all.
    assert_eq!(station_subtitle(&recipe(r#"{"epic": 1}"#)).unwrap(), "Epic");
    let err = station_subtitle(&recipe(r#"{"low:sleep": 1, "low:dark": 1}"#)).unwrap_err();
    assert!(err.contains("no subtitle"), "{err}");
}

#[test]
fn station_list_order_and_subtitles() {
    let wildcard = MOOD_STATIONS
        .iter()
        .find(|s| s.value == "wildcard")
        .unwrap();
    assert_eq!(wildcard.recipe, None);
    let late = MOOD_STATIONS
        .iter()
        .find(|s| s.value == "late-night")
        .unwrap();
    assert_eq!(late.recipe, Some("late-night"));

    let list = mood_station_list();
    let names: Vec<(&str, &str)> = list.iter().map(|s| (s.value, s.name)).collect();
    assert_eq!(
        names,
        [
            ("calm", "Calm"),
            ("energy", "Energy"),
            ("intense", "Intense"),
            ("happy", "Happy"),
            ("melancholy", "Melancholy"),
            ("focus-calm", "Focus: Calm"),
            ("focus-energy", "Focus: Energy"),
            ("feel-good", "Feel-good"),
            ("late-night", "Late Night"),
            ("romantic", "Romantic"),
            ("road-trip", "Road Trip"),
            ("nostalgic", "Nostalgic"),
            ("epic", "Epic"),
            ("wildcard", "Wildcard"),
        ]
    );
    for s in &list {
        assert!(!s.description.is_empty());
    }
    assert_eq!(list[13].description, "Songs with no mood tag");
    assert_eq!(list[7].description, "Uplifting, happy, epic");
}

#[test]
fn late_night_is_personal() {
    let list = mood_station_list();
    assert_eq!(
        list[8].description,
        "The songs you play after dark, and songs like them"
    );
    let late = MOOD_STATIONS
        .iter()
        .find(|s| s.value == "late-night")
        .unwrap();
    assert!(late.personal.is_some());
    assert!(
        MOOD_STATIONS
            .iter()
            .filter(|s| s.personal.is_some())
            .count()
            == 1
    );
}

#[test]
fn station_lookup_current_alias_retired_unknown() {
    match resolve_mood_station("Late-Night") {
        StationLookup::Station { value, station } => {
            assert_eq!(value, "late-night");
            assert_eq!(station.value, "late-night");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(resolve_mood_station("wildcard"), StationLookup::Station { value, .. } if value == "wildcard")
    );
    // An older app's focus is Focus: Calm.
    assert!(
        matches!(resolve_mood_station("focus"), StationLookup::Station { value, .. } if value == "focus-calm")
    );
}

#[test]
fn retired_values_answer_retired() {
    for value in ["summer", "3am-thoughts", "friday-night"] {
        assert_eq!(
            resolve_mood_station(value),
            StationLookup::Retired {
                value: value.to_string()
            }
        );
    }
}

#[test]
fn unknown_value_is_unknown() {
    assert_eq!(
        resolve_mood_station("Nonsense"),
        StationLookup::Unknown {
            value: "nonsense".into()
        }
    );
    assert_eq!(
        resolve_mood_station(""),
        StationLookup::Unknown { value: "".into() }
    );
}
