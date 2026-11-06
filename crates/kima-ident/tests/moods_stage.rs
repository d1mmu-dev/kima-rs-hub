//! The mood step in kima-ident (M5 Task 11): capability gating, one pass, the snapshot worker. Synthetic heads
//! (the real ones are a private asset, owner decision Q3); the database is real.

mod common;

use std::sync::Arc;
use std::time::Duration;

use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use kima_db::vibe::moods::MoodBuild;
use kima_ident::IdentCtx;
use kima_ident::stages::moods::{self, MoodPass};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_ml::CapabilityEvaluationReport;
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn heads(version: &str) -> Arc<MoodHeads> {
    Arc::new(MoodHeads {
        version: version.into(),
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

fn assets(heads: Option<Arc<MoodHeads>>) -> VibeAssets {
    VibeAssets {
        probe: None,
        heads,
        report: CapabilityEvaluationReport {
            capabilities: Default::default(),
        },
    }
}

fn ctx(pool: &PgPool) -> (IdentCtx, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (common::ident_ctx(pool, &root, common::dead_bases()), dir)
}

/// A present track with a vector under the engine's checkpoint.
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
    id
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn moods_off_when_heads_unavailable(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    embedded(&pool, 1).await;
    // The default config has no asset directory: the capability is unavailable.
    assert_eq!(moods::run_pass(&ctx).await.unwrap(), MoodPass::Unavailable);
    assert!(moods::runtime(&ctx).await.is_none());
    assert!(moods::snapshot(&ctx).is_none());
    assert_eq!(count(&pool, "track_moods").await, 0);
    assert_eq!(count(&pool, "mood_state").await, 0);

    // Heads missing while the probe is there (or the reverse) is the same: moods only need their own asset.
    vibe_assets::install(&ctx, assets(None));
    assert_eq!(moods::run_pass(&ctx).await.unwrap(), MoodPass::Unavailable);
    assert_eq!(count(&pool, "track_moods").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_pass_scores_builds_the_statistics_and_the_snapshot(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let ids = [
        embedded(&pool, 1).await,
        embedded(&pool, 2).await,
        embedded(&pool, 3).await,
    ];
    vibe_assets::install(&ctx, assets(Some(heads("synthetic-v1"))));

    assert_eq!(
        moods::run_pass(&ctx).await.unwrap(),
        MoodPass::Ran {
            build: MoodBuild {
                scored: 3,
                state_changed: true
            },
            snapshot_changed: true
        }
    );
    let snap = moods::snapshot(&ctx).expect("the pass leaves a snapshot");
    assert_eq!((snap.views.len(), snap.pending), (3, 0));
    assert!(ids.iter().all(|id| snap.views.contains_key(id)));
    let version: String = sqlx::query_scalar("SELECT version FROM mood_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, "2/synthetic-v1");

    // Nothing changed: nothing written, the snapshot stands.
    assert_eq!(
        moods::run_pass(&ctx).await.unwrap(),
        MoodPass::Ran {
            build: MoodBuild::default(),
            snapshot_changed: false
        }
    );
    // New heads rescore every track (installing them starts a new runtime).
    vibe_assets::install(&ctx, assets(Some(heads("synthetic-v2"))));
    let MoodPass::Ran {
        build,
        snapshot_changed,
    } = moods::run_pass(&ctx).await.unwrap()
    else {
        panic!("heads are installed");
    };
    assert_eq!(build.scored, 3);
    assert!(snapshot_changed);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_snapshot_worker_refreshes_each_interval_and_stops_on_shutdown(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    embedded(&pool, 1).await;
    embedded(&pool, 2).await;
    vibe_assets::install(&ctx, assets(Some(heads("synthetic-v1"))));
    // Another process (or a re-run) wrote the statistics; this process has no snapshot yet.
    let rt = moods::runtime(&ctx).await.unwrap();
    rt.builder.build(&pool).await.unwrap();
    assert!(moods::snapshot(&ctx).is_none());

    let stop = CancellationToken::new();
    let worker = moods::spawn_refresher_every(ctx.clone(), stop.clone(), Duration::from_millis(50));
    let mut waited = 0;
    while moods::snapshot(&ctx).is_none() {
        assert!(waited < 100, "the snapshot appears within a few intervals");
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += 1;
    }
    assert_eq!(moods::snapshot(&ctx).unwrap().views.len(), 2);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the worker stops")
        .unwrap();
}
