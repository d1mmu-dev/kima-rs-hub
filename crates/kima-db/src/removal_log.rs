//! `removal_log`: a snapshot of every track the reconcile retired (D-161), newest first.

use serde_json::Value;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RemovalRow {
    pub id: Uuid,
    pub file_path: String,
    pub title: String,
    pub artist_name: String,
    pub album_title: String,
    pub reason: String,
    pub artifacts: Value,
    pub removed_at: OffsetDateTime,
}

/// Rows, newest first. `limit` and `offset` must already be clamped by the caller.
pub async fn list(pool: &PgPool, limit: i64, offset: i64) -> sqlx::Result<Vec<RemovalRow>> {
    sqlx::query_as!(
        RemovalRow,
        "SELECT id, file_path, title, artist_name, album_title, reason, artifacts, removed_at
         FROM removal_log ORDER BY removed_at DESC, id DESC LIMIT $1 OFFSET $2",
        limit,
        offset
    )
    .fetch_all(pool)
    .await
}

/// All rows in the log (not the page).
pub async fn count(pool: &PgPool) -> sqlx::Result<i64> {
    Ok(sqlx::query_scalar!("SELECT count(*) FROM removal_log")
        .fetch_one(pool)
        .await?
        .unwrap_or(0))
}
