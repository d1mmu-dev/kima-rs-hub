use sqlx::PgPool;
use uuid::Uuid;

use super::listens::{Period, listen_rows};

pub async fn track_play_count(pool: &PgPool, user: Uuid, track: Uuid) -> sqlx::Result<i64> {
    let sql = format!("{} SELECT count(*) FROM p", listen_rows(Period::All, true));
    sqlx::query_scalar(&sql)
        .bind(user)
        .bind(track)
        .fetch_one(pool)
        .await
}
