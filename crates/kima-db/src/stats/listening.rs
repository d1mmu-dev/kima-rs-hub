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

#[derive(sqlx::FromRow)]
pub struct HourTrack {
    pub title: String,
    pub artist: String,
    pub listens: i64,
}
#[derive(sqlx::FromRow)]
pub struct HourArtist {
    pub name: String,
    pub listens: i64,
}
pub struct HourDetail {
    pub tracks: Vec<HourTrack>,
    pub artists: Vec<HourArtist>,
    pub listens: i64,
    pub last_at: Option<time::OffsetDateTime>,
}

pub async fn hour_detail(
    pool: &PgPool,
    user: Uuid,
    dow: i32,
    hour: i32,
    zone: &str,
    period: Period,
) -> sqlx::Result<HourDetail> {
    let prefix = format!(
        "{}, cell AS (SELECT * FROM p WHERE EXTRACT(DOW FROM played_at AT TIME ZONE $2)::int = $3 AND EXTRACT(HOUR FROM played_at AT TIME ZONE $2)::int = $4) ",
        listen_rows(period, false)
    );
    let tracks = sqlx::query_as::<_, HourTrack>(&format!("{prefix} SELECT coalesce(t.display_title,t.title) AS title, coalesce(ar.display_name,ar.name) AS artist, count(*) AS listens FROM cell p JOIN tracks t ON t.id=p.track_id JOIN albums al ON al.id=p.album_id JOIN artists ar ON ar.id=al.artist_id GROUP BY 1,2 ORDER BY listens DESC, 1 LIMIT 6"))
        .bind(user).bind(zone).bind(dow).bind(hour).fetch_all(pool).await?;
    let artists = sqlx::query_as::<_, HourArtist>(&format!("{prefix} SELECT coalesce(ar.display_name,ar.name) AS name, count(*) AS listens FROM cell p JOIN albums al ON al.id=p.album_id JOIN artists ar ON ar.id=al.artist_id GROUP BY 1 ORDER BY listens DESC, 1 LIMIT 4"))
        .bind(user).bind(zone).bind(dow).bind(hour).fetch_all(pool).await?;
    let (listens, last_at) = sqlx::query_as::<_, (i64, Option<time::OffsetDateTime>)>(&format!(
        "{prefix} SELECT count(*), max(played_at) FROM cell"
    ))
    .bind(user)
    .bind(zone)
    .bind(dow)
    .bind(hour)
    .fetch_one(pool)
    .await?;
    Ok(HourDetail {
        tracks,
        artists,
        listens,
        last_at,
    })
}
