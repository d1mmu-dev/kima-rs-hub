//! D-194 transition gate (no private model needed): while the music audio capability is not `Available` (disabled,
//! unconfigured, files missing, bytes not matching the pin), neither the transition nor a whole coordinator cycle
//! writes a single `embedding` stage row or vector. The `Available` path runs on the private model in
//! `embedding_e2e.rs` (`old_general_vectors_are_reembedded_on_the_music_checkpoint_once_available`).

mod common;

use std::path::Path;

use kima_core::identify::IdentifySwitch;
use kima_db::{PgPool, ml};
use kima_ident::coordinator;
use kima_ident::stages::embedding::{self, VectorTransition};
use kima_ml::onnx::OnnxModel;
use tokio_util::sync::CancellationToken;

fn general() -> ml::Provenance {
    ml::Provenance {
        model_id: "hf_audio".into(),
        model_sha256: [0x37; 32],
        pipeline_version: "clap-audio-v1".into(),
    }
}

fn music() -> ml::Provenance {
    let id = kima_db::vibe::MATCHING_SOUND;
    ml::Provenance {
        model_id: id.model_id.into(),
        model_sha256: id.model_sha256,
        pipeline_version: id.pipeline_version.into(),
    }
}

fn vector(hot: usize) -> Vec<f32> {
    let mut v = vec![0.; 512];
    v[hot] = 1.;
    v
}

/// `done` with a vector of `identity`, through the real claim and publication path.
async fn done_with(pool: &PgPool, track: uuid::Uuid, identity: &ml::Provenance, hot: usize) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, started_at, next_attempt_at)
         VALUES ($1, 'embedding', 'working', clock_timestamp(), now() + interval '15 minutes')",
    )
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
    let claim = ml::embedding_claim(pool, track).await.unwrap().unwrap();
    assert!(
        ml::complete_embedding(pool, &claim, identity, &vector(hot))
            .await
            .unwrap()
    );
}

/// A general-model library (plus one correct vector and one row with none), with junk audio files so the other
/// local stages of a cycle fail per file instead of touching the tracks.
async fn library(pool: &PgPool, root: &Path) {
    for (i, identity) in [Some(general()), Some(general()), Some(music()), None]
        .into_iter()
        .enumerate()
    {
        let path = format!("t{i}.flac");
        std::fs::write(root.join(&path), b"not audio").unwrap();
        let t = common::track(pool, &path, 30).await;
        match identity {
            Some(identity) => done_with(pool, t, &identity, i).await,
            None => {
                sqlx::query(
                    "INSERT INTO track_stages (track_id, stage, status, source, finished_at)
                     VALUES ($1, 'embedding', 'done', 'clap', now())",
                )
                .bind(t)
                .execute(pool)
                .await
                .unwrap();
            }
        }
    }
}

async fn embedding_state(pool: &PgPool) -> (Vec<String>, Vec<String>) {
    let stages = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM track_stages s WHERE stage = 'embedding' ORDER BY track_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let vectors = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM (SELECT track_id, file_generation, model_id,
             encode(model_sha256, 'hex') AS sha, pipeline_version, embedding::text AS v, created_at
             FROM track_embeddings) e ORDER BY track_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    (stages, vectors)
}

/// The capability configurations that are not `Available`, each with a reason.
fn unavailable_configs(models: &Path) -> Vec<(&'static str, kima_ident::EmbeddingConfig)> {
    let base = kima_ident::EmbeddingConfig::default();
    let empty = models.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let bad = models.join("bad-hash");
    std::fs::create_dir_all(&bad).unwrap();
    for name in [
        OnnxModel::CLAP_AUDIO_BASENAME,
        OnnxModel::CLAP_TEXT_BASENAME,
        "tokenizer.json",
    ] {
        std::fs::write(bad.join(name), b"not the pinned bytes").unwrap();
    }
    vec![
        ("unconfigured", base.clone()),
        (
            "disabled",
            kima_ident::EmbeddingConfig {
                disabled: true,
                model_dir: Some(bad.clone()),
                ..base.clone()
            },
        ),
        (
            "files missing",
            kima_ident::EmbeddingConfig {
                model_dir: Some(empty),
                ..base.clone()
            },
        ),
        (
            "bytes do not match the pin",
            kima_ident::EmbeddingConfig {
                model_dir: Some(bad),
                ..base
            },
        ),
    ]
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_unavailable_music_capability_reopens_nothing(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    library(&pool, &root).await;
    let models = tempfile::tempdir().unwrap();
    let before = embedding_state(&pool).await;
    assert_eq!(ml::unmatched_embeddings(&pool, &music()).await.unwrap(), 3);
    for (why, config) in unavailable_configs(models.path()) {
        let mut ctx = common::ident_ctx_with_embedding(
            &pool,
            &root,
            common::dead_bases(),
            IdentifySwitch::new(false, false),
            config,
        );
        ctx.config.fingerprinting = false;
        let outcome = embedding::reopen_unmatched_vectors(
            &ctx,
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, VectorTransition::Gated, "{why}");
        assert_eq!(embedding_state(&pool).await, before, "{why}: transition");
        // The same through the coordinator's own job path.
        *ctx.last_cycle.lock().unwrap() = None;
        let report = coordinator::run_cycle(&ctx, &CancellationToken::new())
            .await
            .unwrap();
        assert!(report.ran, "{why}");
        assert_eq!(report.reopened_vectors, 0, "{why}");
        assert_eq!(report.embedding.claimed, 0, "{why}");
        assert_eq!(embedding_state(&pool).await, before, "{why}: cycle");
    }
    assert_eq!(ml::unmatched_embeddings(&pool, &music()).await.unwrap(), 3);
}

/// Cancellation or a stopped cycle before the pass starts writes nothing either.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_cancelled_or_stopped_cycle_reopens_nothing(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    library(&pool, &root).await;
    let before = embedding_state(&pool).await;
    let ctx = common::ident_ctx(&pool, &root, common::dead_bases());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    for (cancel, stop) in [
        (cancelled.clone(), CancellationToken::new()),
        (CancellationToken::new(), cancelled.clone()),
    ] {
        let outcome = embedding::reopen_unmatched_vectors(&ctx, &cancel, &stop)
            .await
            .unwrap();
        assert_eq!(outcome, VectorTransition::Stopped { reopened: 0 });
    }
    assert_eq!(embedding_state(&pool).await, before);
}
