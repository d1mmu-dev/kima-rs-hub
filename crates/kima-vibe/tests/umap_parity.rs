//! The map's arithmetic against umap-learn 0.5.12 / scikit-learn (`map-parity.json`, relative 2e-5), ported from
//! upstream `mapLayout.test.ts`, plus the colour-field properties.
mod common;
use common::*;
use kima_vibe::colour::{colour_at, colour_field};
use kima_vibe::pca::{principal_scores, principal_start};
use kima_vibe::umap::*;
use serde_json::Value;

fn floats(v: &Value) -> Vec<f64> {
    match v {
        Value::Array(a) => a.iter().flat_map(floats).collect(),
        other => vec![other.as_f64().unwrap()],
    }
}

fn ints(v: &Value) -> Vec<u32> {
    floats(v).into_iter().map(|x| x as u32).collect()
}

fn graph_of(idx: &Value, dist: &Value) -> Graph {
    Graph {
        rows: idx.as_array().unwrap().len(),
        k: idx[0].as_array().unwrap().len(),
        idx: ints(idx),
        dist: floats(dist),
    }
}

fn expect_close(actual: &[f64], expected: &[f64], relative: f64) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - e).abs() <= relative * e.abs().max(1.0),
            "[{i}] {a} vs {e}"
        );
    }
}

struct Centred {
    n: usize,
    d: usize,
    rows: Vec<Vec<f64>>,
}

fn centred(points: &Value) -> Centred {
    let pts: Vec<Vec<f64>> = points.as_array().unwrap().iter().map(floats).collect();
    let n = pts.len();
    let d = pts[0].len();
    let mean: Vec<f64> = (0..d)
        .map(|c| pts.iter().map(|p| p[c]).sum::<f64>() / n as f64)
        .collect();
    let rows = pts
        .iter()
        .map(|p| p.iter().zip(&mean).map(|(x, m)| x - m).collect())
        .collect();
    Centred { n, d, rows }
}

impl Centred {
    fn multiply(&self, v: &[f64]) -> Vec<f64> {
        self.rows
            .iter()
            .map(|r| r.iter().zip(v).map(|(x, y)| x * y).sum())
            .collect()
    }
    fn multiply_t(&self, u: &[f64]) -> Vec<f64> {
        let mut out = vec![0.0; self.d];
        for (r, ui) in self.rows.iter().zip(u) {
            for (o, x) in out.iter_mut().zip(r) {
                *o += x * ui;
            }
        }
        out
    }
}

#[test]
fn smooth_knn_scale_and_nearest_distance() {
    let f = fixture("map-parity.json");
    let g = graph_of(&f["knnIdx"], &f["knnDist"]);
    let (rho, sigma) = smooth_knn_dist(&g, 1.0);
    expect_close(&rho, &floats(&f["rho"]), 2e-5);
    expect_close(&sigma, &floats(&f["sigma"]), 2e-5);
}

#[test]
fn two_way_graph_with_weak_edges_dropped() {
    let f = fixture("map-parity.json");
    let g = graph_of(&f["knnIdx"], &f["knnDist"]);
    let edges = fuzzy_graph(&g, f["epochs"].as_u64().unwrap() as usize);
    assert_eq!(edges.head, ints(&f["graph"]["head"]));
    assert_eq!(edges.tail, ints(&f["graph"]["tail"]));
    expect_close(&edges.weight, &floats(&f["graph"]["weight"]), 2e-5);
}

#[test]
fn curve_a_b_for_min_dist_0_1() {
    let f = fixture("map-parity.json");
    assert!((CURVE_A - f["a"].as_f64().unwrap()).abs() < 1e-5);
    assert!((CURVE_B - f["b"].as_f64().unwrap()).abs() < 1e-5);
}

#[test]
fn new_point_start_as_umap_transform() {
    let f = fixture("map-parity.json");
    let t = &f["transform"];
    let g = graph_of(&t["knnIdx"], &t["knnDist"]);
    expect_close(&smooth_knn_dist(&g, 0.0).1, &floats(&t["sigma"]), 2e-5);
    let (pos, weight) = transform_start(&floats(&t["embedding"]), &g);
    expect_close(&weight, &floats(&t["weight"]), 2e-5);
    expect_close(&pos, &floats(&t["start"]), 1e-5);
}

#[test]
fn new_point_on_a_laid_out_one_lands_on_top() {
    let f = fixture("map-parity.json");
    let t = &f["transform"];
    let mut g = graph_of(&t["knnIdx"], &t["knnDist"]);
    g.rows = 1;
    g.idx.truncate(g.k);
    g.dist = floats(&t["knnDist"][0]);
    g.dist[0] = 0.0;
    let embedding = floats(&t["embedding"]);
    let (pos, _) = transform_start(&embedding, &g);
    let twin = g.idx[0] as usize;
    assert_eq!(pos, embedding[twin * 3..twin * 3 + 3]);
}

