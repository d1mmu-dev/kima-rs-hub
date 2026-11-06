//! The map's arithmetic for the similarity step (port of upstream `mapCompute.ts`): the whole-library layout and the
//! placement of new tracks, both from the matching formula's own neighbours.

// Index loops mirror the TypeScript line for line: the port is checked bit for bit against it.
#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;

use crate::colour::{ColourFrame, colour_at, colour_field};
use crate::formula::{FEEL_WEIGHT, GenreModel, MatchTrack};
use crate::neighbours::{NearestColumns, nearest_columns};
use crate::pca::principal_start;
use crate::umap::{Graph, MAP_NEIGHBOURS, SEED, layout, place};

#[derive(Debug, Clone)]
pub struct LaidOutMap {
    /// `n x 3`.
    pub positions: Vec<f64>,
    pub colour: Vec<f64>,
    pub frame: ColourFrame,
    /// The highest score between two tracks; a distance is this minus the score.
    pub max_score: f64,
}

#[derive(Debug, Clone)]
pub struct PlacedTracks {
    pub positions: Vec<f64>,
    pub colour: Vec<f64>,
}

type Product<'a> = Box<dyn Fn(&[f64]) -> Vec<f64> + 'a>;

/// Multiplication by the centred matrix whose rows are [sound; 0.25 feel; w_g genre], the vector the formula is the
/// dot product of (genre counted for an artist's own tracks too), without building it: each artist's genre vector is
/// its sparse TF-IDF entries less the library mean, over its norm. Returns `(d, multiply, multiply_t)`.
fn centred_rows<'a>(
    tracks: &'a [MatchTrack],
    model: &'a GenreModel,
    genre_weight: f64,
) -> (usize, Product<'a>, Product<'a>) {
    let n = tracks.len();
    let dim = tracks[0].sound.len();
    let vocab = model.vocabulary.len();
    let d = dim + 2 + vocab;
    let entry: Vec<_> = tracks
        .iter()
        .map(|t| model.artists.get(&t.artist_id))
        .collect();
    let entry_raw = entry.clone();
    let raw = move |v: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0; n];
        let mut mean_dot = 0.0;
        for g in 0..vocab {
            mean_dot += model.mean[g] * v[dim + 2 + g];
        }
        let mut genre_dot: HashMap<&str, f64> = HashMap::new();
        for i in 0..n {
            let t = &tracks[i];
            let mut s = 0.0;
            for c in 0..dim {
                s += t.sound[c] * v[c];
            }
            s += FEEL_WEIGHT * (t.feel[0] * v[dim] + t.feel[1] * v[dim + 1]);
            if let Some(e) = entry_raw[i] {
                let gd = *genre_dot.entry(t.artist_id.as_str()).or_insert_with(|| {
                    let mut gd = 0.0;
                    for (k, g) in e.index.iter().enumerate() {
                        gd += e.weight[k] * v[dim + 2 + g];
                    }
                    (gd - mean_dot) / e.norm
                });
                s += genre_weight * gd;
            }
            out[i] = s;
        }
        out
    };
    let raw_t = {
        move |u: &[f64]| -> Vec<f64> {
            let mut out = vec![0.0; d];
            // First-seen order, as a JS Map iterates: the sums below depend on it.
            let mut order: Vec<&str> = Vec::new();
            let mut by_artist: HashMap<&str, f64> = HashMap::new();
            for i in 0..n {
                let t = &tracks[i];
                for c in 0..dim {
                    out[c] += u[i] * t.sound[c];
                }
                out[dim] += FEEL_WEIGHT * u[i] * t.feel[0];
                out[dim + 1] += FEEL_WEIGHT * u[i] * t.feel[1];
                if entry[i].is_some() {
                    let slot = by_artist.entry(t.artist_id.as_str()).or_insert_with(|| {
                        order.push(t.artist_id.as_str());
                        0.0
                    });
                    *slot += u[i];
                }
            }
            let mut mean_share = 0.0;
            for artist_id in order {
                let total = by_artist[artist_id];
                let e = &model.artists[artist_id];
                let share = (genre_weight * total) / e.norm;
                for (k, g) in e.index.iter().enumerate() {
                    out[dim + 2 + g] += share * e.weight[k];
                }
                mean_share += share;
            }
            for g in 0..vocab {
                out[dim + 2 + g] -= mean_share * model.mean[g];
            }
            out
        }
    };
    let mean = raw_t(&vec![1.0 / n as f64; n]);
    let mean_m = mean.clone();
    let multiply: Product<'a> = Box::new(move |v: &[f64]| {
        let mut out = raw(v);
        let mut shift = 0.0;
        for c in 0..d {
            shift += mean_m[c] * v[c];
        }
        for o in out.iter_mut() {
            *o -= shift;
        }
        out
    });
    let multiply_t: Product<'a> = Box::new(move |u: &[f64]| {
        let mut out = raw_t(u);
        let mut total = 0.0;
        for x in u.iter().take(n) {
            total += x;
        }
        for c in 0..d {
            out[c] -= mean[c] * total;
        }
        out
    });
    (d, multiply, multiply_t)
}

