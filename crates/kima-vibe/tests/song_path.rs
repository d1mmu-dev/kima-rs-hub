//! Song path arithmetic: ports of the behaviour of upstream `services/songPath.ts` (v1.12.3).
mod common;
use common::Xorshift;
use kima_vibe::formula::{SELF_SCORE, slerp_weights};
use kima_vibe::song_path::*;

fn track(id: &str, artist: &str, s0: f64, s1: f64) -> ScoredTrack {
    ScoredTrack {
        id: id.into(),
        artist_id: artist.into(),
        s0,
        s1,
    }
}

fn ids(p: &[Pick]) -> Vec<&str> {
    p.iter().map(|p| p.id.as_str()).collect()
}

fn never(_: usize) -> usize {
    unreachable!("smooth mode draws no random number")
}

#[test]
fn waypoint_count_formula() {
    assert_eq!(waypoint_count(0.15, None), 8);
    assert_eq!(waypoint_count(0.5, None), 8); // round(7.5) = 8
    assert_eq!(waypoint_count(0.6, None), 9);
    assert_eq!(waypoint_count(1.0, None), 15);
    assert_eq!(waypoint_count(2.0, None), 30);
    assert_eq!(waypoint_count(3.4, None), 50); // 51 clamps to 50
    assert_eq!(waypoint_count(1.0, Some(12)), 12, "an explicit length wins");
    assert_eq!(
        waypoint_count(1.0, Some(0)),
        15,
        "length 0 is falsy upstream: the formula applies"
    );
    assert_eq!(
        waypoint_count(1.0, Some(100)),
        100,
        "the service does not clamp; the route does"
    );
}

#[test]
fn refuses_distance_below_0_15() {
    assert_eq!(check_distance(0.1499999), Err(PathError::TooSimilar));
    assert_eq!(check_distance(0.15), Ok(()));
    assert_eq!(check_distance(0.9), Ok(()));
    assert_eq!(
        PathError::TooSimilar.to_string(),
        "These tracks are too similar for a meaningful journey. Try \"Similar Tracks\" instead."
    );
    // distance = 1 - pair_score / SELF_SCORE
    let (cos_theta, distance) = cos_and_distance(0.5 * SELF_SCORE);
    assert_eq!((cos_theta, distance), (0.5, 0.5));
}

#[test]
fn smooth_picks_first_candidate() {
    // cos 0, one waypoint: weights are equal, so the best track is the one with the largest s0 + s1.
    let lib = vec![
        track("a", "x", 0.2, 0.2),
        track("b", "y", 0.9, 0.5),
        track("c", "z", 0.6, 0.6),
    ];
    let picks = walk(&lib, 0.0, 1, Mode::Smooth, never);
    assert_eq!(ids(&picks), ["b"]);
    let [w0, w1] = slerp_weights(0.0, 0.5);
    assert_eq!(picks[0].distance, 1.0 - (w0 * 0.9 + w1 * 0.5) / SELF_SCORE);
}

#[test]
fn discovery_picks_among_first_three_with_injected_rng() {
    let lib: Vec<ScoredTrack> = (0..6)
        .map(|i| {
            track(
                &format!("t{i}"),
                &format!("a{i}"),
                0.9 - 0.1 * i as f64,
                0.9 - 0.1 * i as f64,
            )
        })
        .collect();
    let mut asked = Vec::new();
    let picks = walk(&lib, 0.0, 1, Mode::Discovery, |bound| {
        asked.push(bound);
        2
    });
    assert_eq!(asked, [3], "draws below min(3, candidates)");
    assert_eq!(ids(&picks), ["t2"]);
    // Fewer than three candidates left: the bound shrinks.
    let small = &lib[..2];
    let mut asked = Vec::new();
    let picks = walk(small, 0.0, 1, Mode::Discovery, |bound| {
        asked.push(bound);
        1
    });
    assert_eq!(asked, [2]);
    assert_eq!(ids(&picks), ["t1"]);
}

/// Library: three tracks of artist A that lead at every waypoint, one of artist B at `v_b`.
fn diversity_library(v_b: f64) -> Vec<ScoredTrack> {
    vec![
        track("A1", "A", 0.9, 0.9),
        track("A2", "A", 0.8, 0.8),
        track("A3", "A", 0.7, 0.7),
        track("B1", "B", v_b, v_b),
    ]
}