#[test]
fn principal_axes_match_scikit_learn_signs_included() {
    // Within 1e-6 of the largest score, as upstream asserts.
    let f = fixture("map-parity.json");
    for set in 0..2 {
        let c = centred(&f["pca"][set]["points"]);
        let expected = floats(&f["pca"][set]["scores"]);
        let largest = expected.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
        let actual = principal_scores(c.n, c.d, &|v| c.multiply(v), &|u| c.multiply_t(u), SEED);
        assert_eq!(actual.len(), expected.len());
        for (i, (a, e)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (a - e).abs() <= 1e-6 * largest,
                "set {set} [{i}] {a} vs {e}"
            );
        }
    }
}

#[test]
fn start_scaled_to_largest_coordinate_10() {
    let f = fixture("map-parity.json");
    let c = centred(&f["pca"][1]["points"]);
    let start = principal_start(c.n, c.d, &|v| c.multiply(v), &|u| c.multiply_t(u), SEED);
    let largest = start.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
    assert!(largest > 10.0 - 1e-3 && largest < 10.0 + 1e-3, "{largest}");
}

#[test]
fn epochs_500_up_to_10000_then_200() {
    assert_eq!(layout_epochs(10_000), 500);
    assert_eq!(layout_epochs(10_001), 200);
}

#[test]
fn lays_out_and_places_the_same_way_every_time() {
    let f = fixture("map-parity.json");
    let g = graph_of(&f["knnIdx"], &f["knnDist"]);
    let c = centred(&f["pca"][0]["points"]);
    let start = principal_start(c.n, c.d, &|v| c.multiply(v), &|u| c.multiply_t(u), SEED);
    let first = layout(&g, &start, SEED);
    assert_eq!(layout(&g, &start, SEED), first);
    assert!(first.iter().all(|x| x.is_finite()));
    let t = &f["transform"];
    let tg = graph_of(&t["knnIdx"], &t["knnDist"]);
    let placed = place(&first, c.n, &tg, SEED);
    assert_eq!(place(&first, c.n, &tg, SEED), placed);
    assert!(placed.iter().all(|x| x.is_finite()));
}

/// The colour-field fixtures of the upstream test: 240 tracks on a feel circle.
const N: usize = 240;

fn angle(i: usize) -> f64 {
    2.0 * std::f64::consts::PI * i as f64 / N as f64
}

fn feel() -> Vec<f64> {
    (0..N)
        .flat_map(|i| [angle(i).sin(), angle(i).cos()])
        .collect()
}

fn map_from(axes: [[f64; 3]; 3]) -> Vec<f64> {
    let mut pos = Vec::with_capacity(N * 3);
    for i in 0..N {
        let plane = [
            5.0 * angle(i).cos(),
            5.0 * angle(i).sin(),
            0.3 * (7.0 * angle(i) + 0.4).sin(),
        ];
        for axis in axes {
            pos.push(2.0 + axis.iter().zip(plane).map(|(a, p)| a * p).sum::<f64>());
        }
    }
    pos
}

fn upright() -> Vec<f64> {
    map_from([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
}

fn turned() -> Vec<f64> {
    let (c, s) = (0.7_f64.cos(), 0.7_f64.sin());
    map_from([
        [c, -s, 0.0],
        [s * 0.6, c * 0.6, 0.8],
        [-s * 0.8, -c * 0.8, 0.6],
    ])
}

#[test]
fn colour_follows_feel_round_the_circle() {
    let (_, colour) = colour_field(&upright(), N, &feel());
    assert_eq!(colour.iter().cloned().fold(f64::INFINITY, f64::min), 0.0);
    assert_eq!(
        colour.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        1.0
    );
    assert!((colour[0] - 0.5).abs() < 0.05);
    assert!((colour[N / 4] - 0.75).abs() < 0.05);
    assert!((colour[3 * N / 4] - 0.25).abs() < 0.05);
}

#[test]
fn colour_independent_of_rotation_and_mirror() {
    let base = colour_field(&upright(), N, &feel()).1;
    expect_close(&colour_field(&turned(), N, &feel()).1, &base, 1e-9);
    let mirrored = map_from([[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
    expect_close(&colour_field(&mirrored, N, &feel()).1, &base, 1e-9);
}

#[test]
fn placed_track_coloured_by_same_rule() {
    let t = turned();
    let (frame, colour) = colour_field(&t, N, &feel());
    expect_close(&colour_at(&frame, &t, N, &t, N), &colour, 1e-12);
}
