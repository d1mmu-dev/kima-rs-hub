//! `MoodDistribution`, `classifyMoods`, `summariseMoods` and the five-entry client mood table, ported from upstream
//! `moodSnapshot.integration.test.ts` (pure part) and `vibeMap.ts`. Synthetic data only: the real heads and their
//! reference outputs are private assets (owner decision Q3).
use kima_vibe::moods::{
    CLIENT_MOODS, MoodDistribution, MoodStats, MoodValues, NEUTRAL, RECIPE_LINE, SummaryTrack,
    UNANALYZED, classify, mood_output, summarise,
};

fn unit() -> [MoodStats; 5] {
    [MoodStats { mean: 0.0, sd: 1.0 }; 5]
}

/// Ten library rows: every mood ranges over 0..=9 except `party`, which is constant 5.
fn lib() -> Vec<MoodValues> {
    (0..10)
        .map(|i| {
            let v = f64::from(i);
            [Some(v), Some(v), Some(v), Some(v), Some(5.0)]
        })
        .collect()
}

#[test]
fn the_client_table_is_the_five_upstream_entries() {
    let got: Vec<_> = CLIENT_MOODS
        .iter()
        .map(|m| (m.id, m.field, m.learned, m.name))
        .collect();
    assert_eq!(
        got,
        vec![
            ("happy", "moodHappy", "happy", "Upbeat"),
            ("sad", "moodSad", "sad", "Melancholic"),
            ("chill", "moodRelaxed", "calm", "Calm"),
            ("aggressive", "moodAggressive", "aggressive", "Intense"),
            ("party", "moodParty", "party", "Energetic"),
        ]
    );
    assert_eq!(RECIPE_LINE, 1.25);
}

#[test]
fn rank_counts_ties_as_half() {
    let d = MoodDistribution::new(&lib(), unit());
    // 3 values below 3, 1 equal: (3 + 4) / 2 / 10
    assert_eq!(d.rank(0, 3.0), 0.35);
    assert_eq!(d.rank(0, -1.0), 0.0);
    assert_eq!(d.rank(0, 99.0), 1.0);
    // Between values: no ties.
    assert_eq!(d.rank(0, 3.5), 0.4);
    // Every library value equal to the probe: the middle.
    assert_eq!(d.rank(4, 5.0), 0.5);
}

#[test]
fn mood_at_1_25_sd_above_mean() {
    let d = MoodDistribution::new(&lib(), unit());
    let on = classify(
        &[Some(1.25), Some(0.0), Some(0.0), Some(0.0), Some(0.0)],
        &d,
    );
    assert_eq!(on.moods, vec!["moodHappy"]);
    let under = classify(
        &[Some(1.2499), Some(0.0), Some(0.0), Some(0.0), Some(0.0)],
        &d,
    );
    assert!(under.moods.is_empty());
    // The line is in standard deviations of the library, not a fixed score.
    let wide = [MoodStats { mean: 0.0, sd: 4.0 }; 5];
    let d = MoodDistribution::new(&lib(), wide);
    let v = classify(
        &[Some(1.25), Some(0.0), Some(0.0), Some(0.0), Some(0.0)],
        &d,
    );
    assert!(v.moods.is_empty());
    let v = classify(&[Some(5.0), Some(0.0), Some(0.0), Some(0.0), Some(0.0)], &d);
    assert_eq!(v.moods, vec!["moodHappy"]);
}

#[test]
fn dominant_is_highest_library_rank_among_moods_at_the_line() {
    let d = MoodDistribution::new(&lib(), unit());
    // happy 2 (on the line), sad 9 (higher rank), aggressive 3 (on the line, lower rank than sad).
    let v = classify(&[Some(2.0), Some(9.0), Some(0.0), Some(3.0), Some(0.0)], &d);
    assert_eq!(v.dominant, "moodSad");
    assert_eq!(v.moods, vec!["moodHappy", "moodSad", "moodAggressive"]);
    assert_eq!(v.score, d.rank(1, 9.0));
    // Every mood's rank is kept, in table order.
    assert_eq!(v.ranks.len(), 5);
    assert_eq!(v.ranks[0], ("moodHappy", d.rank(0, 2.0)));
}

