//! The vibe map's layout: UMAP's published method step for step with umap-learn 0.5.12, run on the matching
//! formula's own neighbours (port of upstream `mapLayout.ts` v1.12.3, c7527577). Only `exact_math::{exp, log}` and
//! IEEE-exact operations, so the map is bit-identical to upstream's for the same inputs. Do not reorder float
//! operations: `tests/layout_golden.rs` compares bit patterns produced by the TypeScript code.

// Index loops mirror the TypeScript line for line: the port is checked bit for bit against it.
#![allow(clippy::needless_range_loop)]

use std::f64::consts::LN_2;

use crate::exact_math::{exp, log};
use crate::prng::SeededRandom;

/// Neighbours per track, the track itself counted first as umap-learn counts it.
pub const MAP_NEIGHBOURS: usize = 15;
/// umap-learn's `find_ab_params(spread 1, min_dist 0.1)`.
pub const CURVE_A: f64 = 1.576_943_460_465_84;
pub const CURVE_B: f64 = 0.895_060_877_963_997_4;
/// Fixed in the engine; other seeds measure how much a fresh layout moves things.
pub const SEED: u32 = 42;
const NEGATIVE_RATE: f64 = 5.0;

/// `rows x k` neighbour lists: indices and distances, nearest first.
#[derive(Debug, Clone)]
pub struct Graph {
    pub rows: usize,
    pub k: usize,
    pub idx: Vec<u32>,
    pub dist: Vec<f64>,
}

/// The edges of the two-way graph in umap-learn's row-major order.
#[derive(Debug, Clone, Default)]
pub struct Edges {
    pub head: Vec<u32>,
    pub tail: Vec<u32>,
    pub weight: Vec<f64>,
}

fn clip(x: f64) -> f64 {
    x.clamp(-4.0, 4.0)
}

/// umap-learn's passes over the edges: 500 up to 10,000 tracks, 200 above.
pub fn layout_epochs(tracks: usize) -> usize {
    if tracks <= 10_000 { 500 } else { 200 }
}

/// Each track's own scale (`smooth_knn_dist`): rho the distance to its nearest neighbour, sigma found by 64 halvings
/// so its neighbours' strengths add up to log2 k, never below 0.001 x the mean distance. Column 0 is left out of the
/// sum, as umap-learn leaves it out.
pub fn smooth_knn_dist(graph: &Graph, local_connectivity: f64) -> (Vec<f64>, Vec<f64>) {
    let Graph { rows, k, dist, .. } = graph;
    let (rows, k) = (*rows, *k);
    let target = log(k as f64) / LN_2;
    let mut rho = vec![0.0; rows];
    let mut sigma = vec![0.0; rows];
    let mut total = 0.0;
    for d in dist.iter().take(rows * k) {
        total += d;
    }
    let mean_all = total / (rows * k) as f64;
    for i in 0..rows {
        let row = &dist[i * k..(i + 1) * k];
        let non_zero: Vec<f64> = row.iter().copied().filter(|d| *d > 0.0).collect();
        if non_zero.len() as f64 >= local_connectivity {
            let index = local_connectivity.floor() as usize;
            let interpolation = local_connectivity - index as f64;
            if index > 0 {
                rho[i] = non_zero[index - 1];
                if interpolation > 1e-5 {
                    rho[i] += interpolation * (non_zero[index] - non_zero[index - 1]);
                }
            } else {
                rho[i] = if non_zero.is_empty() {
                    0.0
                } else {
                    interpolation * non_zero[0]
                };
            }
        } else if !non_zero.is_empty() {
            rho[i] = non_zero.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        }
        let mut lo = 0.0;
        let mut hi = f64::INFINITY;
        let mut mid = 1.0;
        for _ in 0..64 {
            let mut sum = 0.0;
            for d in &row[1..] {
                let d = d - rho[i];
                sum += if d > 0.0 { exp(-(d / mid)) } else { 1.0 };
            }
            if (sum - target).abs() < 1e-5 {
                break;
            }
            if sum > target {
                hi = mid;
                mid = (lo + hi) / 2.0;
            } else {
                lo = mid;
                mid = if hi == f64::INFINITY {
                    mid * 2.0
                } else {
                    (lo + hi) / 2.0
                };
            }
        }
        let mut mean_ith = 0.0;
        for d in row {
            mean_ith += d;
        }
        mean_ith /= k as f64;
        let floor = if rho[i] > 0.0 {
            1e-3 * mean_ith
        } else {
            1e-3 * mean_all
        };
        sigma[i] = if mid < floor { floor } else { mid };
    }
    (rho, sigma)
}

/// `compute_membership_strengths` for one edge.
fn strength(d: f64, rho: f64, sigma: f64) -> f64 {
    if d - rho <= 0.0 || sigma == 0.0 {
        1.0
    } else {
        exp(-((d - rho) / sigma))
    }
}

