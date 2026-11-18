//! The colour field (port of upstream `mapLayout.ts` `colourField`, `colourAt`): a track's colour is its area's place
//! along the hue circle of the (arousal, valence) plane, as a share of the library's range. Placed tracks are coloured
//! by the same rule on the stored frame.

// Index loops mirror the TypeScript line for line: the port is checked bit for bit against it.
#![allow(clippy::needless_range_loop)]

use serde::{Deserialize, Serialize};

use crate::exact_math::atan2;
use crate::pca::symmetric_eigen;

/// A track's colour is its area's: the mean over its nearest tracks on the map, the map gate's k'.
const AREA_NEIGHBOURS: usize = 50;

/// The rigid turn of the map onto the colour plane, kept so placed tracks are coloured on the same field. Stored as
/// JSON (`matching_state.map_frame`) with upstream's camelCase keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColourFrame {
    pub centre: [f64; 3],
    pub scale: f64,
    /// 3 x 2, row-major: map axes onto (arousal, valence).
    pub turn: [f64; 6],
    pub hue_min: f64,
    pub hue_max: f64,
}

/// Indices of each query's `k` nearest positions (Euclidean), nearest first, itself included when it is a member.
/// A bounded heap, worst on top, ties broken towards the lower index.
fn nearest_one(pos: &[f64], count: usize, query: &[f64], k: usize, out: &mut [u32]) {
    let mut heap: Vec<(f64, u32)> = Vec::with_capacity(k);
    // `a` is worse than `b`: farther, or equally far with a higher index.
    let worse = |a: (f64, u32), b: (f64, u32)| a.0 > b.0 || (a.0 == b.0 && a.1 > b.1);
    for j in 0..count {
        let mut s = 0.0;
        for d in 0..3 {
            let t = pos[j * 3 + d] - query[d];
            s += t * t;
        }
        let item = (s, j as u32);
        if heap.len() < k {
            heap.push(item);
            let mut i = heap.len() - 1;
            while i > 0 {
                let p = (i - 1) >> 1;
                if !worse(heap[i], heap[p]) {
                    break;
                }
                heap.swap(i, p);
                i = p;
            }
        } else if s < heap[0].0 || (s == heap[0].0 && item.1 < heap[0].1) {
            heap[0] = item;
            let size = heap.len();
            let mut i = 0;
            loop {
                let l = 2 * i + 1;
                let r = l + 1;
                let mut m = i;
                if l < size && worse(heap[l], heap[m]) {
                    m = l;
                }
                if r < size && worse(heap[r], heap[m]) {
                    m = r;
                }
                if m == i {
                    break;
                }
                heap.swap(i, m);
                i = m;
            }
        }
    }
    heap.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    for (r, (_, j)) in heap.iter().enumerate() {
        out[r] = *j;
    }
}

fn nearest_positions(
    pos: &[f64],
    count: usize,
    query: &[f64],
    queries: usize,
    k: usize,
) -> Vec<u32> {
    let mut out = vec![0_u32; queries * k];
    if k == 0 {
        return out;
    }
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        // Queries are independent, so the result is the serial one.
        out.par_chunks_mut(k)
            .enumerate()
            .for_each(|(q, row)| nearest_one(pos, count, &query[q * 3..q * 3 + 3], k, row));
    }
    #[cfg(not(feature = "parallel"))]
    for (q, row) in out.chunks_mut(k).enumerate() {
        nearest_one(pos, count, &query[q * 3..q * 3 + 3], k, row);
    }
    out
}

/// The hue angle, in degrees, of the colour-plane point (arousal, valence), cut at the calm pole (low arousal).
fn hue_from_calm(arousal: f64, valence: f64) -> f64 {
    ((atan2(valence, arousal) * 180.0) / std::f64::consts::PI - 180.0 + 720.0) % 360.0
}

fn project(frame: &ColourFrame, pos: &[f64], i: usize) -> [f64; 2] {
    let p: Vec<f64> = (0..3)
        .map(|d| (pos[i * 3 + d] - frame.centre[d]) / frame.scale)
        .collect();
    let t = &frame.turn;
    [
        p[0] * t[0] + p[1] * t[2] + p[2] * t[4],
        p[0] * t[1] + p[1] * t[3] + p[2] * t[5],
    ]
}