#[test]
fn equal_rank_keeps_the_first_mood_in_table_order() {
    let d = MoodDistribution::new(&lib(), unit());
    let v = classify(&[Some(7.0), Some(7.0), Some(0.0), Some(0.0), Some(0.0)], &d);
    assert_eq!(v.dominant, "moodHappy");
}

#[test]
fn zero_sd_mood_ranks_no_one() {
    let stats = {
        let mut s = unit();
        s[4] = MoodStats { mean: 5.0, sd: 0.0 };
        s
    };
    let d = MoodDistribution::new(&lib(), stats);
    assert_eq!(d.z(4, 1000.0), f64::NEG_INFINITY);
    let v = classify(
        &[Some(0.0), Some(0.0), Some(0.0), Some(0.0), Some(1000.0)],
        &d,
    );
    assert_eq!(v.dominant, NEUTRAL);
    assert!(v.moods.is_empty());
}

#[test]
fn neutral_when_none_at_line() {
    let d = MoodDistribution::new(&lib(), unit());
    let v = classify(&[Some(0.0); 5], &d);
    assert_eq!((v.dominant, v.score), (NEUTRAL, 0.0));
    assert_eq!(v.ranks.len(), 5, "an analysed track keeps its ranks");
}

#[test]
fn classify_unanalysed_when_any_mood_missing() {
    let d = MoodDistribution::new(&lib(), unit());
    let v = classify(&[Some(9.0), Some(9.0), None, Some(9.0), Some(9.0)], &d);
    assert_eq!(v.dominant, UNANALYZED);
    assert_eq!(v.score, 0.0);
    assert!(v.ranks.is_empty() && v.moods.is_empty());
    // An empty library judges nothing either.
    let empty = MoodDistribution::new(&[], unit());
    assert_eq!(classify(&[Some(1.0); 5], &empty).dominant, UNANALYZED);
}

#[test]
fn the_distribution_leaves_incomplete_rows_out() {
    let mut rows = lib();
    rows.push([Some(100.0), None, Some(100.0), Some(100.0), Some(100.0)]);
    let d = MoodDistribution::new(&rows, unit());
    assert_eq!(d.size(), 10);
    assert_eq!(d.rank(0, 99.0), 1.0);
}

#[test]
fn mood_output_maps_a_missing_view_to_unanalyzed() {
    let o = mood_output(None);
    assert_eq!((o.dominant_mood, o.mood_score), (UNANALYZED, 0.0));
    assert!(o.moods.is_empty());
    let d = MoodDistribution::new(&lib(), unit());
    let v = classify(&[Some(9.0), Some(0.0), Some(0.0), Some(0.0), Some(0.0)], &d);
    let o = mood_output(Some(&v));
    assert_eq!(o.dominant_mood, "moodHappy");
    assert_eq!(o.mood_score, v.score);
    assert_eq!(o.moods.len(), 5);
}

fn t<'a>(dominant: &'a str, album: &'a str, artist: &'a str) -> SummaryTrack<'a> {
    SummaryTrack {
        dominant,
        album_id: album,
        artist_id: artist,
    }
}

#[test]
fn summarise_shares_order_and_neutral_last() {
    let s = summarise(&[
        t("moodSad", "a1", "r1"),
        t("moodHappy", "a1", "r1"),
        t(NEUTRAL, "a2", "r2"),
        t(NEUTRAL, "a2", "r2"),
        t(NEUTRAL, "a2", "r2"),
        t("moodSad", "a3", "r3"),
        t(UNANALYZED, "a3", "r3"),
        t("", "a3", "r3"),
        t("something-else", "a4", "r4"),
    ]);
    // 7 analysed: neutral x4 (incl. the unknown value), sad x2, happy x1. Biggest first, even if neutral.
    let got: Vec<_> = s
        .shares
        .iter()
        .map(|x| (x.key.as_str(), x.fraction))
        .collect();
    assert_eq!(
        got,
        vec![
            (NEUTRAL, 4.0 / 7.0),
            ("moodSad", 2.0 / 7.0),
            ("moodHappy", 1.0 / 7.0)
        ]
    );
    // Equal counts: legend order, neutral after every mood.
    let s = summarise(&[
        t(NEUTRAL, "a", "r"),
        t("moodParty", "a", "r"),
        t("moodSad", "a", "r"),
        t("moodHappy", "a", "r"),
    ]);
    let keys: Vec<_> = s.shares.iter().map(|x| x.key.as_str()).collect();
    assert_eq!(keys, vec!["moodHappy", "moodSad", "moodParty", NEUTRAL]);
}

