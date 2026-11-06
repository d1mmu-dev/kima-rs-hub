//! Memory budget of the layout (Task 9, step 3): 20,000 synthetic tracks stay under 600 MB peak resident memory, which
//! scales to the plan's 1 GB at 100,000. This file is its own test binary, so the process peak is this test's.
mod common;
use common::Xorshift;
use kima_vibe::colour::colour_field;
use kima_vibe::umap::{Graph, SEED, layout};

const N: usize = 20_000;
const K: usize = 15;

fn peak_rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: f64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .unwrap()
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap();
    kb / 1024.0
}

#[test]
fn layout_of_20k_tracks_stays_under_600_mb() {
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    // Clusters of 40 tracks: each track's neighbours are other members, nearer ones first.
    let mut graph = Graph {
        rows: N,
        k: K,
        idx: vec![0; N * K],
        dist: vec![0.0; N * K],
    };
    for i in 0..N {
        let base = i / 40 * 40;
        graph.idx[i * K] = i as u32;
        let mut d = 0.0;
        for j in 1..K {
            let mut other = base + (rng.next_u64() % 40) as usize;
            if other == i {
                other = base + (other - base + 1) % 40;
            }
            graph.idx[i * K + j] = other as u32;
            d += 0.01 + rng.unit() * 0.05;
            graph.dist[i * K + j] = d;
        }
    }
    let start: Vec<f64> = (0..N * 3).map(|_| rng.unit() * 10.0).collect();
    let feel: Vec<f64> = (0..N * 2).map(|_| rng.unit() * 2.0 - 1.0).collect();
    let pos = layout(&graph, &start, SEED);
    let (_, colour) = colour_field(&pos, N, &feel);
    assert!(pos.iter().all(|x| x.is_finite()));
    assert!(colour.iter().all(|c| (0.0..=1.0).contains(c)));
    let peak = peak_rss_mb();
    eprintln!("peak RSS {peak:.0} MB");
    assert!(peak < 600.0, "peak RSS {peak:.0} MB");
}
