use sqlx::PgPool;
use uuid::Uuid;

use super::listens::{Period, listen_rows};

#[derive(sqlx::FromRow)]
pub struct TallyRow {
    pub tracks: i64,
    pub albums: i64,
    pub artists: i64,
}
#[derive(sqlx::FromRow)]
pub struct DecadeRow {
    pub decade: i32,
    pub albums: i64,
    pub heard: i64,
}
#[derive(sqlx::FromRow)]
pub struct Concentration {
    pub for_half: Option<i64>,
    pub for_eighty: Option<i64>,
    pub distinct: Option<i64>,
}
#[derive(sqlx::FromRow)]
pub struct Habit {
    pub moves: i64,
    pub same_album: f64,
    pub in_order: f64,
}
#[derive(sqlx::FromRow)]
pub struct EraRow {
    pub label: String,
    pub plays: i64,
    pub median: i32,
}

pub struct Overview {
    pub total: TallyRow,
    pub heard: TallyRow,
    pub decades: Vec<DecadeRow>,
    pub concentration: Concentration,
    pub habit: Habit,
    pub era: Vec<EraRow>,
}

pub async fn overview(pool: &PgPool, user: Uuid, period: Period) -> sqlx::Result<Overview> {
    let all = listen_rows(Period::All, false);
    let prefix = listen_rows(period, false);
    let totals_sql = format!(
        "{all} SELECT count(t.id) tracks, count(DISTINCT a.id) albums, count(DISTINCT a.artist_id) artists FROM albums a JOIN tracks t ON t.album_id=a.id"
    );
    let heard_sql = format!(
        "{all} SELECT count(DISTINCT t.id) tracks, count(DISTINCT a.id) albums, count(DISTINCT a.artist_id) artists FROM plays pl JOIN tracks t ON t.id=pl.track_id JOIN albums a ON a.id=t.album_id WHERE pl.user_id=$1"
    );
    let decades_sql = format!(
        "{all} SELECT (coalesce(a.display_year,a.year)/10*10)::int decade, count(DISTINCT a.id) albums, count(DISTINCT a.id) FILTER (WHERE pl.id IS NOT NULL) heard FROM albums a LEFT JOIN tracks t ON t.album_id=a.id LEFT JOIN plays pl ON pl.track_id=t.id AND pl.user_id=$1 WHERE coalesce(a.display_year,a.year) IS NOT NULL GROUP BY 1 HAVING (coalesce(a.display_year,a.year)/10*10)>1900 ORDER BY 1"
    );
    let conc_sql = format!(
        "{prefix}, c AS (SELECT track_id,count(*) n FROM p GROUP BY 1), r AS (SELECT n,sum(n) OVER (ORDER BY n DESC,track_id) running,sum(n) OVER () total,row_number() OVER (ORDER BY n DESC,track_id) rank FROM c) SELECT min(rank) FILTER (WHERE running >= total/2.0) for_half, min(rank) FILTER (WHERE running >= total*0.8) for_eighty, max(rank) distinct FROM r"
    );
    let habit_sql = format!(
        "{prefix}, seq AS (SELECT album_id,track_no,lag(album_id) OVER (ORDER BY played_at,id) prev_album,lag(track_no) OVER (ORDER BY played_at,id) prev_no,extract(epoch FROM (played_at-lag(played_at) OVER (ORDER BY played_at,id))) gap FROM p) SELECT count(*) FILTER (WHERE gap<900) moves, coalesce(round(100.0*count(*) FILTER (WHERE gap<900 AND album_id=prev_album)/nullif(count(*) FILTER (WHERE gap<900),0),1),0)::float8 same_album, coalesce(round(100.0*count(*) FILTER (WHERE gap<900 AND album_id=prev_album AND track_no=prev_no+1)/nullif(count(*) FILTER (WHERE gap<900),0),1),0)::float8 in_order FROM seq"
    );
    let era_sql = format!(
        "{prefix}, y AS (SELECT coalesce(a.display_year,a.year) yr FROM p JOIN albums a ON a.id=p.album_id WHERE coalesce(a.display_year,a.year) IS NOT NULL) SELECT CASE WHEN yr>=date_part('year',now())-1 THEN 'the last two years' WHEN yr>=2015 THEN '2015 to ' || (date_part('year',now())-2)::int WHEN yr>=2000 THEN '2000 to 2014' ELSE 'before 2000' END label, count(*) plays, (SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY yr) FROM y)::int median FROM y GROUP BY 1 ORDER BY min(yr) DESC"
    );
    let (total, heard, decades, concentration, habit, era) = tokio::try_join!(
        sqlx::query_as::<_, TallyRow>(&totals_sql)
            .bind(user)
            .fetch_one(pool),
        sqlx::query_as::<_, TallyRow>(&heard_sql)
            .bind(user)
            .fetch_one(pool),
        sqlx::query_as::<_, DecadeRow>(&decades_sql)
            .bind(user)
            .fetch_all(pool),
        sqlx::query_as::<_, Concentration>(&conc_sql)
            .bind(user)
            .fetch_one(pool),
        sqlx::query_as::<_, Habit>(&habit_sql)
            .bind(user)
            .fetch_one(pool),
        sqlx::query_as::<_, EraRow>(&era_sql)
            .bind(user)
            .fetch_all(pool),
    )?;
    Ok(Overview {
        total,
        heard,
        decades,
        concentration,
        habit,
        era,
    })
}
