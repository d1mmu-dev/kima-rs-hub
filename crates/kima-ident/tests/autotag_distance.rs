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

// --- mapping, album distance, ordering, recommendation (Task 6) -------------------------------

mod album {
    use super::*;
    use kima_db::autotag::{LocalAlbum, Recommendation};
    use kima_ident::autotag::score::{Scored, order, recommend, score};
    use kima_ident::musicbrainz::{Release, ReleaseGroup};
    use std::path::PathBuf;

    pub const THIS_YEAR: i32 = 2026;

    pub fn fixture_release(id: &str) -> Release {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/musicbrainz/autotag.json");
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let e = entries
            .iter()
            .find(|e| {
                e["url"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("/release/{id}?"))
            })
            .unwrap();
        serde_json::from_value(e["body"].clone()).unwrap()
    }

    pub fn group(r: &Release) -> ReleaseGroup {
        r.release_group.clone().unwrap()
    }

    pub fn local_track(disc: i32, no: i32, title: &str, artist: &str) -> LocalTrack {
        LocalTrack {
            id: Uuid::from_u128(u128::from(disc.unsigned_abs()) * 1000 + no.unsigned_abs() as u128),
            disc,
            track_no: no,
            title: title.into(),
            artist_name: artist.into(),
            duration_secs: 2,
            ..super::local(no)
        }
    }

    pub fn local_album(title: &str, artist: &str, tracks: Vec<LocalTrack>) -> LocalAlbum {
        LocalAlbum {
            id: Uuid::nil(),
            title: title.into(),
            artist_id: Uuid::nil(),
            artist_name: artist.into(),
            year: None,
            is_compilation: false,
            release_mbid: None,
            release_mbid_source: None,
            release_group_mbid: None,
            release_group_mbid_source: None,
            identity_source: "tag".into(),
            tracks,
        }
    }

    /// The local twin of the 10-track fixture release.
    pub fn twin(n: usize) -> LocalAlbum {
        let tracks = (1..=n)
            .map(|i| local_track(1, i as i32, &format!("Fixture Song {i}"), "Fixture Band"))
            .collect();
        local_album("Fixture Album", "Fixture Band", tracks)
    }

    pub const R1: &str = "c0000001-0000-4000-8000-000000000001";
    pub const R2: &str = "c0000002-0000-4000-8000-000000000001";
    pub const R3: &str = "c0000003-0000-4000-8000-000000000001";

    pub fn run(l: &LocalAlbum, r: &Release) -> Scored {
        score(l, r, &group(r), THIS_YEAR)
    }

    #[test]
    fn perfect_match_strong() {
        let r = fixture_release(R1);
        let s = run(&twin(10), &r);
        assert_eq!(s.distance, 0.0);
        assert_eq!(s.recommendation, Recommendation::Strong);
        assert_eq!(s.mapping.pairs.len(), 10);
        assert!(s.mapping.extra.is_empty() && s.mapping.missing.is_empty());
        assert!(close(s.penalties.values().sum::<f64>(), s.distance));
    }

    #[test]
    fn renamed_track_breaks_pair_and_caps_medium() {
        let r = fixture_release(R1);
        let mut l = twin(10);
        l.tracks[4].title = "Zzzz".into();
        let s = run(&l, &r);
        assert_eq!(s.mapping.pairs.len(), 9);
        assert_eq!(s.mapping.extra, vec![l.tracks[4].id]);
        assert_eq!(s.mapping.missing.len(), 1);
        assert_eq!(s.mapping.missing[0].title, "Fixture Song 5");
        assert_eq!(s.mapping.missing[0].position, 5);
        assert!(close(s.distance, 1.5 / 25.5), "{}", s.distance);
        assert_eq!(s.recommendation, Recommendation::Medium);
        assert!(close(s.penalties.values().sum::<f64>(), s.distance));
    }

    #[test]
    fn missing_track_caps_medium() {
        let r = fixture_release(R1);
        let s = run(&twin(9), &r);
        assert_eq!(s.mapping.pairs.len(), 9);
        assert_eq!(s.mapping.missing.len(), 1);
        assert!(s.mapping.extra.is_empty());
        assert!(s.distance < 0.04, "{}", s.distance);
        assert_eq!(s.recommendation, Recommendation::Medium);
    }

    #[test]
    fn extra_track_caps_medium() {
        let r = fixture_release(R1);
        let mut l = twin(10);
        l.tracks
            .push(local_track(1, 11, "Bonus Track", "Fixture Band"));
        let s = run(&l, &r);
        assert_eq!(s.mapping.extra.len(), 1);
        assert_eq!(s.mapping.pairs.len(), 10);
        assert_eq!(s.recommendation, Recommendation::Medium);
    }

