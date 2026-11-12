use sqlx::PgPool;
use uuid::Uuid;

use super::listens::{Period, listen_rows};

#[derive(sqlx::FromRow)]
pub struct PlayableRow {
    pub id: Uuid,
    pub title: String,
    pub duration: i32,
    pub artist_id: Uuid,
    pub artist_name: String,
    pub album_id: Uuid,
    pub album_title: String,
}

pub async fn playable_slice(
    pool: &PgPool,
    user: Uuid,
    slice: &str,
    decade: f64,
    limit: i64,
    period: Period,
) -> sqlx::Result<Vec<PlayableRow>> {
    let select = "SELECT t.id, coalesce(t.display_title,t.title) title, t.duration_secs duration, ar.id artist_id, coalesce(ar.display_name,ar.name) artist_name, al.id album_id, coalesce(al.display_title,al.title) album_title";
    let joins = "JOIN albums al ON al.id=t.album_id JOIN artists ar ON ar.id=al.artist_id";
    match slice {
        "top" | "core" => {
            let sql = format!(
                "{} , counts AS (SELECT track_id, count(*) n FROM p GROUP BY track_id) {select} FROM counts c JOIN tracks t ON t.id=c.track_id {joins} ORDER BY c.n DESC, title, t.id LIMIT $2",
                listen_rows(period, false)
            );
            sqlx::query_as(&sql)
                .bind(user)
                .bind(limit)
                .fetch_all(pool)
                .await
        }
        "never" => {
            let sql = format!(
                "{select} FROM tracks t {joins} WHERE NOT EXISTS (SELECT 1 FROM plays p WHERE p.track_id=t.id AND p.user_id=$1) ORDER BY random() LIMIT $2"
            );
            sqlx::query_as(&sql)
                .bind(user)
                .bind(limit)
                .fetch_all(pool)
                .await
        }
        "decade" if decade.is_finite() && decade != 0.0 => {
            let sql = format!(
                "{select} FROM tracks t {joins} WHERE coalesce(al.display_year,al.year)::float8 BETWEEN $1 AND $1 + 9 ORDER BY random() LIMIT $2"
            );
            sqlx::query_as(&sql)
                .bind(decade)
                .bind(limit)
                .fetch_all(pool)
                .await
        }
        _ => Ok(vec![]),
    }
}
