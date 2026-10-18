//! Subsonic bookmarks: a resume position per (user, track), spec §7.3. A bookmark of a track
//! flagged missing is kept but hidden until the purge removes the track.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, FromRow)]
pub struct Bookmark {
    pub track_id: Uuid,
    pub position_ms: i64,
    pub comment: Option<String>,
    pub created_at: OffsetDateTime,
    pub changed_at: OffsetDateTime,
}

/// Creates or moves the user's bookmark on the track (the creation time stays). `false` when the
/// track does not exist.
pub async fn upsert(
    pool: &PgPool,
    user: Uuid,
    track: Uuid,
    position_ms: i64,
    comment: Option<&str>,
) -> sqlx::Result<bool> {
    let res = sqlx::query(
        "INSERT INTO subsonic_bookmarks (user_id, track_id, position_ms, comment)
         SELECT $1, $2, $3, $4 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = $2)
         ON CONFLICT (user_id, track_id)
         DO UPDATE SET position_ms = EXCLUDED.position_ms, comment = EXCLUDED.comment, changed_at = now()",
    )
    .bind(user)
    .bind(track)
    .bind(position_ms)
    .bind(comment)
    .execute(pool)
    .await;
    match res {
        Ok(r) => Ok(r.rows_affected() > 0),
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn delete(pool: &PgPool, user: Uuid, track: Uuid) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM subsonic_bookmarks WHERE user_id = $1 AND track_id = $2")
        .bind(user)
        .bind(track)
        .execute(pool)
        .await?;
    Ok(())
}

/// The user's bookmarks on present tracks, most recently changed first.
pub async fn list_for_user(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<Bookmark>> {
    sqlx::query_as(
        "SELECT b.track_id, b.position_ms, b.comment, b.created_at, b.changed_at
         FROM subsonic_bookmarks b JOIN tracks t ON t.id = b.track_id
         WHERE b.user_id = $1 AND t.missing_since IS NULL
         ORDER BY b.changed_at DESC, b.track_id ASC",
    )
    .bind(user)
    .fetch_all(pool)
    .await
}
