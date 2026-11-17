//! Readings derived from current stored music vectors, never by decoding audio again.
use super::ELIGIBLE;
use kima_vibe::zero_shot::Readings;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, FromRow)]
pub struct Pending {
    pub track_id: Uuid,
    pub embedded_at: OffsetDateTime,
    pub embedding: Vec<f32>,
}

/// Bound memory to one batch; wrong checkpoint, missing files and current readings are excluded.
pub async fn pending(pool: &PgPool, limit: i64) -> sqlx::Result<Vec<Pending>> {
    sqlx::query_as(&format!(
        "SELECT te.track_id, te.created_at AS embedded_at, te.embedding::real[] AS embedding
         FROM {} AND NOT EXISTS (SELECT 1 FROM track_clap_readings r
           WHERE r.track_id=te.track_id AND r.embedded_at=te.created_at)
         ORDER BY te.track_id LIMIT $1",
        *ELIGIBLE
    ))
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await
}

pub type ReadingRow = (Uuid, OffsetDateTime, Readings);

/// Batch publication is fenced against vector replacement, model changes and missing files.
/// The locked current-vector CTE prevents replacement between freshness check and insert.
/// No writes happen in detached inference workers. Dropping this future rolls back its transaction.
pub async fn upsert(pool: &PgPool, rows: &[ReadingRow]) -> sqlx::Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    if rows.len() > 500 {
        return Err(sqlx::Error::Protocol("readings batch exceeds 500".into()));
    }
    let mut ids = Vec::new();
    let mut ats = Vec::new();
    let mut instr = Vec::new();
    let mut acoustic = Vec::new();
    let mut valence = Vec::new();
    let mut arousal = Vec::new();
    for (id, at, r) in rows {
        if ![
            r.instrumentalness,
            r.acousticness,
            r.valence_raw,
            r.arousal_raw,
        ]
        .iter()
        .all(|v| v.is_finite() && (0. ..=1.).contains(v))
        {
            return Err(sqlx::Error::Protocol("invalid CLAP readings".into()));
        }
        ids.push(*id);
        ats.push(*at);
        instr.push(r.instrumentalness as f32);
        acoustic.push(r.acousticness as f32);
        valence.push(r.valence_raw as f32);
        arousal.push(r.arousal_raw as f32);
    }
    let mut tx = pool.begin().await?;
    // The invalidation trigger locks the track before deleting its vector. Acquire
    // track locks in a separate statement: a multi-table row mark may lock te first.
    sqlx::query(&format!(
        "SELECT t.id FROM {} AND te.track_id=ANY($1) ORDER BY t.id FOR SHARE OF t",
        *ELIGIBLE
    ))
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    let n=sqlx::query(&format!(
        "WITH current AS MATERIALIZED (
           SELECT te.track_id,te.created_at FROM {} AND te.track_id=ANY($1)
           ORDER BY te.track_id FOR SHARE OF te),
         input AS (SELECT * FROM unnest($1::uuid[], $2::timestamptz[], $3::real[], $4::real[], $5::real[], $6::real[])
           AS r(track_id,embedded_at,instrumentalness,acousticness,valence_raw,arousal_raw))
         INSERT INTO track_clap_readings (track_id,embedded_at,instrumentalness,acousticness,valence_raw,arousal_raw)
         SELECT r.* FROM input r JOIN current c ON c.track_id=r.track_id AND c.created_at=r.embedded_at
         ORDER BY r.track_id
         ON CONFLICT(track_id) DO UPDATE SET embedded_at=EXCLUDED.embedded_at,
           instrumentalness=EXCLUDED.instrumentalness,acousticness=EXCLUDED.acousticness,
           valence_raw=EXCLUDED.valence_raw,arousal_raw=EXCLUDED.arousal_raw", *ELIGIBLE))
        .bind(ids).bind(ats).bind(instr).bind(acoustic).bind(valence).bind(arousal)
        .execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(n)
}
