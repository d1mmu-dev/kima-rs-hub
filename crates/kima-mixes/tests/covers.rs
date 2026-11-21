//! The radio row's pure parts (M5b Task 14; port of upstream `services/mixes/radioRow.ts` `rowStations` and
//! `services/mixes/stationCovers.ts` `localDay` at the pin `c7527577`, unchanged at the reference checkout
//! `9450c94b`).

use kima_mixes::covers::{
    COVER_CONCURRENCY, COVER_SAMPLE, HOME_DECADES, HOME_GENRES, RADIO_COVERS_EVERY,
    RADIO_COVERS_FIRST, RowScope, cover_station_key, row_stations, seconds_left,
};
use kima_mixes::stations::mood_station_list;

#[test]
fn constants_match_upstream() {
    assert_eq!(COVER_SAMPLE, 20);
    assert_eq!(COVER_CONCURRENCY, 4);
    assert_eq!((HOME_GENRES, HOME_DECADES), (6, 4));
    assert_eq!(RADIO_COVERS_FIRST.as_secs(), 60);
    assert_eq!(RADIO_COVERS_EVERY.as_secs(), 300);
}

#[test]
fn scope_default_home_else_known_or_refused() {
    assert_eq!(RowScope::parse(None), Some(RowScope::Home));
    assert_eq!(RowScope::parse(Some("home")), Some(RowScope::Home));
    assert_eq!(RowScope::parse(Some("all")), Some(RowScope::All));
    for bad in ["", "everything", "Home", "ALL"] {
        assert_eq!(RowScope::parse(Some(bad)), None, "{bad}");
    }
}

/// `localDay`'s seconds left: a day of 86,400 s, at least 60.
#[test]
fn seconds_left_of_the_local_day() {
    assert_eq!(seconds_left(0), 86_400);
    assert_eq!(seconds_left(7 * 3600), 17 * 3600);
    assert_eq!(seconds_left(86_399), 60);
    assert_eq!(seconds_left(90_000), 60);
}

#[test]
fn cover_key_is_type_and_value() {
    assert_eq!(cover_station_key("all", None), "all:");
    assert_eq!(
        cover_station_key("genre", Some("no such genre")),
        "genre:no such genre"
    );
}

fn genres(n: usize) -> Vec<(String, i64)> {
    (0..n).map(|i| (format!("g{i}"), 100 - i as i64)).collect()
}

fn decades(n: usize) -> Vec<(i32, i64)> {
    (0..n).map(|i| (1990 - 10 * i as i32, 15)).collect()
}

/// The tiles in their order: all, workout, discovery, favorites, the mood stations, genres, decades; home shows six
/// genres and four decades, the radio page all; Workout and the moods only when shown (D-266).
#[test]
fn rows_in_order_home_slices_moods_only_when_shown() {
    let home = row_stations(RowScope::Home, true, &genres(8), &decades(8));
    let kinds: Vec<&str> = home.iter().map(|s| s.kind).collect();
    assert_eq!(kinds[..4], ["all", "workout", "discovery", "favorites"]);
    let moods: Vec<_> = home.iter().filter(|s| s.kind == "mood").collect();
    let listed = mood_station_list();
    assert_eq!(moods.len(), listed.len());
    for (m, l) in moods.iter().zip(&listed) {
        assert_eq!(m.value.as_deref(), Some(l.value));
        assert_eq!(m.name, Some(l.name));
        assert_eq!(m.description.as_deref(), Some(l.description.as_str()));
        assert_eq!(m.count, None);
    }
    let genre: Vec<_> = home.iter().filter(|s| s.kind == "genre").collect();
    assert_eq!(genre.len(), 6);
    assert_eq!(
        (genre[0].value.as_deref(), genre[0].count),
        (Some("g0"), Some(100))
    );
    let decade: Vec<_> = home
        .iter()
        .filter(|s| s.kind == "decade")
        .map(|s| s.value.clone().unwrap())
        .collect();
    assert_eq!(decade, ["1990", "1980", "1970", "1960"]);
    let last_mood = kinds.iter().rposition(|k| *k == "mood").unwrap();
    assert!(kinds.iter().position(|k| *k == "genre").unwrap() > last_mood);
    assert!(
        kinds.iter().position(|k| *k == "decade").unwrap()
            > kinds.iter().rposition(|k| *k == "genre").unwrap()
    );
    for s in &home[..4] {
        assert_eq!((s.value.as_deref(), s.name, s.count), (None, None, None));
    }

    let all = row_stations(RowScope::All, false, &genres(8), &decades(8));
    let kinds: Vec<&str> = all.iter().map(|s| s.kind).collect();
    assert_eq!(kinds[..3], ["all", "discovery", "favorites"]);
    assert!(!kinds.contains(&"workout") && !kinds.contains(&"mood"));
    assert_eq!(all.iter().filter(|s| s.kind == "genre").count(), 8);
    assert_eq!(all.iter().filter(|s| s.kind == "decade").count(), 8);
}
