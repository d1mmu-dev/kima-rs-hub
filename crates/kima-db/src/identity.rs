//! Identification data (spec §6, §9): the AcoustID fingerprint kept apart from `tracks`.

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Store (or replace) `track`'s AcoustID fingerprint and the duration sent with it. Runs on
/// the caller's connection so it shares the stage report's transaction.
pub async fn store_fingerprint(
    conn: &mut PgConnection,
    track: Uuid,
    fingerprint: &str,
    duration_secs: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs)
         VALUES ($1, $2, $3)
         ON CONFLICT (track_id) DO UPDATE SET
           fingerprint = EXCLUDED.fingerprint, duration_secs = EXCLUDED.duration_secs,
           computed_at = now()",
        track,
        fingerprint,
        duration_secs,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `track`'s stored fingerprint and duration, if any.
pub async fn fingerprint_of(pool: &PgPool, track: Uuid) -> sqlx::Result<Option<(String, i32)>> {
    let row = sqlx::query!(
        "SELECT fingerprint, duration_secs FROM track_acoustid WHERE track_id = $1",
        track
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.fingerprint, r.duration_secs)))
}

/// `track`'s path (relative to the library root) and `duration_secs`.
pub async fn track_path_and_duration(
    pool: &PgPool,
    track: Uuid,
) -> sqlx::Result<Option<(String, i32)>> {
    let row = sqlx::query!(
        "SELECT path, duration_secs FROM tracks WHERE id = $1",
        track
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.path, r.duration_secs)))
}
