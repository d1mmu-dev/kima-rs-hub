//! Subsonic ratings 1..=5 per (user, track | album | artist), spec §7.3. Setting a rating again
//! replaces it; `clear` is what `setRating` with 0 does.

use sqlx::PgPool;
use uuid::Uuid;

use crate::stars::{StarKind, target_exists};

/// Stores `rating` (1..=5) for the target, replacing an earlier one. `false` when the target does
/// not exist. A rating outside 1..=5 is rejected by the table and comes back as an error; callers
/// validate first.
pub async fn set(
    pool: &PgPool,
    user: Uuid,
    kind: StarKind,
    id: Uuid,
    rating: i16,
) -> sqlx::Result<bool> {
    if !target_exists(pool, kind, id).await? {
        return Ok(false);
    }
    let col = kind.column();
    // The conflict target is the partial unique index of this kind.
    let sql = format!(
        "INSERT INTO ratings (id, user_id, {col}, rating) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, {col}) WHERE {col} IS NOT NULL
         DO UPDATE SET rating = EXCLUDED.rating, rated_at = now()"
    );
    match sqlx::query(&sql)
        .bind(Uuid::now_v7())
        .bind(user)
        .bind(id)
        .bind(rating)
        .execute(pool)
        .await
    {
        Ok(_) => Ok(true),
        // The target was purged between the check and the insert.
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Ok(false),
        Err(e) => Err(e),
    }
}

pub async fn clear(pool: &PgPool, user: Uuid, kind: StarKind, id: Uuid) -> sqlx::Result<()> {
    let sql = format!(
        "DELETE FROM ratings WHERE user_id = $1 AND {} = $2",
        kind.column()
    );
    sqlx::query(&sql).bind(user).bind(id).execute(pool).await?;
    Ok(())
}
