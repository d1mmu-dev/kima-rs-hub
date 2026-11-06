//! Task 9 benchmark (owner decision Q7): where the first layout spends its time, stage by stage, on synthetic 512-d
//! libraries shaped like `neighbour_bench` (10 tracks per artist, genres on).
//!
//!   cargo run --release -p kima-vibe --features parallel --example layout_bench
//!
//! Environment: `N` tracks (default 20000), `THREADS` (default 8; 0 = rayon's default), `STAGE` = `sweep` (compute
//! the neighbour lists and cache them in `CACHE_DIR`), `layout` (load them and time everything after the sweep) or
//! `all` (default, both in one process). The cache lets the 100k run be split into bounded chunks.
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use kima_vibe::colour::colour_field;
use kima_vibe::formula::{LibraryArtist, MatchTrack, build_genre_model, unit};
use kima_vibe::map_compute::{library_graph, library_start};
use kima_vibe::neighbours::{NearestColumns, Sweep, nearest_columns_with};
use kima_vibe::umap::{MAP_NEIGHBOURS, SEED, fuzzy_graph, layout, layout_epochs};

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

fn peak_rss_mb() -> f64 {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok())
        .map_or(f64::NAN, |kb| kb / 1024.0)
}

fn main() {
    let n: usize = env("N", 20_000);
    let threads: usize = env("THREADS", 8);
    let stage: String = env("STAGE", "all".to_string());
    let cache = PathBuf::from(env(
        "CACHE_DIR",
        format!("{}/.hermes/cache/scratch", std::env::var("HOME").unwrap()),
    ))
    .join(format!("layout_bench_{n}.bin"));
    if threads > 0 {
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
    let mut feel_z = Vec::with_capacity(n * 2);
    let tracks: Vec<MatchTrack> = (0..n)
        .map(|i| {
            let a = i / 10;
            let c = &centres[a % centres.len()];
            let f = [rng.next(), rng.next()];
            feel_z.extend_from_slice(&f);
            MatchTrack {
                track_id: format!("{i:08}"),
                album_id: format!("al{}-{}", a, (i % 10) / 5),
                artist_id: format!("ar{a:06}"),
                sound: unit(&c.iter().map(|x| x + 0.8 * rng.next()).collect::<Vec<_>>()),
                feel: unit(&f).try_into().unwrap(),
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
    let gw = 0.25;
    println!(
        "n={n} threads={threads} stage={stage} epochs={}",
        layout_epochs(n)
    );

    if stage == "sweep" || stage == "all" {
        let t = Instant::now();
        let (near, _) = nearest_columns_with(
            Sweep {
                accumulation: Default::default(),
                parallel: true,
            },
            &tracks,
            &tracks,
            &model,
            gw * gw,
            MAP_NEIGHBOURS - 1,
            true,
        );
        println!(
            "1 neighbour sweep           {:8.1} s",
            t.elapsed().as_secs_f64()
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(near.k as u64).to_le_bytes());
        bytes.extend_from_slice(&near.max_score.to_le_bytes());
        for i in &near.idx {
            bytes.extend_from_slice(&i.to_le_bytes());
        }
        for s in &near.score {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        fs::write(&cache, bytes).unwrap();
        println!(
            "   cached {} ({:.0} MB peak RSS)",
            cache.display(),
            peak_rss_mb()
        );
    }
    if stage == "layout" || stage == "all" {
        let bytes = fs::read(&cache).expect("run STAGE=sweep first");
        let k = u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize;
        let max_score = f64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let mut at = 16;
        let idx: Vec<u32> = bytes[at..at + 4 * n * k]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        at += 4 * n * k;
        let score: Vec<f64> = bytes[at..at + 8 * n * k]
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let near = NearestColumns {
            k,
            idx,
            score,
            max_score,
        };
        let graph = library_graph(n, &near);

        let t = Instant::now();
        let start = library_start(&tracks, &model, gw, SEED);
        println!(
            "2 PCA start (13-wide)       {:8.1} s",
            t.elapsed().as_secs_f64()
        );

        let t = Instant::now();
        let edges = fuzzy_graph(&graph, layout_epochs(n));
        let fuzzy = t.elapsed().as_secs_f64();
        println!(
            "3 fuzzy graph               {fuzzy:8.1} s  ({} edges)",
            edges.head.len()
        );

        let t = Instant::now();
        let pos = layout(&graph, &start, SEED);
        let both = t.elapsed().as_secs_f64();
        println!(
            "4 optimise {} epochs         {:8.1} s  (layout incl. its own fuzzy graph {both:.1} s)",
            layout_epochs(n),
            both - fuzzy
        );

        let t = Instant::now();
        let (_, colour) = colour_field(&pos, n, &feel_z);
        println!(
            "5 colour field              {:8.1} s  ({} values)",
            t.elapsed().as_secs_f64(),
            colour.len()
        );
        println!("peak RSS {:.0} MB", peak_rss_mb());
    }
}
