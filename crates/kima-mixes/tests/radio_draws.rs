//! The pure parts of `GET /library/radio` (M5b Task 13; upstream `services/mixes/stationSelection.ts`,
//! `routes/library/paging.ts` `parseLimit` and `utils/shuffle.ts` at the pin `c7527577`): the limit reader, the
//! uniform and weighted draws, and the artist radio's sound score and 40/60 split. The draws' statistical cases of
//! `radioArtistRule.integration.test.ts` are ported here over fixed random streams.

use std::collections::{HashMap, HashSet};

use kima_mixes::radio::{
    Sound, artist_mix, artist_sound, closeness, draw_weighted, js_parse_int, parse_limit, shuffle,
};
use kima_mixes::spread::random_from_seed;

#[test]
fn parse_limit_reads_like_upstream() {
    // `Math.max(1, Math.min(parseInt(raw, 10) || 50, 100))`.
    for (raw, want) in [
        (None, 50),
        (Some(""), 50),
        (Some("abc"), 50),
        (Some("0"), 50),
        (Some("-5"), 1),
        (Some("20"), 20),
        (Some(" 20x"), 20),
        (Some("+7"), 7),
        (Some("500"), 100),
        (Some("99999999999999999999999"), 100),
        (Some("-99999999999999999999999"), 1),
        (Some("1e3"), 1),
    ] {
        assert_eq!(parse_limit(raw, 50, 100), want, "{raw:?}");
    }
    assert_eq!(js_parse_int("1995x"), Some(1995));
    assert_eq!(js_parse_int("x1995"), None);
    assert_eq!(js_parse_int("-0"), Some(0));
    // ECMAScript white space and line terminators lead the digits; U+0085 (NEL) is neither, though Rust calls it
    // white space, so `parseInt("\u0085" + "12")` is NaN.
    for lead in [
        "\u{a0}",
        "\u{2028}",
        "\u{feff}",
        "\u{3000}",
        "\t\n\u{b}\u{c}\r ",
    ] {
        assert_eq!(js_parse_int(&format!("{lead}12")), Some(12), "{lead:?}");
    }
    assert_eq!(js_parse_int("\u{85}12"), None);
    assert_eq!(js_parse_int(" \u{85}12"), None);
}

#[test]
fn shuffle_keeps_every_item_and_varies() {
    let items: Vec<u32> = (0..50).collect();
    let mut random = random_from_seed("a");
    let first = shuffle(items.clone(), &mut random);
    let second = shuffle(items.clone(), &mut random);
    let mut sorted = first.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, items);
    assert_ne!(first, items);
    assert_ne!(first, second);
}

#[test]
fn draw_weighted_leans_toward_weight_and_skips_zero() {
    let items = ["heavy", "light", "zero", "nan"];
    let weight = |s: &&str| match *s {
        "heavy" => 2.0,
        "light" => 1.0,
        "zero" => 0.0,
        _ => f64::NAN,
    };
    let mut random = random_from_seed("w");
    let mut first = HashMap::new();
    for _ in 0..3000 {
        let got = draw_weighted(&items, weight, 4, &mut random);
        assert_eq!(got.len(), 2, "only positive, finite weights are drawn");
        *first.entry(got[0]).or_insert(0) += 1;
    }
    // Twice the weight comes first about twice as often: 2000 against 1000.
    let heavy = first["heavy"];
    assert!((1850..=2150).contains(&heavy), "{heavy}");
}

#[test]
fn artist_sound_averages_the_analysed_songs_with_absent_readings_as_zero() {
    let s = |bpm: Option<f64>, valence: Option<f64>| Sound {
        bpm,
        valence,
        ..Sound::default()
    };
    assert_eq!(
        artist_sound(&[s(None, None), s(Some(0.0), Some(0.0))]),
        None
    );
    let avg = artist_sound(&[s(Some(100.0), None), s(None, Some(0.5)), s(None, None)]).unwrap();
    assert_eq!(avg.bpm, Some(50.0));
    assert_eq!(avg.valence, Some(0.25));
    assert_eq!(avg.energy, Some(0.0));
    assert_eq!(avg.danceability, Some(0.0));
}

