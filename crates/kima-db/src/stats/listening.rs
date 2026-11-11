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

#[derive(Debug, sqlx::FromRow)]
pub struct Peak {
    pub weekday: i32,
    pub hour: i32,
    pub plays: i64,
}
#[derive(Debug, sqlx::FromRow)]
pub struct DriftMonth {
    pub month: String,
    pub plays: i64,
}
#[derive(Debug, sqlx::FromRow)]
pub struct TopArtist {
    pub id: Uuid,
    pub name: String,
    pub plays: i64,
    pub estimated_seconds: i64,
}
#[derive(Debug, sqlx::FromRow)]
pub struct TopTrack {
    pub id: Uuid,
    pub title: String,
    pub artist: String,
    pub plays: i64,
}
pub struct ListeningStats {
    pub plays: i64,
    pub estimated_seconds: i64,
    pub distinct_tracks: i64,
    pub distinct_artists: i64,
    pub distinct_albums: i64,
    pub first_play: Option<time::OffsetDateTime>,
    pub last_play: Option<time::OffsetDateTime>,
    pub by_source: Vec<(String, i64)>,
    pub clock: [[i64; 24]; 7],
    pub peak: Option<Peak>,
    pub drift: Vec<DriftMonth>,
    pub top_artists: Vec<TopArtist>,
    pub top_tracks: Vec<TopTrack>,
}
pub async fn listening_stats(
    pool: &PgPool,
    user: Uuid,
    tz: &str,
    period: Period,
) -> sqlx::Result<ListeningStats> {
    let prefix = listen_rows(period, false);
    let totals_sql = format!(
        "{prefix} SELECT count(*), coalesce(sum(least(p.duration,600)),0)::bigint, count(DISTINCT p.track_id), count(DISTINCT al.artist_id), count(DISTINCT p.album_id), min(p.played_at), max(p.played_at) FROM p JOIN albums al ON al.id=p.album_id"
    );
    let source_sql =
        format!("{prefix} SELECT source, count(*) FROM p GROUP BY source ORDER BY source");
    let clock_sql = format!(
        "{prefix} SELECT extract(dow FROM played_at AT TIME ZONE $2)::int, extract(hour FROM played_at AT TIME ZONE $2)::int, count(*) FROM p GROUP BY 1,2 ORDER BY 1,2"
    );
    let drift_sql = format!(
        "{prefix} SELECT to_char(date_trunc('month',played_at AT TIME ZONE $2),'YYYY-MM') AS month, count(*) AS plays FROM p GROUP BY 1 ORDER BY 1"
    );
    let artists_sql = format!(
        "{prefix} SELECT ar.id, coalesce(ar.display_name,ar.name) AS name, count(*) AS plays, coalesce(sum(least(p.duration,600)),0)::bigint AS estimated_seconds FROM p JOIN albums al ON al.id=p.album_id JOIN artists ar ON ar.id=al.artist_id GROUP BY 1,2 ORDER BY plays DESC, name, ar.id LIMIT 8"
    );
    let tracks_sql = format!(
        "{prefix} SELECT t.id, coalesce(t.display_title,t.title) AS title, coalesce(ar.display_name,ar.name) AS artist, count(*) AS plays FROM p JOIN tracks t ON t.id=p.track_id JOIN albums al ON al.id=p.album_id JOIN artists ar ON ar.id=al.artist_id GROUP BY 1,2,3 ORDER BY plays DESC, title, t.id LIMIT 8"
    );
    type Totals = (
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<time::OffsetDateTime>,
        Option<time::OffsetDateTime>,
    );
    let (totals, by_source, clock_rows, drift, top_artists, top_tracks) = tokio::try_join!(
        sqlx::query_as::<_, Totals>(&totals_sql)
            .bind(user)
            .fetch_one(pool),
        sqlx::query_as::<_, (String, i64)>(&source_sql)
            .bind(user)
            .fetch_all(pool),
        sqlx::query_as::<_, (i32, i32, i64)>(&clock_sql)
            .bind(user)
            .bind(tz)
            .fetch_all(pool),
        sqlx::query_as::<_, DriftMonth>(&drift_sql)
            .bind(user)
            .bind(tz)
            .fetch_all(pool),
        sqlx::query_as::<_, TopArtist>(&artists_sql)
            .bind(user)
            .fetch_all(pool),
        sqlx::query_as::<_, TopTrack>(&tracks_sql)
            .bind(user)
            .fetch_all(pool),
    )?;
    let mut clock = [[0; 24]; 7];
    let mut peak: Option<Peak> = None;
    for (weekday, hour, plays) in clock_rows {
        clock[weekday as usize][hour as usize] = plays;
        if peak.as_ref().is_none_or(|p| plays > p.plays) {
            peak = Some(Peak {
                weekday,
                hour,
                plays,
            });
        }
    }
    Ok(ListeningStats {
        plays: totals.0,
        estimated_seconds: totals.1,
        distinct_tracks: totals.2,
        distinct_artists: totals.3,
        distinct_albums: totals.4,
        first_play: totals.5,
        last_play: totals.6,
        by_source,
        clock,
        peak,
        drift,
        top_artists,
        top_tracks,
    })
}
