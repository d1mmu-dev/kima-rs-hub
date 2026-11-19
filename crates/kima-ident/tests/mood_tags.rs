//! The mood step writes the mood tags (M5b Task 5): after the scores and statistics, every covered track gets the live
//! moods it sits at least 1.25 SD above the library's mean in; without mood heads nothing is read or written (the live
//! degradation, M5 Q3). Synthetic heads; the database is real.

mod common;

use std::sync::Arc;

use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use kima_ident::IdentCtx;
use kima_ident::stages::moods::{self, MoodPass};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_ml::CapabilityEvaluationReport;
use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
use uuid::Uuid;

/// Heads that score each track along one axis per mood, so a track's scores are its vector's first 22 entries.
fn heads() -> Arc<MoodHeads> {
    Arc::new(MoodHeads {
        version: "tags-v1".into(),
        model: "synthetic".into(),
        dims: 512,
        heads: (0..MOODS.len())
            .map(|k| MoodHead {
                weights: (0..512).map(|i| if i == k { 1.0 } else { 0.0 }).collect(),
                bias: 0.0,
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

/// A present track whose unit vector carries `happy` (before normalising) on the happy axis and 1 on the last axis.
async fn track(pool: &PgPool, happy: f32) -> Uuid {
    let id = common::track(pool, &Uuid::new_v4().to_string(), 30).await;
    let k = MOODS.iter().position(|m| *m == "happy").unwrap();
    let mut v = vec![0.0_f32; 512];
    v[k] = happy;
    v[511] = 1.0;
    let norm = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    v.iter_mut().for_each(|a| *a /= norm);
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

async fn tags(pool: &PgPool, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT mood_tags FROM tracks WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_mood_pass_writes_the_tags(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    // Ten tracks, one far above the others on happy.
    let mut ids = Vec::new();
    for i in 0..9 {
        ids.push(track(&pool, i as f32 * 0.01).await);
    }
    let top = track(&pool, 5.0).await;
    vibe_assets::install(&ctx, assets(Some(heads())));
    assert!(matches!(
        moods::run_pass(&ctx).await.unwrap(),
        MoodPass::Ran { .. }
    ));
    assert_eq!(tags(&pool, top).await, ["happy"]);
    for id in ids {
        assert!(tags(&pool, id).await.is_empty());
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_heads_no_tags_written(pool: PgPool) {
    let (ctx, _dir) = ctx(&pool);
    let id = track(&pool, 5.0).await;
    sqlx::query("UPDATE tracks SET mood_tags = '{calm}' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    vibe_assets::install(&ctx, assets(None));
    assert_eq!(moods::run_pass(&ctx).await.unwrap(), MoodPass::Unavailable);
    assert_eq!(tags(&pool, id).await, ["calm"]);
}
