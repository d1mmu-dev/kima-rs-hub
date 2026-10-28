//! Beets' `StringDistanceTest` and the kima-rs distance cases (spec §19.3).

use kima_db::autotag::LocalTrack;
use kima_ident::autotag::distance::*;
use kima_ident::musicbrainz::Track;
use uuid::Uuid;

fn d(a: &str, b: &str) -> f64 {
    string_dist(a, b)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn equal_strings_zero() {
    assert_eq!(d("Some String", "Some String"), 0.0);
}

#[test]
fn different_strings_nonzero() {
    assert!(d("Some String", "Totally Different") != 0.0);
}

#[test]
fn punctuation_ignored() {
    assert_eq!(d("Some String", "Some.String!"), 0.0);
}

#[test]
fn case_ignored() {
    assert_eq!(d("Some String", "sOME sTring"), 0.0);
}

#[test]
fn leading_the_has_lower_weight() {
    assert!(d("The Band Name", "Band Name") < d("XXX Band Name", "Band Name"));
}

#[test]
fn parens_have_lower_weight() {
    assert!(d("One (Two)", "One") < d("One .Two.", "One"));
}

#[test]
fn brackets_have_lower_weight() {
    assert!(d("One [Two]", "One") < d("One .Two.", "One"));
}

#[test]
fn ep_label_has_zero_weight() {
    assert_eq!(d("My Song (EP)", "My Song"), 0.0);
}

#[test]
fn featured_has_lower_weight() {
    assert!(d("My Song feat Someone", "My Song") < d("My Song blah Someone", "My Song"));
}

#[test]
fn postfix_the() {
    assert_eq!(d("The Song Title", "Song Title, The"), 0.0);
}

#[test]
fn empty_strings_zero() {
    assert_eq!(d("", ""), 0.0);
}

#[test]
fn solo_patterns_do_not_panic() {
    d("The", "");
    d("(EP)", "(EP)");
    d(", An", "");
}

#[test]
fn heuristic_does_not_harm_distance() {
    assert_eq!(d("Untitled", "[Untitled]"), 0.0);
}

#[test]
fn ampersand_expansion() {
    assert_eq!(d("And", "&"), 0.0);
}

#[test]
fn accented_characters() {
    assert_eq!(d("éáñ", "ean"), 0.0);
}

#[test]
fn string_dist_keeps_non_latin() {
    assert_eq!(d("東京事変", "東京事変"), 0.0);
    assert_eq!(d("東京事変", "椎名林檎"), 1.0);
}

#[test]
fn year_penalty_table() {
    assert_eq!(year_penalty(1999, 1999, None, 2026), 0.0);
    assert_eq!(year_penalty(1990, 2005, Some(1990), 2026), 0.0);
    assert!(close(
        year_penalty(1999, 2005, Some(1990), 2026),
        6.0 / 36.0
    ));
    assert_eq!(year_penalty(1999, 2005, None, 2026), 1.0);
}

#[test]
fn length_penalty_table() {
    assert_eq!(length_penalty(200, 209_000), 0.0);
    assert!(close(length_penalty(200, 215_000), 5.0 / 30.0));
    assert_eq!(length_penalty(200, 250_000), 1.0);
}

fn release_track(position: u32) -> Track {
    serde_json::from_value(serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "position": position,
        "title": "Song",
        "recording": {"id": "22222222-2222-2222-2222-222222222222", "title": "Song"},
    }))
    .unwrap()
}

fn local(track_no: i32) -> LocalTrack {
    LocalTrack {
        id: Uuid::nil(),
        path: String::new(),
        disc: 1,
        track_no,
        title: "Song".into(),
        artist_name: String::new(),
        duration_secs: 0,
        recording_mbid: None,
        recording_source: None,
        tag_release_mbid: None,
        tag_artist_mbids: vec![],
        disc_total: None,
        track_total: None,
    }
}

#[test]
fn track_index_by_medium_or_absolute() {
    // Only title and index are added, so the distance is the index share: 1 / (3 + 1).
    let t = release_track(4);
    let on_medium = ReleaseTrackRef {
        track: &t,
        medium_position: 1,
        absolute_index: 4,
    };
    assert_eq!(track_distance(&local(4), &on_medium, "", false), 0.0);
    let t2 = release_track(1);
    let disc2 = ReleaseTrackRef {
        track: &t2,
        medium_position: 2,
        absolute_index: 4,
    };
    assert_eq!(track_distance(&local(4), &disc2, "", false), 0.0);
    let neither = ReleaseTrackRef {
        track: &t2,
        medium_position: 2,
        absolute_index: 9,
    };
    assert!(close(track_distance(&local(4), &neither, "", false), 0.25));
}

#[test]
fn distance_normalises_added_keys_only() {
    let mut dist = Distance::default();
    assert_eq!(dist.value(), 0.0);
    dist.add("a", 3.0, 1.0);
    dist.add("b", 1.0, 0.0);
    assert!(close(dist.value(), 0.75));
}

#[test]
fn shares_sum_to_value() {
    let mut dist = Distance::default();
    dist.add("a", 3.0, 1.0);
    dist.add("b", 1.0, 0.5);
    dist.add("a", 2.0, 0.25);
    let shares = dist.shares();
    assert!(close(shares["a"], (3.0 + 0.5) / 6.0));
    assert!(close(shares.values().sum::<f64>(), dist.value()));
}
