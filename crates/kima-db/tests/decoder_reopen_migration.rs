//! D-251: migration 0061 reopens, once, the `analysis` and `embedding` rows that failed only because the decoder
//! had no codec for the stream (Opus before libopus was registered). Nothing else changes: other failures, other
//! stages, done rows, vectors and analysis results stay byte for byte.
mod vibe_common;
use kima_db::PgPool;
use serde_json::Value;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use uuid::Uuid;
use vibe_common::*;

const REOPEN: i64 = 61;
/// The exact texts the stages stored on the owner's library (`kima-ml` `AudioError::CorruptAudio` wrapping
/// Symphonia's `Unsupported`): the embedding stage stores the whole error, analysis only the inner message.
const EMBED_TEXT: &str =
    "corrupt or invalid audio: unsupported feature: core (codec): unsupported audio codec";
const ANALYSIS_TEXT: &str = "unsupported feature: core (codec): unsupported audio codec";

fn before_reopen() -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version != REOPEN)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

#[allow(clippy::too_many_arguments)]
async fn stage(
    pool: &PgPool,
    track: Uuid,
    stage: &str,
    status: &str,
    attempts: i16,
    retry: bool,
    code: Option<&str>,
    class: Option<&str>,
    text: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, redelivered, source, last_error, error_code,
                                   error_class, evidence, next_attempt_at, started_at, finished_at, updated_at)
         VALUES ($1, $2, $3, $4, 1, 'x', $5, $6, $7, '{\"k\":1}'::jsonb,
                 CASE WHEN $8 THEN now() + interval '1 hour' END,
                 now() - interval '2 hours', now() - interval '1 hour', now() - interval '1 hour')",
    )
    .bind(track)
    .bind(stage)
    .bind(status)
    .bind(attempts)
    .bind(text)
    .bind(code)
    .bind(class)
    .bind(retry)
    .execute(pool)
    .await
    .unwrap();
}

async fn row(pool: &PgPool, track: Uuid, stage: &str) -> Value {
    sqlx::query_scalar(
        "SELECT to_jsonb(s) FROM track_stages s WHERE s.track_id = $1 AND s.stage = $2",
    )
    .bind(track)
    .bind(stage)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Every public table except the stage ledger and the migration log.
async fn others(pool: &PgPool) -> Vec<(String, Value)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public'
           AND tablename NOT IN ('_sqlx_migrations', 'track_stages') ORDER BY tablename",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut rows = Vec::new();
    for table in tables {
        let value: Value = sqlx::query_scalar(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text), '[]'::jsonb) FROM \"{table}\" t"
        ))
        .fetch_one(pool)
        .await
        .unwrap();
        rows.push((table, value));
    }
    rows
}

#[sqlx::test(migrations = false)]
async fn unsupported_codec_failures_reopen_once_and_nothing_else_moves(pool: PgPool) {
    before_reopen().run(&pool).await.unwrap();
    let opus = track(&pool).await.track;
    let retrying = track(&pool).await.track;
    let other = track(&pool).await.track;
    let done = track(&pool).await.track;

    let other_decode = "corrupt or invalid audio: opus: decode failed";
    let empty = "audio stream contains no samples";
    // (track, stage, status, attempts, retry scheduled, error code, stored error)
    #[rustfmt::skip]
    let seeds = [
        // Terminal codec failures on both ML stages (attempts exhausted): the owner's case.
        (opus, "analysis", "failed", 4, false, Some("CORRUPT_AUDIO"), Some(ANALYSIS_TEXT)),
        (opus, "embedding", "failed", 4, false, Some("DECODE_ERROR"), Some(EMBED_TEXT)),
        // The fingerprint stage decodes with ffmpeg: a failure there is not this decoder's.
        (opus, "fingerprint", "failed", 4, false, Some("DECODE_ERROR"), Some(EMBED_TEXT)),
        // A codec failure still inside its retry schedule gets its full budget back.
        (retrying, "embedding", "failed", 2, true, Some("DECODE_ERROR"), Some(EMBED_TEXT)),
        // Other decode failures describe the file and stay as they are.
        (other, "embedding", "failed", 4, false, Some("DECODE_ERROR"), Some(other_decode)),
        (other, "analysis", "failed", 1, false, Some("EMPTY_AUDIO"), Some(empty)),
        // The same text under another code, or a done row, is not a codec failure.
        (done, "analysis", "failed", 4, false, Some("ANALYSIS_STORAGE_REJECTED"), Some(ANALYSIS_TEXT)),
        (done, "embedding", "done", 0, false, None, None),
    ];
    for (t, s, status, attempts, retry, code, text) in seeds {
        let class = code.map(|_| "INCONCLUSIVE");
        stage(&pool, t, s, status, attempts, retry, code, class, text).await;
    }
    embed(&pool, done, &axis(3)).await;

    let kept = [
        (opus, "fingerprint"),
        (other, "embedding"),
        (other, "analysis"),
        (done, "analysis"),
        (done, "embedding"),
    ];
    let mut before = Vec::new();
    for (t, s) in kept {
        before.push(row(&pool, t, s).await);
    }
    let tables = others(&pool).await;

    kima_db::MIGRATOR.run(&pool).await.unwrap();

    for (t, s) in [
        (opus, "analysis"),
        (opus, "embedding"),
        (retrying, "embedding"),
    ] {
        let r = row(&pool, t, s).await;
        assert_eq!(r["status"], "pending", "{s}: {r}");
        assert_eq!(r["attempts"], 0, "{s}: {r}");
        assert_eq!(r["redelivered"], 0, "{s}: {r}");
        for null in [
            "next_attempt_at",
            "started_at",
            "finished_at",
            "last_error",
            "error_code",
            "error_class",
            "source",
        ] {
            assert_eq!(r[null], Value::Null, "{s}.{null}: {r}");
        }
        assert_eq!(
            r["evidence"]["reason"], "decoder gained the codec",
            "{s}: {r}"
        );
    }
    for ((t, s), was) in kept.into_iter().zip(before) {
        assert_eq!(row(&pool, t, s).await, was, "{s} must not move");
    }
    assert_eq!(others(&pool).await, tables, "only stage rows may change");

    // Applied once: a later failure with the same text is the stage's own business.
    let applied: Vec<(i64, bool)> =
        sqlx::query_as("SELECT version, success FROM _sqlx_migrations WHERE version = $1")
            .bind(REOPEN)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(applied, vec![(REOPEN, true)]);
}
