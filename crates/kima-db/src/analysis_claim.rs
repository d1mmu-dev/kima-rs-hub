//! Claim/lease snapshot layer for the `'analysis'` `track_stages` row (M4), mirroring
//! `kima_db::ml`'s embedding claim functions exactly (same worker-token binding via
//! `(track_id, started_at)`, same `next_attempt_at > clock_timestamp()` lease check).
//!
//! This module intentionally stops at the claim/lease boundary: it has no `complete_*`
//! counterpart to `ml::complete_embedding`. The 'analysis' stage's publication
//! (`kima_ml::analysis_publish::publish_track_analysis`, reviewed and left untouched here)
//! writes four independently-nullable `track_analysis` column groups, each fenced on its
//! own against `tracks.ml_generation` — not against this ledger row. How/when the
//! `track_stages` row for `'analysis'` itself transitions to `done` given four
//! independent group outcomes is a scheduling policy decision for a future attempt.rs
//! slice, not invented here. Closing a claim as *failed* already goes through
//! `stages::report_failure`, which this module does not duplicate: that function already
//! branches on `Stage::Analysis` for the same lock-row-before-evaluating-the-lease
//! ordering as `Stage::Embedding` (`stages.rs`'s own lease-fencing pitfall).
//!
//! Callers of `kima_ml::analysis_publish::publish_track_analysis` must already own a
//! live claim before calling it (that module's own doc comment): the functions here are
//! how a future attempt.rs caller captures that claim's `file_generation`, binds it to
//! its own worker token, and atomically pairs it with the input path to compute against.
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// A snapshot of a live `'analysis'` claim: the file generation the caller may compute
/// against, fenced the same way as [`crate::ml::EmbeddingClaim`]. Not itself a database
/// row lock — callers still race normal invalidation/lease-expiry until they finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisClaim {
    pub track_id: Uuid,
    pub started_at: OffsetDateTime,
    pub file_generation: i64,
}

/// Snapshot a scheduler-owned claim; does not claim work or choose eligibility. Mirrors
/// `ml::embedding_claim`.
pub async fn analysis_claim(pool: &PgPool, track: Uuid) -> sqlx::Result<Option<AnalysisClaim>> {
    sqlx::query_as!(AnalysisClaim,
        "SELECT t.id AS track_id, s.started_at AS \"started_at!\", t.ml_generation AS file_generation
         FROM tracks t JOIN track_stages s ON s.track_id=t.id AND s.stage='analysis'
         WHERE t.id=$1 AND t.missing_since IS NULL AND s.status='working'
           AND s.started_at IS NOT NULL AND s.next_attempt_at > clock_timestamp()",
        track
    ).fetch_optional(pool).await
}

/// Capture the file generation only for the exact claim held by this worker. Mirrors
/// `ml::embedding_claim_for_worker`.
pub async fn analysis_claim_for_worker(
    pool: &PgPool,
    track: Uuid,
    started_at: OffsetDateTime,
) -> sqlx::Result<Option<AnalysisClaim>> {
    let generation: Option<i64> = sqlx::query_scalar(
        "SELECT t.ml_generation FROM tracks t JOIN track_stages s ON s.track_id=t.id AND s.stage='analysis'
         WHERE t.id=$1 AND t.missing_since IS NULL AND s.status='working'
           AND s.started_at=$2 AND s.next_attempt_at > clock_timestamp()",
    )
    .bind(track)
    .bind(started_at)
    .fetch_optional(pool)
    .await?;
    Ok(generation.map(|file_generation| AnalysisClaim {
        track_id: track,
        started_at,
        file_generation,
    }))
}

/// Atomically capture the path and generation belonging to the exact worker claim.
/// Mirrors `ml::embedding_input_for_worker`: the caller uses this `file_generation` as
/// the stable `file_generation` argument to every `analysis_publish::publish_*` call for
/// this attempt.
pub async fn analysis_input_for_worker(
    pool: &PgPool,
    track: Uuid,
    started_at: OffsetDateTime,
) -> sqlx::Result<Option<(String, AnalysisClaim)>> {
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT t.path, t.ml_generation FROM tracks t JOIN track_stages s ON s.track_id=t.id AND s.stage='analysis'
         WHERE t.id=$1 AND t.missing_since IS NULL AND s.status='working'
           AND s.started_at=$2 AND s.next_attempt_at > clock_timestamp()",
    ).bind(track).bind(started_at).fetch_optional(pool).await?;
    Ok(row.map(|(path, file_generation)| {
        (
            path,
            AnalysisClaim {
                track_id: track,
                started_at,
                file_generation,
            },
        )
    }))
}
