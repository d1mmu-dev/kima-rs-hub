//! `GET /api/recommendations/for-you` (M5b Task 18; upstream `routes/recommendations.ts` at the M5b reference
//! `9450c94b`): the reads behind it. Similar artists are the stored `artists."similar"` list (D-113); "owned" is an
//! artist with a present library track until M7 adds album ownership (D-271).

use std::collections::{HashMap, HashSet};

use kima_core::profile::ProfileId;
use sqlx::PgPool;
use uuid::Uuid;

/// Upstream `take: 50`: the plays the seeds are counted from.
pub const RECENT_PLAYS: i64 = 50;
/// Upstream `.slice(0, 3)`: the seed artists.
pub const SEED_ARTISTS: i64 = 3;

/// The profile's seed artists with their stored similar list: the album artists of its last 50 plays, most played
/// first, a tie in the order first met (newest play first: upstream's stable sort over a `Map`), at most three.
pub async fn seed_artists(
    pool: &PgPool,
    profile: ProfileId,
) -> sqlx::Result<Vec<(Uuid, Option<serde_json::Value>)>> {
    sqlx::query_as(
        r#"WITH recent AS (
               SELECT p.track_id, row_number() OVER (ORDER BY p.played_at DESC, p.id DESC) AS rn
               FROM plays p WHERE p.profile_id = $1
               ORDER BY p.played_at DESC, p.id DESC LIMIT $2),
           counted AS (
               SELECT al.artist_id, count(*) AS n, min(r.rn) AS first
               FROM recent r JOIN tracks t ON t.id = r.track_id JOIN albums al ON al.id = t.album_id
               GROUP BY al.artist_id)
           SELECT c.artist_id, ar."similar"
           FROM counted c JOIN artists ar ON ar.id = c.artist_id
           ORDER BY c.n DESC, c.first
           LIMIT $3"#,
    )
    .bind(profile)
    .bind(RECENT_PLAYS)
    .bind(SEED_ARTISTS)
    .fetch_all(pool)
    .await
}

/// Of `artists`, those the library owns: an artist with a present track (kima's "owned" until M7, D-271).
pub async fn owned(pool: &PgPool, artists: &[Uuid]) -> sqlx::Result<HashSet<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT al.artist_id FROM albums al JOIN tracks t ON t.album_id = al.id
         WHERE al.artist_id = ANY($1) AND t.missing_since IS NULL",
    )
    .bind(artists)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect())
}

/// Upstream `album.groupBy({ by: artistId, _count: { rgMbid } })`: each artist's albums with a release group.
pub async fn album_counts(pool: &PgPool, artists: &[Uuid]) -> sqlx::Result<HashMap<Uuid, i64>> {
    Ok(sqlx::query_as::<_, (Uuid, i64)>(
        "SELECT artist_id, count(release_group_mbid) FROM albums WHERE artist_id = ANY($1) GROUP BY artist_id",
    )
    .bind(artists)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect())
}
