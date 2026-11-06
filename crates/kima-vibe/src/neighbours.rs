//! Exact neighbour lists from the formula, every pair scored: the public export's lists (capped, the track itself
//! counted) and the vibe map's neighbours (plain top k). Port of upstream `services/matching/neighbours.ts`
//! (v1.12.3, c7527577).
//!
//! The time grows with the square of the library, so callers run this on a blocking thread. Float operations keep
//! upstream's order (the dot product of a row and a column sums in dimension order, no fused multiply-add, no
//! reassociation), because the map goldens compare scores bit for bit. Parallelism is across rows only: a row's
//! scores come out identical whatever the thread count.

use std::collections::HashMap;

use crate::formula::{
    FEEL_WEIGHT, GenreModel, MatchTrack, Placed, WeightedTrack, artist_bonuses, cap_list,
};

/// Neighbour lists grow their candidate pool from this size until the caps leave `k` (or nothing is left).
pub const FIRST_POOL: usize = 64;

/// How the sound dot product accumulates. `F64` is upstream's arithmetic; `F32` is the cheaper choice measured by
/// the benchmark (Task 8) and is never bit-equal to the goldens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accumulation {
    #[default]
    F64,
    F32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Sweep {
    pub accumulation: Accumulation,
    /// Spread rows over the rayon pool. Ignored unless the `parallel` feature is on.
    pub parallel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepStats {
    /// How many times an artist's genre bonuses were built (once per artist when rows are grouped by artist).
    pub bonus_builds: usize,
}

/// Rows x k nearest columns by the formula, best first, equal scores in track id order.
#[derive(Debug, Clone, PartialEq)]
pub struct NearestColumns {
    pub k: usize,
    /// `rows * k` column indices.
    pub idx: Vec<u32>,
    pub score: Vec<f64>,
    /// The highest score between a row and a column other than itself.
    pub max_score: f64,
}

/// Per track id: up to `k` neighbours as (track id, score), best first.
pub type NeighbourLists = HashMap<String, Vec<(String, f64)>>;

/// Rank of each track in id order (ids compared bytewise, as upstream's `<`).
fn id_order(tracks: &[MatchTrack]) -> Vec<u32> {
    let mut order: Vec<usize> = (0..tracks.len()).collect();
    order.sort_by(|a, b| tracks[*a].track_id.cmp(&tracks[*b].track_id));
    let mut rank = vec![0_u32; tracks.len()];
    for (r, i) in order.into_iter().enumerate() {
        rank[i] = r as u32;
    }
    rank
}

trait Real:
    Copy + Default + std::ops::Add<Output = Self> + std::ops::Mul<Output = Self> + Send + Sync
{
    fn from_f64(x: f64) -> Self;
    fn to_f64(self) -> f64;
}
impl Real for f64 {
    fn from_f64(x: f64) -> Self {
        x
    }
    fn to_f64(self) -> f64 {
        self
    }
}
impl Real for f32 {
    fn from_f64(x: f64) -> Self {
        x as f32
    }
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
}

/// Everything a row block needs, shared by all threads.
struct Columns<'a, T: Real> {
    rows: &'a [MatchTrack],
    model: &'a GenreModel,
    gw_sq: f64,
    dim: usize,
    sound: Vec<T>,
    feel_v: Vec<f64>,
    feel_a: Vec<f64>,
    col_artist: Vec<usize>,
    row_artist: Vec<usize>,
    slots: usize,
    slot_ids: HashMap<&'a str, usize>,
}

