//! The coordinator's library-wide M5 phases (Task 12, spec 9.1): the learned moods, then the similarity step (feel,
//! matching state, vibe map), after the per-track phases and only once embedding and analysis have settled; a failing
//! pass never stops the cycle; the re-run matrix is upstream's `STEP_CLEARS`; the "moods changed" event. Synthetic
//! heads and probe (the real ones are a private asset, owner decision Q3); the database is real.

mod common;

use std::sync::Arc;

use kima_db::PgPool;
use kima_db::enrichment::{self, Clears};
use kima_db::vibe::MATCHING_SOUND;
use kima_ident::IdentCtx;
use kima_ident::coordinator::control::{self, Step};
use kima_ident::coordinator::{self, CycleReport};
use kima_ident::stages::moods;
use kima_ident::stages::similarity::{self, SimilarityPass};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_ml::CapabilityEvaluationReport;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use tokio::sync::broadcast::error::TryRecvError;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn heads() -> Arc<MoodHeads> {
    Arc::new(MoodHeads {
        version: "cycle-heads".into(),
        model: "synthetic".into(),
        dims: 512,
        heads: (0..MOODS.len())
            .map(|k| MoodHead {
                weights: (0..512)
                    .map(|i| (((i * 7 + k * 13) % 17) as f64 / 17.0 - 0.5) * 0.3)
                    .collect(),
                bias: k as f64 * 0.05 - 0.5,
            })
            .collect(),
    })
}

fn probe() -> Arc<FeelProbe> {
    let axis = |scale: f64| ProbeAxis {
        alpha: 1.0,
        bias: 0.5,
        weights: (0..512)
            .map(|i| (((i * 7) % 13) as f64 / 13.0 - 0.5) * scale)
            .collect(),
    };
    Arc::new(FeelProbe {
        model: "synthetic".into(),
        valence: axis(0.02),
        arousal: axis(-0.03),
        probe_id: "cycle-probe".into(),
    })
}

fn assets(probe: Option<Arc<FeelProbe>>, heads: Option<Arc<MoodHeads>>) -> VibeAssets {
    VibeAssets {
        probe,
        heads,
        report: CapabilityEvaluationReport {
            capabilities: Default::default(),
        },
    }
}

/// A context whose per-track phases have nothing to do (fingerprinting off, the network dead) and both vibe assets.
fn ctx(pool: &PgPool) -> (IdentCtx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut ctx = common::ident_ctx(pool, &root, common::dead_bases());
    ctx.config.fingerprinting = false;
    vibe_assets::install(&ctx, assets(Some(probe()), Some(heads())));
    (ctx, dir)
}

async fn stage(pool: &PgPool, t: Uuid, stage: &str, status: &str, next: &str) {
    sqlx::query(&format!(
        "INSERT INTO track_stages (track_id, stage, status, next_attempt_at, error_class, started_at)
         VALUES ($1, $2, $3, {next}, CASE WHEN $3 = 'failed' THEN 'INCONCLUSIVE' END, now() - interval '1 hour')
         ON CONFLICT (track_id, stage) DO UPDATE SET status = $3, next_attempt_at = {next},
             error_class = CASE WHEN $3 = 'failed' THEN 'INCONCLUSIVE' END"
    ))
    .bind(t)
    .bind(stage)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

/// A present track with a vector under the engine's checkpoint and its per-track stages settled.
async fn embedded(pool: &PgPool, seed: usize) -> Uuid {
    let id = common::track(pool, &Uuid::new_v4().to_string(), 30).await;
    let mut v: Vec<f32> = (0..512)
        .map(|i| (((i * 31 + seed * 17) % 29) as f32 / 29.0) - 0.5)
        .collect();
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    v.iter_mut().for_each(|a| *a /= n);
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         VALUES ($1, 0, $2, $3, $4, $5::real[]::vector)",
    )
    .bind(id)
    .bind(MATCHING_SOUND.model_id)
    .bind(MATCHING_SOUND.model_sha256.to_vec())
    .bind(MATCHING_SOUND.pipeline_version)
    .bind(v)
    .execute(pool)
    .await
    .unwrap();
    for s in ["recording", "embedding", "analysis"] {
        stage(pool, id, s, "done", "NULL").await;
    }
    id
}

