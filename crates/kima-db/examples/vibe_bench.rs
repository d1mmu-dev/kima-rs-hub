//! Task 6 measurements on synthetic rows (not a CI test): exact top-20 latency at 25k/50k/100k/200k vectors and the
//! ranking queries at 100k. Run via `tools/vibe-bench.sh`, which owns the Postgres container.
//!
//! `BENCH_DATABASE_URL` must name a database called `m5bench` on 127.0.0.1; the example creates its tables with the
//! migrations, drops the HNSW index (exact scans never use it and 200k incremental inserts into it would dominate
//! the run) and fills the library with random 512-d vectors, 5 % of the tracks marked missing.

use std::time::{Duration, Instant};

use kima_db::vibe::path::{PathOptions, generate_song_path};
use kima_db::vibe::rank::{
    WeightedInput, matching_context, rank_matches, score_library, top_scores,
};
use kima_db::vibe::state::build_matching;
use kima_db::vibe::{ELIGIBLE, MATCHING_SOUND};
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use sqlx::PgPool;
use uuid::Uuid;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as f64 / (1u64 << 31) as f64 * 2.0 - 1.0
    }
}

fn axis(rng: &mut Rng, scale: f64, bias: f64) -> ProbeAxis {
    ProbeAxis {
        alpha: 1.0,
        bias,
        weights: (0..512).map(|_| rng.next() * scale).collect(),
    }
}

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    let at = |q: f64| v[((v.len() as f64 * q).ceil() as usize).clamp(1, v.len()) - 1];
    format!(
        "n={} median {:.0} ms  p95 {:.0} ms  max {:.0} ms",
        v.len(),
        at(0.5).as_secs_f64() * 1e3,
        at(0.95).as_secs_f64() * 1e3,
        at(1.0).as_secs_f64() * 1e3
    )
}

/// Adds `tracks` tracks (10 per artist, 2 albums of 5) with random unit vectors; 5 % are marked missing.
async fn grow(pool: &PgPool, from_artist: i64, tracks: i64) -> sqlx::Result<()> {
    let artists = tracks / 10;
    let (lo, hi) = (from_artist + 1, from_artist + artists);
    sqlx::query("INSERT INTO artists (id, name, name_key) SELECT gen_random_uuid(), 'a' || g, 'a' || g FROM generate_series($1::bigint, $2::bigint) g")
        .bind(lo).bind(hi).execute(pool).await?;
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) SELECT gen_random_uuid(), a.id, 't', a.name_key || '-' || k FROM artists a JOIN generate_series($1::bigint, $2::bigint) g ON a.name_key = 'a' || g, generate_series(1, 2) k")
        .bind(lo).bind(hi).execute(pool).await?;
    sqlx::query("INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, missing_since) SELECT gen_random_uuid(), al.id, 'a', 't', 'flac', 'p' || gen_random_uuid()::text, 1, now(), '\\x00', CASE WHEN random() < 0.05 THEN now() END FROM albums al JOIN artists a ON a.id = al.artist_id JOIN generate_series($1::bigint, $2::bigint) g ON a.name_key = 'a' || g, generate_series(1, 5) k")
        .bind(lo).bind(hi).execute(pool).await?;
    let id = MATCHING_SOUND;
    let sha: Vec<u8> = id.model_sha256.to_vec();
    sqlx::query("INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding) SELECT t.id, 0, $1, $2, $3, l2_normalize((SELECT array_agg(random() - 0.5) FROM generate_series(1, 512) s WHERE t.id IS NOT NULL)::real[]::vector) FROM tracks t WHERE NOT EXISTS (SELECT 1 FROM track_embeddings e WHERE e.track_id = t.id)")
        .bind(id.model_id).bind(sha).bind(id.pipeline_version).execute(pool).await?;
    sqlx::query("ANALYZE").execute(pool).await?;
    Ok(())
}