impl<'a, T: Real> Columns<'a, T> {
    fn new(
        rows: &'a [MatchTrack],
        cols: &'a [MatchTrack],
        model: &'a GenreModel,
        gw_sq: f64,
    ) -> Self {
        let dim = cols[0].sound.len();
        let mut slot_ids: HashMap<&str, usize> = HashMap::new();
        let mut slot_of = |artist: &'a str| {
            let next = slot_ids.len();
            *slot_ids.entry(artist).or_insert(next)
        };
        let mut sound = Vec::with_capacity(cols.len() * dim);
        let (mut feel_v, mut feel_a, mut col_artist) = (vec![], vec![], vec![]);
        for t in cols {
            sound.extend(t.sound.iter().map(|x| T::from_f64(*x)));
            feel_v.push(t.feel[0]);
            feel_a.push(t.feel[1]);
            col_artist.push(slot_of(&t.artist_id));
        }
        let row_artist: Vec<usize> = rows.iter().map(|t| slot_of(&t.artist_id)).collect();
        let slots = slot_ids.len();
        Self {
            rows,
            model,
            gw_sq,
            dim,
            sound,
            feel_v,
            feel_a,
            col_artist,
            row_artist,
            slots,
            slot_ids,
        }
    }

    fn bonus_row(&self, row: usize) -> Vec<f64> {
        let mut bonus = vec![0.0; self.slots];
        let input = [WeightedTrack {
            track: &self.rows[row],
            weight: 1.0,
        }];
        for (artist_id, b) in artist_bonuses(&input, self.model, self.gw_sq) {
            if let Some(slot) = self.slot_ids.get(artist_id.as_str()) {
                bonus[*slot] = b;
            }
        }
        bonus
    }

    /// Scores `block` (up to four rows, artist-contiguous) against every column; `on_row` receives each row's
    /// scores.
    fn block<R>(
        &self,
        block: &[usize],
        bonus_cache: &mut HashMap<usize, Vec<f64>>,
        scores: &mut [Vec<f64>; 4],
        builds: &mut usize,
        on_row: &impl Fn(usize, &[f64]) -> R,
        out: &mut Vec<(usize, R)>,
    ) {
        let feel_sq = FEEL_WEIGHT * FEEL_WEIGHT;
        let n = self.col_artist.len();
        let dim = self.dim;
        bonus_cache.retain(|slot, _| block.iter().any(|i| self.row_artist[*i] == *slot));
        let pad = |q: usize| block[q.min(block.len() - 1)];
        let r: [usize; 4] = [pad(0), pad(1), pad(2), pad(3)];
        for i in r {
            let slot = self.row_artist[i];
            if let std::collections::hash_map::Entry::Vacant(e) = bonus_cache.entry(slot) {
                e.insert(self.bonus_row(i));
                *builds += 1;
            }
        }
        let sounds: [Vec<T>; 4] =
            r.map(|i| self.rows[i].sound.iter().map(|x| T::from_f64(*x)).collect());
        let feels: [[f64; 2]; 4] = r.map(|i| self.rows[i].feel);
        let bonus: [&Vec<f64>; 4] = r.map(|i| &bonus_cache[&self.row_artist[i]]);
        let [s0, s1, s2, s3] = scores;
        for j in 0..n {
            let col = &self.sound[j * dim..(j + 1) * dim];
            let (mut a0, mut a1, mut a2, mut a3) =
                (T::default(), T::default(), T::default(), T::default());
            for d in 0..dim {
                let x = col[d];
                a0 = a0 + sounds[0][d] * x;
                a1 = a1 + sounds[1][d] * x;
                a2 = a2 + sounds[2][d] * x;
                a3 = a3 + sounds[3][d] * x;
            }
            let (v, a) = (self.feel_v[j], self.feel_a[j]);
            let slot = self.col_artist[j];
            s0[j] = a0.to_f64() + feel_sq * (feels[0][0] * v + feels[0][1] * a) + bonus[0][slot];
            s1[j] = a1.to_f64() + feel_sq * (feels[1][0] * v + feels[1][1] * a) + bonus[1][slot];
            s2[j] = a2.to_f64() + feel_sq * (feels[2][0] * v + feels[2][1] * a) + bonus[2][slot];
            s3[j] = a3.to_f64() + feel_sq * (feels[3][0] * v + feels[3][1] * a) + bonus[3][slot];
        }
        for (q, row) in block.iter().enumerate() {
            out.push((*row, on_row(*row, &scores[q])));
        }
    }

    fn run_chunk<R>(
        &self,
        order: &[usize],
        on_row: &impl Fn(usize, &[f64]) -> R,
    ) -> (Vec<(usize, R)>, usize) {
        let n = self.col_artist.len();
        let mut scores = [vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]];
        let mut cache = HashMap::new();
        let (mut builds, mut out) = (0, Vec::with_capacity(order.len()));
        for block in order.chunks(4) {
            self.block(
                block,
                &mut cache,
                &mut scores,
                &mut builds,
                on_row,
                &mut out,
            );
        }
        (out, builds)
    }
}