/// The two-way graph (fuzzy union A + At - A.At), edges weaker than max/epochs dropped, in umap-learn's row-major
/// order. Row i of `graph` is track i's neighbours with i itself first. Keys are `i * rows + c`.
pub fn fuzzy_graph(graph: &Graph, epochs: usize) -> Edges {
    let Graph { rows, k, idx, dist } = graph;
    let (rows, k) = (*rows, *k);
    let (rho, sigma) = smooth_knn_dist(graph, 1.0);
    let key = |i: usize, c: usize| (i as u64) * (rows as u64) + c as u64;
    let mut one: Vec<(u64, f64)> = Vec::with_capacity(rows * k);
    for i in 0..rows {
        for j in 0..k {
            let c = idx[i * k + j] as usize;
            if c == i {
                continue;
            }
            let w = strength(dist[i * k + j], rho[i], sigma[i]);
            if w > 0.0 {
                one.push((key(i, c), w));
            }
        }
    }
    // A repeated key keeps its last strength, as a Map's `set` does.
    one.reverse();
    one.sort_by_key(|e| e.0); // stable: among equal keys the originally last comes first
    one.dedup_by_key(|e| e.0);
    let lookup = |k: u64| one.binary_search_by_key(&k, |e| e.0).ok().map(|p| one[p].1);
    let mut both: Vec<(u64, f64)> = Vec::with_capacity(one.len() * 2);
    for &(kk, w) in &one {
        let i = (kk / rows as u64) as usize;
        let c = (kk % rows as u64) as usize;
        let back = lookup(key(c, i)).unwrap_or(0.0);
        let union = w + back - w * back;
        both.push((kk, union));
        both.push((key(c, i), union));
    }
    // The union is symmetric (commutative IEEE operations), so duplicates of a key agree.
    both.sort_by_key(|e| e.0);
    both.dedup_by_key(|e| e.0);
    let mut max = 0.0_f64;
    for (_, w) in &both {
        max = max.max(*w);
    }
    let floor = max / epochs as f64;
    let mut edges = Edges::default();
    for (kk, w) in both {
        if w >= floor {
            edges.head.push((kk / rows as u64) as u32);
            edges.tail.push((kk % rows as u64) as u32);
            edges.weight.push(w);
        }
    }
    edges
}

/// Where the tail positions of an edge come from: the moving positions themselves (layout) or a held-still copy of
/// the laid-out tracks (placement).
enum Tail<'a> {
    Same,
    Fixed(&'a [f64]),
}

/// The moves (`optimize_layout_euclidean`): each edge pulls its tracks together by the derivative of log q,
/// q = 1/(1 + a d^2b), and pushes against random tracks by the derivative of log(1 - q); every move is clipped to
/// +-4; the step size falls linearly from `alpha0` to 0. Edges are visited in proportion to their weight.
#[allow(clippy::too_many_arguments)]
fn optimize(
    head: &mut [f64],
    tail: Tail<'_>,
    edges: &Edges,
    epochs: usize,
    vertices: usize,
    alpha0: f64,
    rng: &mut SeededRandom,
    move_other: bool,
) {
    let m = edges.head.len();
    let mut wmax = 0.0_f64;
    for w in &edges.weight {
        wmax = wmax.max(*w);
    }
    let per_sample: Vec<f64> = edges.weight.iter().map(|w| wmax / w).collect();
    let per_negative: Vec<f64> = per_sample.iter().map(|p| p / NEGATIVE_RATE).collect();
    let mut next_sample = per_sample.clone();
    let mut next_negative = per_negative.clone();
    let (a, b) = (CURVE_A, CURVE_B);
    let mut alpha = alpha0;
    for n in 0..epochs {
        let nf = n as f64;
        for e in 0..m {
            if next_sample[e] > nf {
                continue;
            }
            let cj = edges.head[e] as usize * 3;
            let ok = edges.tail[e] as usize * 3;
            // Reading a tail coordinate: live (layout) or held still (placement).
            let tail_at = |head: &[f64], i: usize| match &tail {
                Tail::Same => head[i],
                Tail::Fixed(f) => f[i],
            };
            let mut d2 = 0.0;
            for d in 0..3 {
                let q = head[cj + d] - tail_at(head, ok + d);
                d2 += q * q;
            }
            let mut pull = 0.0;
            if d2 > 0.0 {
                let powered = exp(b * log(d2));
                pull = (-2.0 * a * b * (powered / d2)) / (a * powered + 1.0);
            }
            for d in 0..3 {
                let g = clip(pull * (head[cj + d] - tail_at(head, ok + d)));
                head[cj + d] += g * alpha;
                if move_other {
                    // Only reached in layout, where the tail is the head array.
                    head[ok + d] -= g * alpha;
                }
            }
            next_sample[e] += per_sample[e];
            let negatives = ((nf - next_negative[e]) / per_negative[e]).trunc() as i64;
            for _ in 0..negatives {
                let other = (rng.next_f64() * vertices as f64).floor() as usize;
                let o = other * 3;
                let mut dd = 0.0;
                for d in 0..3 {
                    let q = head[cj + d] - tail_at(head, o + d);
                    dd += q * q;
                }
                let mut push = 0.0;
                if dd > 0.0 {
                    push = (2.0 * b) / ((0.001 + dd) * (a * exp(b * log(dd)) + 1.0));
                } else if edges.head[e] as usize == other {
                    continue;
                }
                for d in 0..3 {
                    let delta = if push > 0.0 {
                        clip(push * (head[cj + d] - tail_at(head, o + d)))
                    } else {
                        0.0
                    };
                    head[cj + d] += delta * alpha;
                }
            }
            next_negative[e] += negatives as f64 * per_negative[e];
        }
        alpha = alpha0 * (1.0 - nf / epochs as f64);
    }
}