async fn library(pool: &PgPool, n: usize) -> Vec<Uuid> {
    let mut ids = vec![];
    for i in 0..n {
        ids.push(embedded(pool, i + 1).await);
    }
    ids
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn cycle(ctx: &IdentCtx) -> CycleReport {
    *ctx.last_cycle.lock().unwrap() = None;
    coordinator::run_cycle(ctx, &CancellationToken::new())
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cycle_runs_mood_then_similarity_after_embedding(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    library(&pool, 4).await;

    let report = cycle(&ctx).await;
    assert!(report.ran);
    assert_eq!(report.errors, 0);
    assert_eq!(report.moods.claimed, 4, "every vector scored");
    assert_eq!(
        report.similarity.claimed, 4,
        "every track got feel and a place"
    );
    assert_eq!(count(&pool, "track_moods").await, 4);
    assert_eq!(count(&pool, "mood_state").await, 1);
    assert_eq!(count(&pool, "matching_feel").await, 4);
    assert_eq!(count(&pool, "matching_state").await, 1);
    assert_eq!(count(&pool, "matching_map").await, 4);
    // The moods ran first: their statistics are older than the matching state.
    let in_order: bool = sqlx::query_scalar(
        "SELECT (SELECT computed_at FROM mood_state) < (SELECT built_at FROM matching_state)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(in_order, "mood before similarity");
    assert_eq!(report.next, Some(coordinator::NEXT_AFTER_WORK));

    // Nothing changed: both passes find nothing to do and the cycle goes idle.
    let again = cycle(&ctx).await;
    assert_eq!((again.moods.claimed, again.similarity.claimed), (0, 0));
    assert_eq!(again.next, Some(coordinator::NEXT_WHEN_IDLE));
    assert_eq!(count(&pool, "matching_map").await, 4);

    // The progress the route answers counts both steps done, so the pipeline is complete.
    let vibe = similarity::vibe_progress(&ctx).await.unwrap();
    assert_eq!((vibe.moods, vibe.similarity), (Some((4, 4)), Some((4, 4))));
    let p = enrichment::progress_with(&pool, false, vibe).await.unwrap();
    assert!(p.is_fully_complete);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_and_similarity_wait_until_embedding_settled(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let ids = library(&pool, 3).await;
    // One vector is still owed (a retry is scheduled): neither step runs.
    stage(
        &pool,
        ids[2],
        "embedding",
        "failed",
        "now() + interval '1 hour'",
    )
    .await;
    let report = cycle(&ctx).await;
    assert_eq!((report.moods.claimed, report.similarity.claimed), (0, 0));
    assert_eq!(report.errors, 0);
    for table in [
        "track_moods",
        "mood_state",
        "matching_state",
        "matching_map",
    ] {
        assert_eq!(count(&pool, table).await, 0, "{table}");
    }
    // The same for an analysis still owed.
    stage(&pool, ids[2], "embedding", "done", "NULL").await;
    stage(&pool, ids[1], "analysis", "pending", "NULL").await;
    let report = cycle(&ctx).await;
    assert_eq!((report.moods.claimed, report.similarity.claimed), (0, 0));

    // Settled: both run.
    stage(&pool, ids[1], "analysis", "done", "NULL").await;
    let report = cycle(&ctx).await;
    assert_eq!((report.moods.claimed, report.similarity.claimed), (3, 3));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_first_layout_waits_for_the_artists(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let ids = library(&pool, 3).await;
    // An artist still to answer: feel and state are built, the permanent layout waits for its genres.
    sqlx::query(
        "UPDATE artists SET enrichment_status = 'pending'
         WHERE id = (SELECT al.artist_id FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.id = $1)",
    )
    .bind(ids[0])
    .execute(&pool)
    .await
    .unwrap();
    // (The network is dead: the artist is not answered in this cycle.)
    similarity::run_pass(&ctx, false).await.unwrap();
    assert_eq!(count(&pool, "matching_state").await, 1);
    assert_eq!(count(&pool, "matching_map").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_failing_mood_pass_does_not_stop_the_cycle(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    library(&pool, 3).await;
    // The mood pass cannot write its scores.
    sqlx::query("ALTER TABLE track_moods RENAME TO track_moods_away")
        .execute(&pool)
        .await
        .unwrap();

    let report = cycle(&ctx).await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.moods.claimed, 0);
    assert_eq!(report.similarity.claimed, 3, "similarity ran all the same");
    assert_eq!(count(&pool, "matching_map").await, 3);
    let st = enrichment::state(&pool).await.unwrap();
    assert!(
        st.last_error
            .as_deref()
            .unwrap_or("")
            .contains("track_moods"),
        "the failure is recorded: {:?}",
        st.last_error
    );
    assert!(report.next.is_some(), "the next cycle is queued");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_unavailable_asset_turns_off_only_its_own_step(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    library(&pool, 2).await;
    // No probe: moods run, similarity is off without an error.
    vibe_assets::install(&ctx, assets(None, Some(heads())));
    assert_eq!(
        similarity::run_pass(&ctx, true).await.unwrap(),
        SimilarityPass::Unavailable
    );
    let report = cycle(&ctx).await;
    assert_eq!(report.errors, 0);
    assert_eq!((report.moods.claimed, report.similarity.claimed), (2, 0));
    assert_eq!(count(&pool, "matching_state").await, 0);
    let vibe = similarity::vibe_progress(&ctx).await.unwrap();
    assert_eq!((vibe.moods, vibe.similarity), (Some((2, 2)), None));

    // No heads: the reverse.
    vibe_assets::install(&ctx, assets(Some(probe()), None));
    let report = cycle(&ctx).await;
    assert_eq!(report.errors, 0);
    assert_eq!((report.moods.claimed, report.similarity.claimed), (0, 2));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn moods_changed_is_announced_when_scores_or_statistics_change(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    library(&pool, 2).await;
    let mut changes = moods::subscribe_changes(&ctx)
        .await
        .expect("heads installed");
    moods::run_pass(&ctx).await.unwrap();
    assert_eq!(changes.try_recv(), Ok(()));
    // Nothing changed: no event.
    moods::run_pass(&ctx).await.unwrap();
    assert_eq!(changes.try_recv(), Err(TryRecvError::Empty));
    // A new vector is scored: one event.
    embedded(&pool, 9).await;
    moods::run_pass(&ctx).await.unwrap();
    assert_eq!(changes.try_recv(), Ok(()));

    vibe_assets::install(&ctx, assets(Some(probe()), None));
    assert!(moods::subscribe_changes(&ctx).await.is_none());
}

/// Upstream's `STEP_CLEARS` (`services/enrichmentRestart.ts:34-52`, v1.12.3), copied as a table:
/// `(similarity, moods, recording, artists, analysis, vectors)`.
#[test]
fn rerun_matrix_equals_upstream_step_clears() {
    let upstream = [
        ("identification", (true, true, true, true, true, true)),
        ("artists", (true, false, false, true, false, false)),
        ("analysis", (true, true, false, false, true, true)),
        ("vectors", (true, true, false, false, false, true)),
        ("moods", (false, true, false, false, false, false)),
        ("similarity", (true, false, false, false, false, false)),
    ];
    assert_eq!(Step::ALL.len(), upstream.len());
    for (id, (similarity, moods, recording, artists, analysis, vectors)) in upstream {
        let step = Step::parse(id).unwrap();
        assert_eq!(
            step.clears(),
            Clears {
                similarity,
                moods,
                recording,
                artists,
                analysis,
                vectors,
            },
            "{id}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rerun_from_each_step_through_control(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    library(&pool, 3).await;
    cycle(&ctx).await;
    let tables = [
        "track_moods",
        "mood_state",
        "matching_feel",
        "matching_state",
        "matching_map",
        "track_embeddings",
    ];
    // (step, rows left in `tables` right after the re-run, vectors reported)
    let cases: [(Step, [i64; 6], i64); 4] = [
        (Step::Moods, [0, 0, 3, 1, 3, 3], 0),
        (Step::Similarity, [3, 1, 0, 0, 0, 3], 0),
        (Step::Artists, [3, 1, 0, 0, 0, 3], 0),
        (Step::Vectors, [0, 0, 0, 0, 0, 0], 3),
    ];
    for (step, left, vectors) in cases {
        let counts = control::rerun(&pool, step).await.unwrap();
        assert_eq!(counts.vectors, vectors, "{step:?}");
        let mut got = [0; 6];
        for (i, t) in tables.iter().enumerate() {
            got[i] = count(&pool, t).await;
        }
        assert_eq!(got, left, "{step:?}");
        if step == Step::Artists {
            // The artist network pipeline is exercised elsewhere (artist.rs); with the test's
            // bases dead it cannot resolve within one cycle (RETRY_CAP=4 failures before
            // `unresolvable`). Simulate its completion so the genre-settled gate this test
            // actually cares about (the similarity layout) can be exercised.
            sqlx::query(
                "UPDATE artists SET enrichment_status = 'completed' WHERE name_key <> 'various artists'",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        // The next cycle builds everything again (the vectors themselves come back only from the analyzer).
        if step != Step::Vectors {
            let report = cycle(&ctx).await;
            assert_eq!(report.errors, 0, "{step:?}");
            assert_eq!(count(&pool, "matching_map").await, 3, "{step:?}");
            assert_eq!(count(&pool, "track_moods").await, 3, "{step:?}");
        }
    }
    // The re-run kicked the coordinator.
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'enrich' AND state = 'queued'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(queued >= 1);
}