#[test]
fn artist_diversity_switch_within_1_05_of_best() {
    // B1 at 0.695 is within 1.05 x the distance of A3: it replaces the third A pick. At 0.68 it is not.
    assert_eq!(
        ids(&walk(
            &diversity_library(0.695),
            0.0,
            3,
            Mode::Smooth,
            never
        )),
        ["A1", "A2", "B1"]
    );
    assert_eq!(
        ids(&walk(&diversity_library(0.68), 0.0, 3, Mode::Smooth, never)),
        ["A1", "A2", "A3"]
    );
}

#[test]
fn no_repeat_of_visited() {
    let lib: Vec<ScoredTrack> = (0..5)
        .map(|i| {
            track(
                &format!("t{i}"),
                &format!("a{i}"),
                0.9 - 0.1 * i as f64,
                0.9 - 0.1 * i as f64,
            )
        })
        .collect();
    let picks = walk(&lib, 0.0, 5, Mode::Smooth, never);
    let mut seen = ids(&picks);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5);
}

#[test]
fn ties_broken_by_id() {
    let lib = vec![
        track("m", "x", 0.5, 0.5),
        track("b", "y", 0.5, 0.5),
        track("z", "w", 0.5, 0.5),
        track("a", "v", 0.5, 0.5),
    ];
    assert_eq!(
        ids(&walk(&lib, 0.0, 4, Mode::Smooth, never)),
        ["a", "b", "m", "z"]
    );
}

#[test]
fn stops_when_candidates_run_out() {
    let lib = vec![track("a", "x", 0.5, 0.5), track("b", "y", 0.4, 0.4)];
    let picks = walk(&lib, 0.3, 10, Mode::Smooth, never);
    assert_eq!(picks.len(), 2);
    assert!(walk(&[], 0.3, 10, Mode::Smooth, never).is_empty());
}

#[test]
fn walk_matches_a_naive_full_sort_on_random_libraries() {
    let mut rng = Xorshift(7);
    for case in 0..200 {
        let n = 5 + (rng.next_u64() % 60) as usize;
        let lib: Vec<ScoredTrack> = (0..n)
            .map(|i| {
                track(
                    &format!("t{:03}", n - i),
                    &format!("a{}", rng.next_u64() % 5),
                    (rng.unit() * 8.0).round() / 8.0,
                    (rng.unit() * 8.0).round() / 8.0,
                )
            })
            .collect();
        let cos_theta = rng.unit() * 1.7 - 0.7;
        let waypoints = 1 + (rng.next_u64() % 12) as usize;
        let got = walk(&lib, cos_theta, waypoints, Mode::Smooth, never);
        // naive reference: sort everything per waypoint
        let mut visited = std::collections::HashSet::new();
        let mut expected: Vec<Pick> = Vec::new();
        for i in 1..=waypoints {
            let [w0, w1] = slerp_weights(cos_theta, i as f64 / (waypoints + 1) as f64);
            let mut c: Vec<Pick> = lib
                .iter()
                .filter(|t| !visited.contains(&t.id))
                .map(|t| Pick {
                    id: t.id.clone(),
                    artist_id: t.artist_id.clone(),
                    distance: 1.0 - (w0 * t.s0 + w1 * t.s1) / SELF_SCORE,
                })
                .collect();
            c.sort_by(|a, b| {
                a.distance
                    .partial_cmp(&b.distance)
                    .unwrap()
                    .then_with(|| a.id.cmp(&b.id))
            });
            c.truncate(8);
            if c.is_empty() {
                break;
            }
            let mut sel = c[0].clone();
            if expected.len() >= 2 {
                let (p1, p2) = (&expected[expected.len() - 1], &expected[expected.len() - 2]);
                if p1.artist_id == p2.artist_id && sel.artist_id == p1.artist_id {
                    let threshold = c[0].distance * 1.05;
                    if let Some(alt) = c
                        .iter()
                        .find(|x| x.artist_id != p1.artist_id && x.distance <= threshold)
                    {
                        sel = alt.clone();
                    }
                }
            }
            visited.insert(sel.id.clone());
            expected.push(sel);
        }
        assert_eq!(got, expected, "case {case}");
    }
}