#[test]
fn group_winner_neutral_is_omitted() {
    let s = summarise(&[
        t("moodSad", "a1", "r1"),
        t("moodSad", "a1", "r1"),
        t(NEUTRAL, "a1", "r1"),
        t(NEUTRAL, "a2", "r2"),
        t(NEUTRAL, "a2", "r2"),
        t("moodHappy", "a2", "r2"),
        // Tied album: legend position decides (happy before sad), neutral never wins a tie against a mood.
        t("moodSad", "a3", "r3"),
        t("moodHappy", "a3", "r3"),
        t(NEUTRAL, "a4", "r4"),
        t("moodAggressive", "a4", "r4"),
        // Unanalysed-only groups do not appear.
        t(UNANALYZED, "a5", "r5"),
    ]);
    assert_eq!(s.albums.get("a1").map(String::as_str), Some("moodSad"));
    assert!(!s.albums.contains_key("a2"), "neutral winner is omitted");
    assert_eq!(s.albums.get("a3").map(String::as_str), Some("moodHappy"));
    assert_eq!(
        s.albums.get("a4").map(String::as_str),
        Some("moodAggressive")
    );
    assert!(!s.albums.contains_key("a5"));
    assert!(!s.artists.contains_key("r5"));
    assert_eq!(s.artists.get("r1").map(String::as_str), Some("moodSad"));
}

#[test]
fn summarise_nothing_is_empty() {
    let s = summarise(&[]);
    assert!(s.shares.is_empty() && s.albums.is_empty() && s.artists.is_empty());
}

/// The shipped heads on the shipped reference vectors, to 1e-4. Needs the private assets `moodHeads.json` and
/// `moods-reference.json` (`{"vectors": [[512 f32]...], "scores": [{mood: logit}...]}`) in `KIMA_VIBE_ASSET_DIR`;
/// ignored until the owner has cleared the training-data licences (Q3). Panics when the directory is unset.
#[test]
#[ignore = "private assets: owner decision Q3"]
fn shipped_reference_scores_to_1e_4() {
    use kima_vibe::mood_heads::{MOODS, parse_mood_heads};
    use kima_vibe::moods::score;
    let dir = std::env::var("KIMA_VIBE_ASSET_DIR").expect("KIMA_VIBE_ASSET_DIR must be set");
    let dir = std::path::Path::new(&dir);
    let heads = parse_mood_heads(&std::fs::read(dir.join("moodHeads.json")).unwrap()).unwrap();
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("moods-reference.json")).unwrap()).unwrap();
    let vectors = reference["vectors"].as_array().unwrap();
    assert!(!vectors.is_empty());
    for (i, v) in vectors.iter().enumerate() {
        let e: Vec<f64> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        for (m, head) in MOODS.iter().zip(&heads.heads) {
            let want = reference["scores"][i][m].as_f64().unwrap();
            assert!((score(head, &e) - want).abs() < 1e-4, "{m} on vector {i}");
        }
    }
}

#[test]
fn score_is_the_folded_dot_product_over_the_norm() {
    use kima_vibe::mood_heads::MoodHead;
    use kima_vibe::moods::score;
    let head = MoodHead {
        weights: (0..512).map(|i| f64::from(i % 3) - 1.0).collect(),
        bias: 0.5,
    };
    let e: Vec<f64> = (0..512).map(|i| f64::from(i % 5) + 1.0).collect();
    let norm = e.iter().map(|x| x * x).sum::<f64>().sqrt();
    let dot: f64 = e.iter().zip(&head.weights).map(|(a, b)| a * b).sum();
    assert_eq!(score(&head, &e), dot / norm + 0.5);
}
