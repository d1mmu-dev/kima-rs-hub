//! The arithmetic of a song path: waypoints spaced evenly along the arc between two tracks, each filled by one of
//! the closest unvisited tracks, with an artist diversity rule. Port of upstream `services/songPath.ts` (v1.12.3);
//! the library scoring and the track lookup are the database's job (`kima_db::vibe::path`).

use crate::formula::{SELF_SCORE, slerp_weights};

/// Pairs closer than this (`1 - pair_score / SELF_SCORE`) have no meaningful journey between them.
pub const MIN_PATH_DISTANCE: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    TooSimilar,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::TooSimilar => f.write_str("These tracks are too similar for a meaningful journey. Try \"Similar Tracks\" instead."),
        }
    }
}

impl std::error::Error for PathError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Smooth,
    Discovery,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Smooth => "smooth",
            Mode::Discovery => "discovery",
        }
    }

    /// How many unvisited tracks compete for each waypoint.
    fn candidate_limit(self) -> usize {
        match self {
            Mode::Discovery => 10,
            Mode::Smooth => 8,
        }
    }
}

/// One library track scored against the start (`s0`) and the end (`s1`) of the path.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredTrack {
    pub id: String,
    pub artist_id: String,
    pub s0: f64,
    pub s1: f64,
}

/// A waypoint's chosen track and its distance `1 - (w0 s0 + w1 s1) / SELF_SCORE`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub id: String,
    pub artist_id: String,
    pub distance: f64,
}

/// `(cos_theta, distance)` of a start/end pair from its pair score.
pub fn cos_and_distance(pair_score: f64) -> (f64, f64) {
    let cos_theta = pair_score / SELF_SCORE;
    (cos_theta, 1.0 - cos_theta)
}

pub fn check_distance(distance: f64) -> Result<(), PathError> {
    if distance < MIN_PATH_DISTANCE {
        Err(PathError::TooSimilar)
    } else {
        Ok(())
    }
}

/// `length || max(8, min(50, round(distance * 15)))`: a non-zero explicit length wins.
pub fn waypoint_count(distance: f64, length: Option<usize>) -> usize {
    match length {
        Some(n) if n > 0 => n,
        _ => (distance * 15.0).round().clamp(8.0, 50.0) as usize,
    }
}

fn before(a: &Pick, b: &Pick) -> bool {
    a.distance < b.distance || (a.distance == b.distance && a.id < b.id)
}

/// The `size` unvisited tracks closest by `distance`, closest first (bounded insertion, ties by id).
fn nearest(
    tracks: &[ScoredTrack],
    visited: &[bool],
    size: usize,
    distance: impl Fn(&ScoredTrack) -> f64,
) -> Vec<Pick> {
    let mut out: Vec<Pick> = Vec::with_capacity(size);
    for (n, track) in tracks.iter().enumerate() {
        if visited[n] {
            continue;
        }
        let c = Pick {
            id: track.id.clone(),
            artist_id: track.artist_id.clone(),
            distance: distance(track),
        };
        if out.len() == size && !before(&c, &out[size - 1]) {
            continue;
        }
        let mut i = out.len().min(size - 1);
        if i == out.len() {
            out.push(c);
        } else {
            out[i] = c;
        }
        while i > 0 && before(&out[i], &out[i - 1]) {
            out.swap(i, i - 1);
            i -= 1;
        }
    }
    out
}

/// Walks `waypoints` steps along the arc. `pick_below(bound)` returns an index below `bound` (discovery mode
/// only), so tests inject the randomness. Stops early when the library runs out of unvisited tracks.
pub fn walk(
    scored: &[ScoredTrack],
    cos_theta: f64,
    waypoints: usize,
    mode: Mode,
    mut pick_below: impl FnMut(usize) -> usize,
) -> Vec<Pick> {
    let mut visited = vec![false; scored.len()];
    let index_of: std::collections::HashMap<&str, usize> = scored
        .iter()
        .enumerate()
        .map(|(n, t)| (t.id.as_str(), n))
        .collect();
    let mut picked: Vec<Pick> = Vec::new();
    for i in 1..=waypoints {
        let [w0, w1] = slerp_weights(cos_theta, i as f64 / (waypoints + 1) as f64);
        let candidates = nearest(scored, &visited, mode.candidate_limit(), |t| {
            1.0 - (w0 * t.s0 + w1 * t.s1) / SELF_SCORE
        });
        if candidates.is_empty() {
            break;
        }
        let pick_index = match mode {
            Mode::Discovery => pick_below(candidates.len().min(3)),
            Mode::Smooth => 0,
        };
        let mut selected = candidates[pick_index].clone();
        if picked.len() >= 2 {
            let prev1 = &picked[picked.len() - 1];
            let prev2 = &picked[picked.len() - 2];
            if prev1.artist_id == prev2.artist_id && selected.artist_id == prev1.artist_id {
                let threshold = candidates[0].distance * 1.05;
                if let Some(alt) = candidates
                    .iter()
                    .find(|c| c.artist_id != prev1.artist_id && c.distance <= threshold)
                {
                    selected = alt.clone();
                }
            }
        }
        if let Some(&n) = index_of.get(selected.id.as_str()) {
            visited[n] = true;
        }
        picked.push(selected);
    }
    picked
}
