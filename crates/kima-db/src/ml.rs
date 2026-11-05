//! CLAP storage, independent of inference and stage scheduling.
//! Callers capture an existing working claim before reading audio. No connection is held
//! across computation. A database generation is not proof of immutable source bytes:
//! the hub must still enforce the stable-input contract before submitting a result.
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// Exact encoder bytes plus the preprocessing/aggregation implementation version.
/// These are caller assertions, not independent model verification by the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub model_id: String,
    pub model_sha256: [u8; 32],
    pub pipeline_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingClaim {
    pub track_id: Uuid,
    pub started_at: OffsetDateTime,
    pub file_generation: i64,
}

/// Snapshot a scheduler-owned claim; does not claim work or choose eligibility.
pub async fn embedding_claim(pool: &PgPool, track: Uuid) -> sqlx::Result<Option<EmbeddingClaim>> {
    sqlx::query_as!(EmbeddingClaim,
        "SELECT t.id AS track_id, s.started_at AS \"started_at!\", t.ml_generation AS file_generation
         FROM tracks t JOIN track_stages s ON s.track_id=t.id AND s.stage='embedding'
         WHERE t.id=$1 AND t.missing_since IS NULL AND s.status='working'
           AND s.started_at IS NOT NULL AND s.next_attempt_at > clock_timestamp()",
        track
    ).fetch_optional(pool).await
}

/// Capture the file generation only for the exact claim held by this worker.
pub async fn embedding_claim_for_worker(
    pool: &PgPool,
    track: Uuid,
    started_at: OffsetDateTime,
) -> sqlx::Result<Option<EmbeddingClaim>> {
    let generation: Option<i64> = sqlx::query_scalar(
        "SELECT t.ml_generation FROM tracks t JOIN track_stages s ON s.track_id=t.id AND s.stage='embedding'
         WHERE t.id=$1 AND t.missing_since IS NULL AND s.status='working'
           AND s.started_at=$2 AND s.next_attempt_at > clock_timestamp()",
    )
    .bind(track)
    .bind(started_at)
    .fetch_optional(pool)
    .await?;
    Ok(generation.map(|file_generation| EmbeddingClaim {
        track_id: track,
        started_at,
        file_generation,
    }))
}

/// Atomically capture the path and generation belonging to the exact worker claim.
pub async fn embedding_input_for_worker(
    pool: &PgPool,
    track: Uuid,
    started_at: OffsetDateTime,
) -> sqlx::Result<Option<(String, EmbeddingClaim)>> {
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT t.path, t.ml_generation FROM tracks t JOIN track_stages s ON s.track_id=t.id AND s.stage='embedding'
         WHERE t.id=$1 AND t.missing_since IS NULL AND s.status='working'
           AND s.started_at=$2 AND s.next_attempt_at > clock_timestamp()",
    ).bind(track).bind(started_at).fetch_optional(pool).await?;
    Ok(row.map(|(path, file_generation)| {
        (
            path,
            EmbeddingClaim {
                track_id: track,
                started_at,
                file_generation,
            },
        )
    }))
}

/// Publish the result and stage success in one transaction, or change nothing.
/// Locks track then stage, matching file invalidation's lock order. Invalid dimensions,
/// nonfinite/zero vectors or invalid provenance fail in Postgres and roll back success.
/// Receives the existing computation boundary's normalized 512 floats without renormalizing.
pub async fn complete_embedding(
    pool: &PgPool,
    claim: &EmbeddingClaim,
    provenance: &Provenance,
    embedding: &[f32],
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar!(
        "SELECT id FROM tracks WHERE id=$1 AND ml_generation=$2 AND missing_since IS NULL FOR UPDATE",
        claim.track_id, claim.file_generation
    ).fetch_optional(&mut *tx).await?;
    if current.is_none() {
        return Ok(false);
    }
    // Lock first, then evaluate the lease with wall-clock time. An UPDATE predicate
    // evaluated before a lock wait can otherwise publish after the lease has expired.
    sqlx::query!(
        "SELECT track_id FROM track_stages WHERE track_id=$1 AND stage='embedding' FOR UPDATE",
        claim.track_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let done = sqlx::query(
        "UPDATE track_stages SET status='done', next_attempt_at=NULL, last_error=NULL,
             error_code=NULL, error_class=NULL, source=$3, evidence=$4,
             finished_at=clock_timestamp(), updated_at=clock_timestamp()
         WHERE track_id=$1 AND stage='embedding' AND status='working' AND started_at=$2
           AND next_attempt_at > clock_timestamp()",
    )
    .bind(claim.track_id)
    .bind(claim.started_at)
    .bind("clap")
    .bind(serde_json::json!({"pipeline_version": provenance.pipeline_version}))
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query!(
        "INSERT INTO track_embeddings
             (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         VALUES ($1,$2,$3,$4,$5,$6::real[]::vector)
         ON CONFLICT (track_id) DO UPDATE SET file_generation=EXCLUDED.file_generation,
             model_id=EXCLUDED.model_id, model_sha256=EXCLUDED.model_sha256,
             pipeline_version=EXCLUDED.pipeline_version, embedding=EXCLUDED.embedding,
             created_at=clock_timestamp()",
        claim.track_id,
        claim.file_generation,
        provenance.model_id,
        &provenance.model_sha256[..],
        provenance.pipeline_version,
        embedding
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Only expose results matching the requested model AND pipeline on a present generation
/// with completed embedding work. Explicit provenance prevents accidental model mixing.
pub async fn embedding(
    pool: &PgPool,
    track: Uuid,
    provenance: &Provenance,
) -> sqlx::Result<Option<Vec<f32>>> {
    sqlx::query_scalar!(
        "SELECT e.embedding::real[] AS \"embedding!\" FROM track_embeddings e
         JOIN tracks t ON t.id=e.track_id AND t.ml_generation=e.file_generation
         JOIN track_stages s ON s.track_id=t.id AND s.stage='embedding' AND s.status='done'
         WHERE t.id=$1 AND t.missing_since IS NULL AND e.model_id=$2
           AND e.model_sha256=$3 AND e.pipeline_version=$4",
        track,
        provenance.model_id,
        &provenance.model_sha256[..],
        provenance.pipeline_version
    )
    .fetch_optional(pool)
    .await
}
