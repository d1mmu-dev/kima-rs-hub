//! The re-run's clears (M5 Task 12, spec 9.5), against a real Postgres: what each combination of `Clears` removes,
//! what it leaves, and the lock order (the matching state row first).
mod vibe_common;
use kima_db::PgPool;
use kima_db::enrichment::{Clears, rerun_clears};
use kima_db::vibe::moods::MoodBuilder;
use kima_db::vibe::state::build_matching;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use vibe_common::*;

const NONE: Clears = Clears {
    similarity: false,
    moods: false,
    recording: false,
    artists: false,
    analysis: false,
    vectors: false,
};

fn vector(seed: u64) -> Vec<f32> {
    let mut v: Vec<f32> = (0..512)
        .map(|i| (((i as u64 * 31 + seed * 17) % 29) as f32 / 29.0) - 0.5)
        .collect();
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    v.iter_mut().for_each(|a| *a /= n);
    v
}

fn heads() -> Arc<MoodHeads> {
    Arc::new(MoodHeads {
        version: "rerun-heads".into(),
        model: "synthetic".into(),
        dims: 512,
        heads: (0..MOODS.len())
            .map(|k| MoodHead {
                weights: (0..512)
                    .map(|i| (((i * 7 + k * 13) % 17) as f64 / 17.0 - 0.5) * 0.3)
                    .collect(),
                bias: 0.0,
            })
            .collect(),
    })
}

fn probe() -> FeelProbe {
    let axis = |scale: f64| ProbeAxis {
        alpha: 1.0,
        bias: 0.5,
        weights: (0..512)
            .map(|i| (((i * 7) % 13) as f64 / 13.0 - 0.5) * scale)
            .collect(),
    };
    FeelProbe {
        model: "synthetic".into(),
        valence: axis(0.02),
        arousal: axis(-0.03),
        probe_id: "rerun-probe".into(),
    }
}

async fn stage(pool: &PgPool, track: Uuid, stage: &str, status: &str) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, redelivered, last_error, error_code,
                                   error_class, evidence, started_at, finished_at)
         VALUES ($1, $2, $3, 2, 1, 'boom', 'HTTP_503', NULL, '{\"x\":1}'::jsonb, now(), now())
         ON CONFLICT (track_id, stage) DO UPDATE SET status = $3, attempts = 2, redelivered = 1,
             last_error = 'boom', error_code = 'HTTP_503', evidence = '{\"x\":1}'::jsonb,
             started_at = now(), finished_at = now()",
    )
    .bind(track)
    .bind(stage)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

/// `n` tracks with a vector, stage rows, a stored analysis, scores and statistics, feel and state, and a map.
async fn full_library(pool: &PgPool, n: usize) -> Vec<Uuid> {
    let mut ids = vec![];
    for i in 0..n {
        let s = track(pool).await;
        embed(pool, s.track, &vector(i as u64 + 1)).await;
        stage(pool, s.track, "analysis", "done").await;
        stage(pool, s.track, "embedding", "done").await;
        sqlx::query("INSERT INTO track_analysis (track_id, file_generation) VALUES ($1, 0)")
            .bind(s.track)
            .execute(pool)
            .await
            .unwrap();
        ids.push(s.track);
    }
    MoodBuilder::new(heads()).build(pool).await.unwrap();
    build_matching(pool, &probe()).await.unwrap();
    sqlx::query(
        "INSERT INTO matching_map (track_id, embedded_at, x, y, z, colour, laid_out)
         SELECT track_id, created_at, 0, 0, 0, 0.5, true FROM track_embeddings",
    )
    .execute(pool)
    .await
    .unwrap();
    ids
}

struct Rows {
    moods: i64,
    mood_state: i64,
    feel: i64,
    state: i64,
    map: i64,
    vectors: i64,
    analysis: i64,
}

async fn rows(pool: &PgPool) -> Rows {
    Rows {
        moods: count(pool, "track_moods").await,
        mood_state: count(pool, "mood_state").await,
        feel: count(pool, "matching_feel").await,
        state: count(pool, "matching_state").await,
        map: count(pool, "matching_map").await,
        vectors: count(pool, "track_embeddings").await,
        analysis: count(pool, "track_analysis").await,
    }
}