/// Scores every row track against every column track with the formula, four rows per sweep over the columns, and
/// hands each row's scores to `on_row`. Rows are visited grouped by artist, so each artist's genre bonuses are
/// built once. The results come back in visiting order.
fn score_rows<T: Real, R: Send>(
    sweep: Sweep,
    rows: &[MatchTrack],
    cols: &[MatchTrack],
    model: &GenreModel,
    gw_sq: f64,
    on_row: impl Fn(usize, &[f64]) -> R + Sync,
) -> (Vec<(usize, R)>, SweepStats) {
    if rows.is_empty() || cols.is_empty() {
        return (vec![], SweepStats::default());
    }
    let ctx = Columns::<T>::new(rows, cols, model, gw_sq);
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|i| ctx.row_artist[*i]); // stable, as upstream's sort

    #[cfg(feature = "parallel")]
    if sweep.parallel {
        use rayon::prelude::*;
        // Chunks hold whole blocks of four, so every row sees the same block mates as in the serial order.
        let parts: Vec<(Vec<(usize, R)>, usize)> = order
            .par_chunks(4 * 16)
            .map(|chunk| ctx.run_chunk(chunk, &on_row))
            .collect();
        let builds = parts.iter().map(|p| p.1).sum();
        let out = parts.into_iter().flat_map(|p| p.0).collect();
        return (
            out,
            SweepStats {
                bonus_builds: builds,
            },
        );
    }
    let _ = sweep.parallel;
    let (out, builds) = ctx.run_chunk(&order, &on_row);
    (
        out,
        SweepStats {
            bonus_builds: builds,
        },
    )
}

fn dispatch<R: Send>(
    sweep: Sweep,
    rows: &[MatchTrack],
    cols: &[MatchTrack],
    model: &GenreModel,
    gw_sq: f64,
    on_row: impl Fn(usize, &[f64]) -> R + Sync,
) -> (Vec<(usize, R)>, SweepStats) {
    match sweep.accumulation {
        Accumulation::F64 => score_rows::<f64, R>(sweep, rows, cols, model, gw_sq, on_row),
        Accumulation::F32 => score_rows::<f32, R>(sweep, rows, cols, model, gw_sq, on_row),
    }
}

/// `x` ranks below `y`: lower score, or equal score and later in id order.
fn worse(scores: &[f64], id_rank: &[u32], x: usize, y: usize) -> bool {
    scores[x] < scores[y] || (scores[x] == scores[y] && id_rank[x] > id_rank[y])
}

/// Indices of the `size` best scores, best first, equal scores in id order; `skip` is never chosen. A binary heap
/// whose root is the worst kept index (upstream `topIndices`).
pub fn top_indices(
    scores: &[f64],
    id_rank: &[u32],
    size: usize,
    skip: Option<usize>,
) -> Vec<usize> {
    let w = |x: usize, y: usize| worse(scores, id_rank, x, y);
    let mut heap: Vec<usize> = Vec::with_capacity(size);
    let sift_down = |heap: &mut Vec<usize>, mut i: usize| {
        loop {
            let (l, r) = (2 * i + 1, 2 * i + 2);
            let mut m = i;
            if l < heap.len() && w(heap[l], heap[m]) {
                m = l;
            }
            if r < heap.len() && w(heap[r], heap[m]) {
                m = r;
            }
            if m == i {
                return;
            }
            heap.swap(i, m);
            i = m;
        }
    };
    if size == 0 {
        return heap;
    }
    for j in 0..scores.len() {
        if Some(j) == skip {
            continue;
        }
        if heap.len() < size {
            heap.push(j);
            let mut i = heap.len() - 1;
            while i > 0 {
                let p = (i - 1) >> 1;
                if !w(heap[i], heap[p]) {
                    break;
                }
                heap.swap(i, p);
                i = p;
            }
        } else if w(heap[0], j) {
            heap[0] = j;
            sift_down(&mut heap, 0);
        }
    }
    heap.sort_by(|x, y| {
        if w(*x, *y) {
            std::cmp::Ordering::Greater
        } else if w(*y, *x) {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    });
    heap
}

struct Ranked<'a> {
    index: usize,
    track: &'a MatchTrack,
}