/// The 15-neighbour graph of umap-learn from the formula's own neighbour lists: row i is track i first (distance 0),
/// then its nearest tracks with distance `max_score - score`.
pub fn library_graph(n: usize, near: &NearestColumns) -> Graph {
    let k = near.k + 1;
    let mut graph = Graph {
        rows: n,
        k,
        idx: vec![0; n * k],
        dist: vec![0.0; n * k],
    };
    for i in 0..n {
        graph.idx[i * k] = i as u32;
        for r in 0..near.k {
            graph.idx[i * k + 1 + r] = near.idx[i * near.k + r];
            graph.dist[i * k + 1 + r] = near.max_score - near.score[i * near.k + r];
        }
    }
    graph
}

/// The layout's start: the library's 3 main axes (of the matrix the formula is the dot product of), scaled like
/// umap-learn's `noisy_scale_coords`.
pub fn library_start(
    tracks: &[MatchTrack],
    model: &GenreModel,
    genre_weight: f64,
    seed: u32,
) -> Vec<f64> {
    let (d, multiply, multiply_t) = centred_rows(tracks, model, genre_weight);
    principal_start(tracks.len(), d, &*multiply, &*multiply_t, seed)
}

/// The whole library's map from neighbour lists that were already swept (`near` from a same-set
/// `nearest_columns(.., MAP_NEIGHBOURS - 1, true)`).
pub fn layout_from_neighbours(
    tracks: &[MatchTrack],
    feel_z: &[f64],
    model: &GenreModel,
    genre_weight: f64,
    near: &NearestColumns,
    seed: Option<u32>,
) -> LaidOutMap {
    let seed = seed.unwrap_or(SEED);
    let n = tracks.len();
    let graph = library_graph(n, near);
    let start = library_start(tracks, model, genre_weight, seed);
    let positions = layout(&graph, &start, seed);
    let (frame, colour) = colour_field(&positions, n, feel_z);
    LaidOutMap {
        positions,
        colour,
        frame,
        max_score: near.max_score,
    }
}

/// The whole library's map: neighbours, start, layout and colour field. `feel_z` is `n x 2` standardised (valence,
/// arousal). `seed` is fixed in the engine (`None`); other seeds measure how much a fresh layout moves things.
pub fn layout_library(
    tracks: &[MatchTrack],
    feel_z: &[f64],
    model: &GenreModel,
    genre_weight: f64,
    seed: Option<u32>,
) -> LaidOutMap {
    let near = nearest_columns(
        tracks,
        tracks,
        model,
        genre_weight * genre_weight,
        MAP_NEIGHBOURS - 1,
        true,
    );
    layout_from_neighbours(tracks, feel_z, model, genre_weight, &near, seed)
}

/// New tracks placed among the laid-out ones by umap's transform rule; each coloured by its area among every
/// positioned track (`positioned`, laid out and placed before, then these).
#[allow(clippy::too_many_arguments)]
pub fn place_tracks(
    laid_out: &[MatchTrack],
    laid_out_positions: &[f64],
    positioned: &[f64],
    new_tracks: &[MatchTrack],
    model: &GenreModel,
    genre_weight: f64,
    frame: &ColourFrame,
    max_score: f64,
) -> PlacedTracks {
    let m = new_tracks.len();
    let near = nearest_columns(
        new_tracks,
        laid_out,
        model,
        genre_weight * genre_weight,
        MAP_NEIGHBOURS,
        false,
    );
    let graph = Graph {
        rows: m,
        k: near.k,
        idx: near.idx.clone(),
        dist: near
            .score
            .iter()
            .map(|s| (max_score - s).max(0.0))
            .collect(),
    };
    let positions = place(laid_out_positions, laid_out.len(), &graph, SEED);
    let mut all = Vec::with_capacity(positioned.len() + positions.len());
    all.extend_from_slice(positioned);
    all.extend_from_slice(&positions);
    let colour = colour_at(frame, &all, all.len() / 3, &positions, m);
    PlacedTracks { positions, colour }
}