    #[test]
    fn wrong_year_penalty() {
        let r = fixture_release(R1);
        let mut l = twin(10);
        l.year = Some(1990);
        let s = run(&l, &r);
        // Release and original year are 2001: |1990 - 2001| / (2026 - 2001) = 11/25. Σw = 27.
        assert!(
            close(s.penalties["year"], (11.0 / 25.0) / 27.0),
            "{:?}",
            s.penalties
        );
        // Without an original year the penalty is 1: share 1/Σw.
        let mut g = group(&r);
        g.first_release_date = None;
        let s = score(&l, &r, &g, THIS_YEAR);
        assert!(close(s.penalties["year"], 1.0 / 27.0));
        // The right year adds the key with penalty 0.
        l.year = Some(2001);
        assert_eq!(run(&l, &r).distance, 0.0);
    }

    #[test]
    fn unknown_local_artist_skips_the_artist_key() {
        let r = fixture_release(R1);
        let mut l = twin(10);
        l.artist_name = "Unknown Artist".into();
        let s = run(&l, &r);
        assert!(!s.penalties.contains_key("artist"));
    }

    #[test]
    fn album_id_term() {
        let r = fixture_release(R1);
        let mut l = twin(10);
        l.release_mbid = Some(r.id.0);
        assert_eq!(run(&l, &r).distance, 0.0);
        l.release_mbid = Some(Uuid::from_u128(5));
        // 5 / (3 + 3 + 5 + 20)
        assert!(close(run(&l, &r).penalties["album_id"], 5.0 / 31.0));
    }

    fn va_local() -> LocalAlbum {
        let artists = ["Alpha Act", "Beta Trio", "Alpha Act"];
        let tracks = (1..=3)
            .map(|i| local_track(1, i, &format!("Hits Song {i}"), artists[i as usize - 1]))
            .collect();
        local_album("Fixture Hits", "Various Artists", tracks)
    }

    #[test]
    fn va_compilation_artist_zero() {
        let r = fixture_release(R3);
        let s = run(&va_local(), &r);
        assert_eq!(s.penalties["artist"], 0.0);
        assert_eq!(s.mapping.pairs.len(), 3);
        assert_eq!(s.distance, 0.0);
        // track_artist is added per pair: each track distance is title 3 + artist 2 + index 1 +
        // length 2 = 8 weights; a wrong artist on one track shows in its pair.
        let mut l = va_local();
        l.tracks[0].artist_name = "Somebody Else".into();
        let s = run(&l, &r);
        assert!(s.mapping.pairs[0].distance > 0.0);
        assert!(close(
            s.mapping.pairs[0].distance,
            2.0 * 1.0 / 8.0 * artist_dist()
        ));
    }

    fn artist_dist() -> f64 {
        string_dist("Somebody Else", "Alpha Act")
    }

    #[test]
    fn va_release_with_empty_local_artist_skips_track_artist() {
        let r = fixture_release(R3);
        let mut l = va_local();
        for t in &mut l.tracks {
            t.artist_name.clear();
        }
        let s = run(&l, &r);
        assert_eq!(s.distance, 0.0, "{:?}", s.penalties);
    }

    #[test]
    fn two_disc_mapping() {
        let r = fixture_release(R2);
        let mut tracks = Vec::new();
        for disc in 1..=2 {
            for no in 1..=3 {
                tracks.push(local_track(
                    disc,
                    no,
                    &format!("Double Song {disc}-{no}"),
                    "Fixture Band",
                ));
            }
        }
        let l = local_album("Double Fixture", "Fixture Band", tracks);
        let s = run(&l, &r);
        assert_eq!(s.mapping.pairs.len(), 6);
        for (pair, local) in s.mapping.pairs.iter().zip(&l.tracks) {
            assert_eq!(i32::try_from(pair.medium).unwrap(), local.disc);
            assert_eq!(i32::try_from(pair.position).unwrap(), local.track_no);
        }
        assert_eq!(s.penalties["mediums"], 0.0);
        assert_eq!(s.distance, 0.0);
        // One disc locally against two media: penalty 1, with disc_total read as well.
        let mut one = l.clone();
        one.tracks.truncate(3);
        one.tracks[0].disc_total = Some(1);
        assert!(run(&one, &r).penalties["mediums"] > 0.0);
    }