fn area_hues(
    frame: &ColourFrame,
    pos: &[f64],
    count: usize,
    query: &[f64],
    queries: usize,
) -> Vec<f64> {
    let k = AREA_NEIGHBOURS.min(count);
    let near = nearest_positions(pos, count, query, queries, k);
    let mut plane = vec![0.0; count * 2];
    for j in 0..count {
        let [x, y] = project(frame, pos, j);
        plane[j * 2] = x;
        plane[j * 2 + 1] = y;
    }
    (0..queries)
        .map(|q| {
            let (mut x, mut y) = (0.0, 0.0);
            for r in 0..k {
                let j = near[q * k + r] as usize;
                x += plane[j * 2];
                y += plane[j * 2 + 1];
            }
            hue_from_calm(x / k as f64, y / k as f64)
        })
        .collect()
}

fn to_share(frame: &ColourFrame, hue: f64) -> f64 {
    let span = frame.hue_max - frame.hue_min;
    if span > 0.0 {
        ((hue - frame.hue_min) / span).clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// The colour field: the map centred, scaled to unit spread and turned rigidly onto the colour plane so its axes line
/// up with arousal and valence (orthogonal Procrustes, 3 x 2); each track takes the mean of its area's 50 nearest
/// tracks on the map; its colour is its place along the hue circle from the calm pole, as a share of the library's
/// range: 0 is the logo's azure end, 1 its amber end. `feel` is `n x 2` standardised (valence, arousal).
pub fn colour_field(pos: &[f64], n: usize, feel: &[f64]) -> (ColourFrame, Vec<f64>) {
    let mut centre = [0.0; 3];
    for (d, c) in centre.iter_mut().enumerate() {
        let mut s = 0.0;
        for i in 0..n {
            s += pos[i * 3 + d];
        }
        *c = s / n as f64;
    }
    let mut spread = 0.0;
    for i in 0..n {
        for d in 0..3 {
            let t = pos[i * 3 + d] - centre[d];
            spread += t * t;
        }
    }
    let scale = {
        let s = (spread / n as f64).sqrt();
        if s == 0.0 || s.is_nan() { 1.0 } else { s }
    };
    let mut mean_feel = [0.0; 2];
    for (c, m) in mean_feel.iter_mut().enumerate() {
        let mut s = 0.0;
        for i in 0..n {
            s += feel[i * 2 + c];
        }
        *m = s / n as f64;
    }
    let mut cross = [[0.0; 2]; 3];
    for i in 0..n {
        let target = [feel[i * 2 + 1] - mean_feel[1], feel[i * 2] - mean_feel[0]];
        for d in 0..3 {
            let p = (pos[i * 3 + d] - centre[d]) / scale;
            cross[d][0] += p * target[0];
            cross[d][1] += p * target[1];
        }
    }
    let mtm: Vec<Vec<f64>> = (0..2)
        .map(|r| {
            (0..2)
                .map(|c| {
                    cross[0][r] * cross[0][c]
                        + cross[1][r] * cross[1][c]
                        + cross[2][r] * cross[2][c]
                })
                .collect()
        })
        .collect();
    let (values, vectors) = symmetric_eigen(&mtm);
    let mut turn = [0.0; 6];
    for c in 0..2 {
        let mut sv = values[c].max(0.0).sqrt();
        if sv == 0.0 || sv.is_nan() {
            sv = 1.0;
        }
        for d in 0..3 {
            let u = (cross[d][0] * vectors[0][c] + cross[d][1] * vectors[1][c]) / sv;
            for out in 0..2 {
                turn[d * 2 + out] += u * vectors[out][c];
            }
        }
    }
    let mut frame = ColourFrame {
        centre,
        scale,
        turn,
        hue_min: 0.0,
        hue_max: 360.0,
    };
    let hue = area_hues(&frame, pos, n, pos, n);
    let mut hue_min = f64::INFINITY;
    let mut hue_max = f64::NEG_INFINITY;
    for h in &hue {
        if *h < hue_min {
            hue_min = *h;
        }
        if *h > hue_max {
            hue_max = *h;
        }
    }
    frame.hue_min = hue_min;
    frame.hue_max = hue_max;
    let colour = hue.iter().map(|h| to_share(&frame, *h)).collect();
    (frame, colour)
}

/// The colour of tracks at `query` positions on an existing field, their areas taken among all `pos` tracks.
pub fn colour_at(
    frame: &ColourFrame,
    pos: &[f64],
    count: usize,
    query: &[f64],
    queries: usize,
) -> Vec<f64> {
    area_hues(frame, pos, count, query, queries)
        .iter()
        .map(|h| to_share(frame, *h))
        .collect()
}