/// Every track's position (`rows x 3`): the fuzzy graph of its neighbours, `start` stretched to 0-10 per axis, then
/// the moves.
pub fn layout(graph: &Graph, start: &[f64], seed: u32) -> Vec<f64> {
    let n = graph.rows;
    let epochs = layout_epochs(n);
    let edges = fuzzy_graph(graph, epochs);
    let mut pos = vec![0.0; n * 3];
    for d in 0..3 {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for i in 0..n {
            lo = lo.min(start[i * 3 + d]);
            hi = hi.max(start[i * 3 + d]);
        }
        let span = {
            let s = hi - lo;
            if s == 0.0 || s.is_nan() { 1.0 } else { s }
        };
        for i in 0..n {
            pos[i * 3 + d] = (10.0 * (start[i * 3 + d] - lo)) / span;
        }
    }
    let mut rng = SeededRandom::new(seed);
    optimize(&mut pos, Tail::Same, &edges, epochs, n, 1.0, &mut rng, true);
    pos
}

/// New tracks among laid-out ones (umap `transform`): their neighbours' strengths with local connectivity 0, a start
/// at the strength-weighted mean of those neighbours (`init_graph_transform`), then 100 passes (30 above 10,000 new
/// tracks) at a quarter of the step size with every laid-out track held still. Row i of `graph` lists new track i's
/// nearest laid-out tracks, nearest first.
pub fn place(fitted: &[f64], fitted_count: usize, graph: &Graph, seed: u32) -> Vec<f64> {
    let Graph { rows, k, idx, .. } = graph;
    let (rows, k) = (*rows, *k);
    let (mut pos, weight) = transform_start(fitted, graph);
    let epochs = if rows <= 10_000 { 100 } else { 30 };
    let mut wmax = 0.0_f64;
    for w in &weight {
        wmax = wmax.max(*w);
    }
    let floor = wmax / epochs as f64;
    let mut edges = Edges::default();
    for i in 0..rows {
        for j in 0..k {
            let w = weight[i * k + j];
            if w < floor {
                continue;
            }
            edges.head.push(i as u32);
            edges.tail.push(idx[i * k + j]);
            edges.weight.push(w);
        }
    }
    let mut rng = SeededRandom::new(seed);
    optimize(
        &mut pos,
        Tail::Fixed(&fitted[..fitted_count * 3]),
        &edges,
        epochs,
        fitted_count,
        0.25,
        &mut rng,
        false,
    );
    pos
}

/// A new track's starting point (`init_graph_transform`) and its edge strengths, local connectivity 0.
pub fn transform_start(fitted: &[f64], graph: &Graph) -> (Vec<f64>, Vec<f64>) {
    let Graph { rows, k, idx, dist } = graph;
    let (rows, k) = (*rows, *k);
    let (rho, sigma) = smooth_knn_dist(graph, 0.0);
    let mut weight = vec![0.0; rows * k];
    for i in 0..rows {
        for j in 0..k {
            weight[i * k + j] = strength(dist[i * k + j], rho[i], sigma[i]);
        }
    }
    let mut pos = vec![0.0; rows * 3];
    for i in 0..rows {
        let mut sum = 0.0;
        for j in 0..k {
            sum += weight[i * k + j];
        }
        for j in 0..k {
            let w = weight[i * k + j];
            let c = idx[i * k + j] as usize * 3;
            if w == 1.0 {
                pos[i * 3..i * 3 + 3].copy_from_slice(&fitted[c..c + 3]);
                break;
            }
            for d in 0..3 {
                pos[i * 3 + d] += (w / sum) * fitted[c + d];
            }
        }
    }
    (pos, weight)
}
