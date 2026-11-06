//! Task 8 benchmark: where the neighbour sweep spends its time, and what the full first layout's sweep would cost.
//!
//!   cargo run --release -p kima-vibe --features parallel --example neighbour_bench
//!
//! Environment: `N` columns (default 20000), `ROWS` sampled rows (default 400, artist-contiguous, so bonus builds
//! are representative), `ACC` = f64|f32, `THREADS` (default 8; 0 = rayon's default). The cost of a sweep is linear
//! in the number of rows, so `time * N / ROWS` is the full same-set sweep of N tracks.
use std::time::Instant;

use kima_vibe::formula::{LibraryArtist, MatchTrack, build_genre_model, unit};
use kima_vibe::neighbours::{Accumulation, Sweep, nearest_columns_with, top_indices};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn main() {
    let n: usize = env("N", 20_000);
    let rows: usize = env("ROWS", 400).min(n);
    let threads: usize = env("THREADS", 8);
    let acc = if env::<String>("ACC", "f64".into()) == "f32" {
        Accumulation::F32
    } else {
        Accumulation::F64
    };
    let parallel = threads != 1;
    if parallel && threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .unwrap();
    }
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let artists = n / 10;
    let centres: Vec<Vec<f64>> = (0..artists.min(2000))
        .map(|_| (0..512).map(|_| rng.next()).collect())
        .collect();
    let tracks: Vec<MatchTrack> = (0..n)
        .map(|i| {
            let a = i / 10;
            let c = &centres[a % centres.len()];
            MatchTrack {
                track_id: format!("{i:08}"),
                album_id: format!("al{}-{}", a, (i % 10) / 5),
                artist_id: format!("ar{a:06}"),
                sound: unit(&c.iter().map(|x| x + 0.8 * rng.next()).collect::<Vec<_>>()),
                feel: unit(&[rng.next(), rng.next()]).try_into().unwrap(),
            }
        })
        .collect();
    let genres = ["rock", "pop", "jazz", "folk", "metal", "techno"];
    let lib_artists: Vec<LibraryArtist> = (0..artists)
        .map(|a| LibraryArtist {
            artist_id: format!("ar{a:06}"),
            tracks: 10,
            genres: if a % 4 == 0 {
                vec![]
            } else {
                vec![genres[a % 6].to_string(), genres[(a / 6) % 6].to_string()]
            },
        })
        .collect();
    let (model, _) = build_genre_model(&lib_artists);
    let gw_sq = 0.0625;
    println!("n={n} rows={rows} acc={acc:?} threads={threads} parallel={parallel}");

    let sweep = Sweep {
        accumulation: acc,
        parallel,
    };
    let t = Instant::now();
    let (near, stats) =
        nearest_columns_with(sweep, &tracks[..rows], &tracks, &model, gw_sq, 14, false);
    let full = t.elapsed().as_secs_f64();
    let pairs = rows as f64 * n as f64;
    println!(
        "sweep {rows} rows x {n} cols: {full:.2} s  ({:.1} M pairs/s, {:.2} GFLOP/s dot-product), bonus builds {}, max {:.4}",
        pairs / full / 1e6,
        pairs * 512.0 * 2.0 / full / 1e9,
        stats.bonus_builds,
        near.max_score
    );
    println!(
        "=> full same-set sweep of {n}: {:.0} s ({:.1} min)",
        full * n as f64 / rows as f64,
        full * n as f64 / rows as f64 / 60.0
    );
    if n != 100_000 {
        println!(
            "=> extrapolated to 100000 (x (1e5/n)^2): {:.0} s ({:.1} min)",
            full * n as f64 / rows as f64 * (1e5 / n as f64).powi(2),
            full * n as f64 / rows as f64 * (1e5 / n as f64).powi(2) / 60.0
        );
    }

    // Where the time goes, single thread: selection vs the rest.
    let scores: Vec<f64> = (0..n).map(|_| rng.next()).collect();
    let id_rank: Vec<u32> = (0..n as u32).collect();
    let reps = 200;
    let t = Instant::now();
    let mut sink = 0;
    for r in 0..reps {
        sink += top_indices(&scores, &id_rank, 14, Some(r))[0];
    }
    let top = t.elapsed().as_secs_f64() / reps as f64;
    println!(
        "top_indices(k=14) over {n} scores: {:.2} ms per row ({sink}); x{rows} rows = {:.2} s single-thread",
        top * 1e3,
        top * rows as f64
    );
    let t = Instant::now();
    let one = nearest_columns_with(
        Sweep {
            accumulation: acc,
            parallel: false,
        },
        &tracks[..8],
        &tracks,
        &model,
        gw_sq,
        14,
        false,
    )
    .0;
    let ser = t.elapsed().as_secs_f64();
    println!(
        "serial 8 rows x {n}: {ser:.2} s = {:.1} ms per row (incl. selection and bonus builds; {})",
        ser / 8.0 * 1e3,
        one.k
    );
}