impl Placed for Ranked<'_> {
    fn album_id(&self) -> &str {
        &self.track.album_id
    }
    fn artist_id(&self) -> &str {
        &self.track.artist_id
    }
}

/// Up to `k` neighbours per track as (id, score), best first, capped (2 per album, 4 per artist) with the track
/// itself counted.
pub fn neighbour_lists_with(
    sweep: Sweep,
    tracks: &[MatchTrack],
    model: &GenreModel,
    gw_sq: f64,
    k: usize,
) -> NeighbourLists {
    let mut lists = NeighbourLists::new();
    if tracks.len() < 2 {
        for t in tracks {
            lists.insert(t.track_id.clone(), vec![]);
        }
        return lists;
    }
    let id_rank = id_order(tracks);
    let (rows, _) = dispatch(sweep, tracks, tracks, model, gw_sq, |row, scores| {
        let seed = &tracks[row];
        let mut pool = FIRST_POOL;
        loop {
            let size = pool.min(tracks.len() - 1);
            let ranked: Vec<Ranked> = top_indices(scores, &id_rank, size, Some(row))
                .into_iter()
                .map(|index| Ranked {
                    index,
                    track: &tracks[index],
                })
                .collect();
            let picked = cap_list(ranked, std::slice::from_ref(seed), k);
            if picked.len() == k || size == tracks.len() - 1 {
                return picked
                    .iter()
                    .map(|c| (tracks[c.index].track_id.clone(), scores[c.index]))
                    .collect::<Vec<_>>();
            }
            pool *= 2;
        }
    });
    for (row, list) in rows {
        lists.insert(tracks[row].track_id.clone(), list);
    }
    lists
}

pub fn neighbour_lists(
    tracks: &[MatchTrack],
    model: &GenreModel,
    gw_sq: f64,
    k: usize,
) -> NeighbourLists {
    neighbour_lists_with(Sweep::default(), tracks, model, gw_sq, k)
}

/// Each row track's `k` nearest column tracks by the formula, no album or artist rule. When the rows are the
/// columns (`same_set`), a track is never its own neighbour.
pub fn nearest_columns_with(
    sweep: Sweep,
    rows: &[MatchTrack],
    cols: &[MatchTrack],
    model: &GenreModel,
    gw_sq: f64,
    k: usize,
    same_set: bool,
) -> (NearestColumns, SweepStats) {
    let size = k.min(cols.len().saturating_sub(usize::from(same_set)));
    let id_rank = id_order(cols);
    let (found, stats) = dispatch(sweep, rows, cols, model, gw_sq, |row, scores| {
        let skip = same_set.then_some(row);
        let mut best = f64::NEG_INFINITY;
        for (j, s) in scores.iter().enumerate() {
            if Some(j) != skip && *s > best {
                best = *s;
            }
        }
        let top: Vec<(u32, f64)> = top_indices(scores, &id_rank, size, skip)
            .into_iter()
            .map(|j| (j as u32, scores[j]))
            .collect();
        (best, top)
    });
    let mut idx = vec![0_u32; rows.len() * size];
    let mut score = vec![0.0; rows.len() * size];
    let mut max_score = f64::NEG_INFINITY;
    for (row, (best, top)) in found {
        max_score = max_score.max(best);
        for (r, (j, s)) in top.into_iter().enumerate() {
            idx[row * size + r] = j;
            score[row * size + r] = s;
        }
    }
    (
        NearestColumns {
            k: size,
            idx,
            score,
            max_score,
        },
        stats,
    )
}

pub fn nearest_columns(
    rows: &[MatchTrack],
    cols: &[MatchTrack],
    model: &GenreModel,
    gw_sq: f64,
    k: usize,
    same_set: bool,
) -> NearestColumns {
    nearest_columns_with(Sweep::default(), rows, cols, model, gw_sq, k, same_set).0
}