#[test]
fn closeness_matches_upstream_terms() {
    let artist = Sound {
        bpm: Some(120.0),
        energy: Some(0.0),
        valence: Some(0.5),
        danceability: Some(0.0),
    };
    // Nothing to compare: 0.5.
    assert_eq!(closeness(&Sound::default(), &artist), 0.5);
    // 20 bpm away: 0.5; valence 0.25 away: 0.75; energy and danceability absent (D-263) and the artist's averages 0.
    let song = Sound {
        bpm: Some(140.0),
        valence: Some(0.25),
        ..Sound::default()
    };
    assert!((closeness(&song, &artist) - 0.625).abs() < 1e-12);
    // 80 bpm away and nothing else: 0.
    let far = Sound {
        bpm: Some(200.0),
        ..Sound::default()
    };
    assert_eq!(closeness(&far, &artist), 0.0);
    // Upstream compares energy whenever the song has one (`!== null`) and the artist's is truthy.
    let artist_energy = Sound {
        energy: Some(1.0),
        ..artist
    };
    let quiet = Sound {
        bpm: Some(120.0),
        energy: Some(0.0),
        ..Sound::default()
    };
    assert_eq!(closeness(&quiet, &artist_energy), 0.5);
}

#[test]
fn artist_mix_takes_40_percent_own_then_similar() {
    let own: Vec<String> = (0..30).map(|i| format!("own{i}")).collect();
    let similar: Vec<(String, Option<f64>)> =
        (0..30).map(|i| (format!("sim{i}"), Some(0.5))).collect();
    let mut random = random_from_seed("split");
    let got = artist_mix(&own, &similar, 20, &mut random);
    assert_eq!(got.len(), 20);
    assert!(got[..8].iter().all(|s| s.starts_with("own")));
    assert!(got[8..].iter().all(|s| s.starts_with("sim")));
    // Few own songs: the similar ones fill the rest of the limit.
    let got = artist_mix(&own[..3], &similar, 20, &mut random);
    assert_eq!(got.len(), 20);
    assert_eq!(got.iter().filter(|s| s.starts_with("own")).count(), 3);
    // A limit of 1: one own song.
    assert_eq!(artist_mix(&own, &similar, 1, &mut random).len(), 1);
}

/// `radioArtistRule`: "artist radio: similar tracks without a closeness score — draws them uniformly: every one comes
/// back".
#[test]
fn unscored_similar_songs_are_drawn_uniformly() {
    let own = vec!["a1".to_string(), "a2".to_string()];
    let similar: Vec<(String, Option<f64>)> = (1..=12).map(|i| (format!("s{i}"), None)).collect();
    let mut random = random_from_seed("uniform");
    let mut seen = HashSet::new();
    for _ in 0..60 {
        let got = artist_mix(&own, &similar, 8, &mut random);
        assert_eq!(got.len(), 8);
        seen.extend(got);
    }
    for (s, _) in &similar {
        assert!(seen.contains(s), "{s} never drawn");
    }
}

/// `radioArtistRule`: "a similar track scoring exactly 0 is still taken when the slots outnumber the weighted
/// tracks".
#[test]
fn a_zero_score_song_fills_when_slots_outnumber_weighted_songs() {
    let own = vec!["b1".to_string(), "b2".to_string()];
    let similar = vec![
        ("zero".to_string(), Some(0.0)),
        ("z2".to_string(), Some(1.0)),
        ("z3".to_string(), Some(0.9)),
    ];
    let mut random = random_from_seed("zero");
    for _ in 0..15 {
        let got = artist_mix(&own, &similar, 5, &mut random);
        assert_eq!(got.len(), 5);
        assert!(got.contains(&"zero".to_string()));
    }
}

/// `radio.integration`: "radio: artist weighting — leans toward similar tracks closer to the artist's sound, yet
/// reaches past the top slice".
#[test]
fn closer_similar_songs_come_more_often_yet_the_draw_reaches_past_the_top() {
    let own: Vec<String> = (0..20).map(|i| format!("own{i}")).collect();
    let similar: Vec<(String, Option<f64>)> = (0..200)
        .map(|i| (format!("sim{i}"), Some((i + 1) as f64 / 200.0)))
        .collect();
    let score: HashMap<&str, f64> = similar
        .iter()
        .map(|(s, w)| (s.as_str(), w.unwrap()))
        .collect();
    let mean_all = score.values().sum::<f64>() / score.len() as f64;
    let mut random = random_from_seed("lean");
    let mut returned = Vec::new();
    for _ in 0..40 {
        let got = artist_mix(&own, &similar, 20, &mut random);
        assert_eq!(got.len(), 20);
        returned.extend(got.into_iter().filter(|s| s.starts_with("sim")));
    }
    let mean = returned.iter().map(|s| score[s.as_str()]).sum::<f64>() / returned.len() as f64;
    assert!(mean > mean_all, "{mean} <= {mean_all}");
    let top: HashSet<String> = (176..200).map(|i| format!("sim{i}")).collect();
    let past: HashSet<&String> = returned.iter().filter(|s| !top.contains(*s)).collect();
    assert!(past.len() > 24, "{}", past.len());
}