    fn rel_with(id: u128, status: &str, format: &str, country: &str, date: &str) -> Release {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(id).to_string(),
            "title": "T", "status": status, "country": country, "date": date,
            "media": [{"position": 1, "format": format, "track-count": 0, "tracks": []}],
        }))
        .unwrap()
    }

    fn scored(distance: f64) -> Scored {
        Scored {
            distance,
            recommendation: recommend(distance, false),
            penalties: Default::default(),
            mapping: Default::default(),
        }
    }

    fn ids(c: &[(Release, Scored)]) -> Vec<u128> {
        c.iter().map(|(r, _)| r.id.0.as_u128()).collect()
    }

    #[test]
    fn ordering_tie_breakers() {
        let mut c = vec![
            (rel_with(9, "Official", "CD", "GB", "2000"), scored(0.1)),
            (
                rel_with(8, "Bootleg", "Digital Media", "XW", "1999"),
                scored(0.1),
            ),
            (
                rel_with(7, "Official", "12\" Vinyl", "XW", "2000"),
                scored(0.1),
            ),
            (
                rel_with(6, "Official", "Digital Media", "GB", "2000"),
                scored(0.1),
            ),
            (rel_with(5, "Official", "CD", "XW", "2000"), scored(0.1)),
            (rel_with(4, "Official", "CD", "XW", "1999-05"), scored(0.1)),
            (rel_with(3, "Official", "CD", "XW", "1999-05"), scored(0.1)),
            (
                rel_with(2, "Official", "Enhanced CD", "XW", "1990"),
                scored(0.1),
            ),
        ];
        order(&mut c);
        // Official first; Digital Media; CD (XW before GB; earlier date; smaller MBID);
        // "… CD"; Vinyl; the bootleg last.
        assert_eq!(ids(&c), vec![6, 3, 4, 5, 9, 2, 7, 8]);
    }

    #[test]
    fn ordering_rounds_distance_to_three_decimals() {
        let mut c = vec![
            (rel_with(2, "Official", "CD", "XW", "2000"), scored(0.1004)),
            (rel_with(1, "Official", "CD", "XW", "2001"), scored(0.1001)),
            (rel_with(3, "Official", "CD", "XW", "2000"), scored(0.0994)),
        ];
        order(&mut c);
        // 0.0994 rounds to 0.099 (first); 0.1004 and 0.1001 tie at 0.100, then by date, then id.
        assert_eq!(ids(&c), vec![3, 2, 1]);
    }

    #[test]
    fn worst_medium_ranks_the_release() {
        let mut mixed = rel_with(1, "Official", "Digital Media", "XW", "2000");
        let vinyl = rel_with(2, "Official", "Vinyl", "XW", "2000");
        mixed.media.push(vinyl.media[0].clone());
        let cd = rel_with(3, "Official", "CD", "XW", "2000");
        let mut c = vec![(mixed, scored(0.1)), (cd, scored(0.1))];
        order(&mut c);
        assert_eq!(ids(&c), vec![3, 1]);
    }

    #[test]
    fn recommendation_thresholds() {
        assert_eq!(recommend(0.0, false), Recommendation::Strong);
        assert_eq!(recommend(0.039, false), Recommendation::Strong);
        assert_eq!(recommend(0.04, false), Recommendation::Medium);
        assert_eq!(recommend(0.039, true), Recommendation::Medium);
        assert_eq!(recommend(0.199, true), Recommendation::Medium);
        assert_eq!(recommend(0.2, false), Recommendation::Weak);
    }

    #[test]
    fn hungarian_prefers_global_minimum() {
        // Greedy from the first local track would give it R1 (1/8 against 2/8) and leave the
        // second with R2; the optimum swaps them.
        let release: Release = serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(1).to_string(), "title": "T",
            "media": [{"position": 1, "track-count": 2, "tracks": [
                {"id": Uuid::from_u128(11).to_string(), "position": 1, "title": "abcdefgh",
                 "recording": {"id": Uuid::from_u128(21).to_string()}},
                {"id": Uuid::from_u128(12).to_string(), "position": 2, "title": "abcdefyz",
                 "recording": {"id": Uuid::from_u128(22).to_string()}},
            ]}],
        }))
        .unwrap();
        let mut a = local_track(1, 0, "abcdefgx", "");
        a.id = Uuid::from_u128(101);
        a.duration_secs = 0;
        let mut b = local_track(1, 0, "abcdefgh", "");
        b.id = Uuid::from_u128(102);
        b.duration_secs = 0;
        let l = local_album("T", "", vec![a, b]);
        let m = kima_ident::autotag::score::map_tracks(&l, &release);
        assert_eq!(m.pairs.len(), 2);
        let by_local = |id: u128| {
            m.pairs
                .iter()
                .find(|p| p.track_id.as_u128() == id)
                .unwrap()
                .release_track_mbid
                .0
                .as_u128()
        };
        assert_eq!(by_local(101), 12);
        assert_eq!(by_local(102), 11);
    }

    #[test]
    fn empty_sides_do_not_panic() {
        let r = fixture_release(R1);
        let none = local_album("Fixture Album", "Fixture Band", vec![]);
        let s = run(&none, &r);
        assert_eq!(s.mapping.missing.len(), 10);
        assert_ne!(s.recommendation, Recommendation::Strong);
    }
}
