//! Per-user playback state (what is playing and the queue), for resuming on another device.

use serde_json::Value;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

/// The longest queue kept. Upstream reads this from an admin setting; M1 has no such setting.
pub const QUEUE_LIMIT: usize = 10_000;

#[derive(Debug, Clone, FromRow)]
pub struct PlaybackState {
    pub user_id: Uuid,
    pub playback_type: String,
    pub track_id: Option<String>,
    pub audiobook_id: Option<String>,
    pub podcast_id: Option<String>,
    pub queue: Option<Value>,
    pub current_index: i32,
    pub is_shuffle: bool,
    pub position: i32,
    pub changed_by: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A partial update. `None` leaves a stored field alone; `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub playback_type: String,
    pub track_id: Option<Option<String>>,
    pub audiobook_id: Option<Option<String>>,
    pub podcast_id: Option<Option<String>>,
    pub queue: Option<Option<Value>>,
    pub current_index: Option<i32>,
    pub is_shuffle: Option<bool>,
}

pub async fn get(pool: &PgPool, user: Uuid) -> sqlx::Result<Option<PlaybackState>> {
    sqlx::query_as("SELECT * FROM playback_state WHERE user_id = $1")
        .bind(user)
        .fetch_optional(pool)
        .await
}

/// Inserts the user's state, or applies `patch` to the stored one in a single statement.
pub async fn upsert(pool: &PgPool, user: Uuid, patch: Patch) -> sqlx::Result<PlaybackState> {
    let flat = |o: &Option<Option<String>>| o.clone().flatten();
    sqlx::query_as(
        "INSERT INTO playback_state AS s
             (user_id, playback_type, track_id, audiobook_id, podcast_id, queue, current_index, is_shuffle)
         VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, 0), COALESCE($8, false))
         ON CONFLICT (user_id) DO UPDATE SET
             playback_type = EXCLUDED.playback_type,
             track_id = CASE WHEN $9 THEN EXCLUDED.track_id ELSE s.track_id END,
             audiobook_id = CASE WHEN $10 THEN EXCLUDED.audiobook_id ELSE s.audiobook_id END,
             podcast_id = CASE WHEN $11 THEN EXCLUDED.podcast_id ELSE s.podcast_id END,
             queue = CASE WHEN $12 THEN EXCLUDED.queue ELSE s.queue END,
             current_index = CASE WHEN $7 IS NOT NULL THEN EXCLUDED.current_index ELSE s.current_index END,
             is_shuffle = CASE WHEN $8 IS NOT NULL THEN EXCLUDED.is_shuffle ELSE s.is_shuffle END,
             updated_at = now()
         RETURNING *",
    )
    .bind(user)
    .bind(&patch.playback_type)
    .bind(flat(&patch.track_id))
    .bind(flat(&patch.audiobook_id))
    .bind(flat(&patch.podcast_id))
    .bind(patch.queue.clone().flatten())
    .bind(patch.current_index)
    .bind(patch.is_shuffle)
    .bind(patch.track_id.is_some())
    .bind(patch.audiobook_id.is_some())
    .bind(patch.podcast_id.is_some())
    .bind(patch.queue.is_some())
    .fetch_one(pool)
    .await
}

/// Clears the user's state; returns how many rows went (0 when there was none).
pub async fn delete(pool: &PgPool, user: Uuid) -> sqlx::Result<u64> {
    Ok(sqlx::query("DELETE FROM playback_state WHERE user_id = $1")
        .bind(user)
        .execute(pool)
        .await?
        .rows_affected())
}