async fn stage_row(pool: &PgPool, t: Uuid, stage: &str) -> (String, i16, Option<String>, bool) {
    sqlx::query_as(
        "SELECT status, attempts, last_error, evidence IS NULL FROM track_stages WHERE track_id = $1 AND stage = $2",
    )
    .bind(t)
    .bind(stage)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_moods_clears_mood_state_only(pool: PgPool) {
    assert!(current_database(&pool).await.starts_with("_sqlx_test"));
    let ids = full_library(&pool, 4).await;
    let before = rows(&pool).await;
    assert_eq!(
        (before.moods, before.mood_state, before.state, before.map),
        (4, 1, 1, 4)
    );
    let counts = rerun_clears(
        &pool,
        Clears {
            moods: true,
            ..NONE
        },
    )
    .await
    .unwrap();
    let after = rows(&pool).await;
    assert_eq!((after.moods, after.mood_state), (0, 0));
    assert_eq!(
        (
            after.feel,
            after.state,
            after.map,
            after.vectors,
            after.analysis
        ),
        (4, 1, 4, 4, 4)
    );
    assert_eq!(stage_row(&pool, ids[0], "embedding").await.0, "done");
    assert_eq!(counts.vectors, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_similarity_clears_state_feel_map(pool: PgPool) {
    full_library(&pool, 4).await;
    rerun_clears(
        &pool,
        Clears {
            similarity: true,
            ..NONE
        },
    )
    .await
    .unwrap();
    let r = rows(&pool).await;
    assert_eq!((r.state, r.feel, r.map), (0, 0, 0));
    assert_eq!(
        (r.moods, r.mood_state, r.vectors, r.analysis),
        (4, 1, 4, 4),
        "moods and vectors stay"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_vectors_clears_both_and_deletes_vectors(pool: PgPool) {
    let ids = full_library(&pool, 4).await;
    // A track whose file is missing waits for the scan: its stage row is left as it is.
    let gone = track(&pool).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone.track)
        .execute(&pool)
        .await
        .unwrap();

    // (Marking it missing reopened its stage rows through the file-change trigger; close it afterwards.)
    stage(&pool, gone.track, "embedding", "failed").await;
    let c = Clears {
        similarity: true,
        moods: true,
        vectors: true,
        ..NONE
    };
    let counts = rerun_clears(&pool, c).await.unwrap();
    let r = rows(&pool).await;
    assert_eq!(
        (r.moods, r.mood_state, r.feel, r.state, r.map, r.vectors),
        (0, 0, 0, 0, 0, 0)
    );
    assert_eq!(counts.vectors, 4);
    assert_eq!(r.analysis, 4, "the stored analysis is not a vector");
    // The embedding stage is asked again, with no memory of earlier attempts; the analysis stage is untouched.
    assert_eq!(
        stage_row(&pool, ids[0], "embedding").await,
        ("pending".into(), 0, None, true)
    );
    assert_eq!(stage_row(&pool, ids[0], "analysis").await.0, "done");
    assert_eq!(stage_row(&pool, gone.track, "embedding").await.0, "failed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_analysis_clears_analysis_and_what_depends_on_it(pool: PgPool) {
    let ids = full_library(&pool, 3).await;
    let c = Clears {
        similarity: true,
        moods: true,
        analysis: true,
        vectors: true,
        ..NONE
    };
    let counts = rerun_clears(&pool, c).await.unwrap();
    let r = rows(&pool).await;
    assert_eq!(
        (r.analysis, r.vectors, r.moods, r.state, r.map),
        (0, 0, 0, 0, 0)
    );
    assert_eq!((counts.tracks, counts.vectors), (3, 3));
    assert_eq!(
        stage_row(&pool, ids[1], "analysis").await,
        ("pending".into(), 0, None, true)
    );
    assert_eq!(stage_row(&pool, ids[1], "embedding").await.0, "pending");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_with_nothing_to_clear_changes_nothing(pool: PgPool) {
    full_library(&pool, 2).await;
    let counts = rerun_clears(&pool, NONE).await.unwrap();
    assert_eq!((counts.artists, counts.tracks, counts.vectors), (0, 0, 0));
    let r = rows(&pool).await;
    assert_eq!(
        (
            r.moods,
            r.mood_state,
            r.feel,
            r.state,
            r.map,
            r.vectors,
            r.analysis
        ),
        (2, 1, 2, 1, 2, 2, 2)
    );
}

async fn waiting_on_locks(pool: &PgPool, n: i64) {
    for _ in 0..200 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{n} backend(s) never waited on a lock");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_deletes_matching_state_first(pool: PgPool) {
    let ids = full_library(&pool, 3).await;
    // A map row is held, as a map write holds it. The re-run must already have taken the state row when it reaches
    // the map (state row, then map rows): a map write, which takes the state row first, then cannot start.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT track_id FROM matching_map WHERE track_id = $1 FOR UPDATE")
        .bind(ids[0])
        .execute(&mut *holder)
        .await
        .unwrap();
    let rerunning = {
        let pool = pool.clone();
        tokio::spawn(async move {
            rerun_clears(
                &pool,
                Clears {
                    similarity: true,
                    moods: true,
                    ..NONE
                },
            )
            .await
        })
    };
    waiting_on_locks(&pool, 1).await;
    // The state row (deleted by the re-run, uncommitted) and the mood state row are both locked against writers.
    for table in ["matching_state", "mood_state"] {
        let err = sqlx::query(&format!("SELECT id FROM {table} FOR UPDATE NOWAIT"))
            .fetch_all(&pool)
            .await
            .expect_err("the re-run holds the row");
        assert_eq!(
            err.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("55P03"),
            "{table}"
        );
    }
    holder.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), rerunning)
        .await
        .expect("no deadlock")
        .unwrap()
        .unwrap();
    assert_eq!(rows(&pool).await.map, 0);
}
