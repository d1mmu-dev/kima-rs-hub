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

// Release lengths here are whole seconds in ms, clear of the ms_to_secs rounding edge (.5 s).
#[test]
fn length_penalty_boundaries() {
    assert_eq!(length_penalty(200, 210_000), 0.0); // 10 s
    assert!(close(length_penalty(200, 211_000), 1.0 / 30.0)); // 11 s
    assert_eq!(length_penalty(200, 240_000), 1.0); // 40 s
    assert_eq!(length_penalty(200, 241_000), 1.0); // 41 s
    assert_eq!(length_penalty(200, 160_000), 1.0); // shorter by 40 s
}

#[test]
fn non_letters_and_numbers_are_dropped_per_spec() {
    // Devanagari vowel sign I is a spacing mark (Mc): not \p{L} or \p{N}, so it is dropped.
    assert_eq!(d("कि", "क"), 0.0);
    // Symbols are dropped; digits are kept.
    assert_eq!(d("a\u{2665}", "a"), 0.0);
    assert!(d("a1", "a") > 0.0);
}

#[test]
fn voiced_kana_marks_are_kept() {
    assert!(d("ば", "は") > 0.0);
    assert!(d("ぱ", "は") > 0.0);
    assert_eq!(d("ば", "は\u{3099}"), 0.0);
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
fn track_index_by_position_or_absolute() {
    // Only title and index are added, so the distance is the index share: 1 / (3 + 1).
    let t = release_track(4);
    let on_medium = ReleaseTrackRef {
        track: &t,
        absolute_index: 4,
    };
    assert_eq!(track_distance(&local(4), &on_medium, "", false), 0.0);
    let t2 = release_track(1);
    let disc2 = ReleaseTrackRef {
        track: &t2,
        absolute_index: 4,
    };
    assert_eq!(track_distance(&local(4), &disc2, "", false), 0.0);
    let neither = ReleaseTrackRef {
        track: &t2,
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

// --- track_distance terms -------------------------------------------------------------------

const RECORDING: Uuid = Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222);

fn credit(name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name, "joinphrase": "",
        "artist": {"id": "33333333-3333-3333-3333-333333333333", "name": name, "sort-name": name},
    })
}

fn track_with(extra: serde_json::Value, recording_extra: serde_json::Value) -> Track {
    let mut recording = serde_json::json!({"id": RECORDING.to_string(), "title": "Song"});
    recording
        .as_object_mut()
        .unwrap()
        .extend(recording_extra.as_object().unwrap().clone());
    let mut t = serde_json::json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "position": 1, "title": "Song", "recording": recording,
    });
    t.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(t).unwrap()
}

fn plain() -> serde_json::Value {
    serde_json::json!({})
}

fn dist(local: &LocalTrack, t: &Track, release_credit: &str, va: bool) -> f64 {
    let rt = ReleaseTrackRef {
        track: t,
        absolute_index: 1,
    };
    track_distance(local, &rt, release_credit, va)
}

/// A local track whose title is 0.25 away ("Sang" / "Song"), no number, length or id.
fn off_title() -> LocalTrack {
    LocalTrack {
        title: "Sang".into(),
        artist_name: "A".into(),
        ..local(0)
    }
}

#[test]
fn track_id_term() {
    let t = track_with(plain(), plain());
    let mut l = off_title();
    // Absent id: only the title term (weight 3).
    assert!(close(dist(&l, &t, "", false), 0.25));
    l.recording_mbid = Some(RECORDING);
    assert!(close(dist(&l, &t, "", false), 3.0 * 0.25 / 8.0));
    l.recording_mbid = Some(Uuid::from_u128(1));
    assert!(close(dist(&l, &t, "", false), (3.0 * 0.25 + 5.0) / 8.0));
}

#[test]
fn track_artist_term() {
    let same = track_with(serde_json::json!({"artist-credit": [credit("A")]}), plain());
    let other = track_with(serde_json::json!({"artist-credit": [credit("B")]}), plain());
    let l = off_title();
    // Equal credit, not VA: term absent.
    assert!(close(dist(&l, &same, "A", false), 0.25));
    // VA release: added even when equal (penalty 0), diluting the title term.
    assert!(close(dist(&l, &same, "A", true), 3.0 * 0.25 / 5.0));
    // Differing credit: added with penalty 1 ("A" vs "B").
    assert!(close(
        dist(&l, &other, "A", false),
        (3.0 * 0.25 + 2.0) / 5.0
    ));
}

#[test]
fn recording_credit_is_the_fallback() {
    let t = track_with(plain(), serde_json::json!({"artist-credit": [credit("A")]}));
    let l = off_title();
    assert!(close(dist(&l, &t, "A", false), 0.25)); // same as release: absent
    assert!(close(dist(&l, &t, "Other", false), 3.0 * 0.25 / 5.0)); // differs, matches local: 0
}

#[test]
fn track_length_term() {
    let mut l = off_title();
    l.duration_secs = 200;
    let expect = (3.0 * 0.25 + 2.0 / 6.0) / 5.0; // 15 s apart: penalty 5/30
    let own = track_with(serde_json::json!({"length": 215_000}), plain());
    assert!(close(dist(&l, &own, "", false), expect));
    // The recording's length is the fallback, also when the track's length is zero.
    let rec = track_with(plain(), serde_json::json!({"length": 215_000}));
    assert!(close(dist(&l, &rec, "", false), expect));
    let zero = track_with(
        serde_json::json!({"length": 0}),
        serde_json::json!({"length": 215_000}),
    );
    assert!(close(dist(&l, &zero, "", false), expect));
    // Zero everywhere is unknown: the term is absent.
    let none = track_with(
        serde_json::json!({"length": 0}),
        serde_json::json!({"length": 0}),
    );
    assert!(close(dist(&l, &none, "", false), 0.25));
    // An unknown local length is absent too.
    l.duration_secs = 0;
    assert!(close(dist(&l, &own, "", false), 0.25));
}