async fn exact_search(pool: &PgPool, rng: &mut Rng, runs: usize) -> sqlx::Result<Vec<Duration>> {
    let eligible: &str = &ELIGIBLE;
    let sql = format!(
        "SELECT te.track_id FROM {eligible} ORDER BY (te.embedding <=> $1::real[]::vector) + 0, te.track_id LIMIT 20"
    );
    let mut out = vec![];
    for i in 0..runs + 2 {
        let q: Vec<f32> = (0..512).map(|_| rng.next() as f32).collect();
        let t = Instant::now();
        let rows = sqlx::query(&sql).bind(q).fetch_all(pool).await?;
        assert_eq!(rows.len(), 20);
        if i >= 2 {
            out.push(t.elapsed()); // the first two runs warm the cache
        }
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> sqlx::Result<()> {
    let url = std::env::var("BENCH_DATABASE_URL").expect("BENCH_DATABASE_URL");
    assert!(
        url.contains("@127.0.0.1:") && url.ends_with("/m5bench"),
        "refusing a non-bench database"
    );
    let pool = kima_db::connect(&url).await?;
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await?;
    assert_eq!(db, "m5bench");
    kima_db::MIGRATOR.run(&pool).await?;
    sqlx::query("DROP INDEX IF EXISTS track_embeddings_cosine_idx")
        .execute(&pool)
        .await?;
    for (name, q) in [
        ("shared_buffers", "SHOW shared_buffers"),
        (
            "max_parallel_workers_per_gather",
            "SHOW max_parallel_workers_per_gather",
        ),
        (
            "pgvector",
            "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
        ),
    ] {
        let v: String = sqlx::query_scalar(q).fetch_one(&pool).await?;
        println!("{name} = {v}");
    }
    println!(
        "cpus = {}",
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );

    let mut rng = Rng(42);
    let mut have = 0_i64;
    let mut table = vec![];
    for target in [25_000_i64, 50_000, 100_000, 200_000] {
        let t = Instant::now();
        grow(&pool, have / 10, target - have).await?;
        have = target;
        println!(
            "-- seeded {target} tracks in {:.0} s",
            t.elapsed().as_secs_f64()
        );
        let eligible: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}", &*ELIGIBLE))
            .fetch_one(&pool)
            .await?;
        let s = stats(exact_search(&pool, &mut rng, 30).await?);
        println!("exact top-20 @ {target} ({eligible} eligible): {s}");
        table.push((target, eligible, s));

        if target == 100_000 {
            let probe = FeelProbe {
                model: "synthetic".into(),
                valence: axis(&mut rng, 0.05, 0.5),
                arousal: axis(&mut rng, 0.05, 0.5),
                probe_id: "bench0123456789a".into(),
            };
            let t = Instant::now();
            let outcome = build_matching(&pool, &probe).await?;
            println!(
                "build_matching @100k: {:.1} s ({outcome:?})",
                t.elapsed().as_secs_f64()
            );
            let t = Instant::now();
            let outcome = build_matching(&pool, &probe).await?;
            println!(
                "build_matching again (nothing changed) @100k: {:.1} s ({outcome:?})",
                t.elapsed().as_secs_f64()
            );

            let seeds: Vec<Uuid> = sqlx::query_scalar(&format!(
                "SELECT te.track_id FROM {} ORDER BY te.track_id LIMIT 24",
                &*ELIGIBLE
            ))
            .fetch_all(&pool)
            .await?;
            let ctx = matching_context(&pool, &probe, &seeds)
                .await?
                .expect("state");
            let (mut rank, mut top) = (vec![], vec![]);
            for (i, seed) in seeds.iter().enumerate() {
                let inputs = [WeightedInput {
                    track_id: *seed,
                    weight: 1.0,
                }];
                let t = Instant::now();
                let r = rank_matches(&pool, &ctx, &inputs, 20, &[*seed])
                    .await
                    .expect("rank");
                let d = t.elapsed();
                let t2 = Instant::now();
                top_scores(&pool, &ctx, &inputs, 20, &[*seed])
                    .await
                    .expect("top");
                if i >= 4 {
                    print!("{:.0} ", d.as_secs_f64() * 1e3);
                    rank.push(d);
                    top.push(t2.elapsed());
                }
                assert_eq!(r.len(), 20);
            }
            println!("(ms per seed)");
            println!(
                "rank_matches @100k (20 seeds, 4 warm-up skipped): {}",
                stats(rank)
            );
            println!("top_scores   @100k: {}", stats(top));
            let mut path = vec![];
            for i in 0..5 {
                let sets = [
                    vec![WeightedInput {
                        track_id: seeds[i],
                        weight: 1.0,
                    }],
                    vec![WeightedInput {
                        track_id: seeds[i + 5],
                        weight: 1.0,
                    }],
                ];
                let t = Instant::now();
                let rows = score_library(&pool, &ctx, &sets, &[seeds[i], seeds[i + 5]])
                    .await
                    .expect("score");
                path.push(t.elapsed());
                assert!(rows.len() > 90_000);
            }
            println!(
                "score_library two sets @100k (path scoring, whole library returned): {}",
                stats(path)
            );
            let mut whole = vec![];
            for i in 0..5 {
                let t = Instant::now();
                let p = generate_song_path(
                    &pool,
                    &probe,
                    seeds[i],
                    seeds[i + 5],
                    PathOptions::default(),
                    |_| 0,
                )
                .await
                .expect("path");
                whole.push(t.elapsed());
                assert!(p.path.len() >= 8);
            }
            println!(
                "generate_song_path (smooth, default length) @100k: {}",
                stats(whole)
            );
        }
    }
    println!("\n| tracks | eligible | exact top-20 |\n|---|---|---|");
    for (n, e, s) in table {
        println!("| {n} | {e} | {s} |");
    }
    Ok(())
}
