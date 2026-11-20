//! The radio's library lists (M5b Task 11; upstream `services/mixes/radioRow.ts`).

use sqlx::PgPool;

/// A genre or decade becomes a station once it has this many library songs (upstream `MIN_STATION_SONGS`).
pub const MIN_STATION_SONGS: i64 = 15;

/// One decade of `GET /library/decades`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct Decade {
    pub decade: i32,
    pub count: i64,
}

/// Upstream `libraryDecades`: each album's songs count in the decade of its effective year (`getEffectiveYear`:
/// display year, else original year, else year; kima keeps no original year, D-272), a decade needs
/// [`MIN_STATION_SONGS`] songs, newest first. Songs are present tracks (D-276); an album with no year has no decade.
pub async fn library_decades(pool: &PgPool) -> sqlx::Result<Vec<Decade>> {
    sqlx::query_as(
        "SELECT (COALESCE(b.display_year, b.year) / 10 * 10)::int4 AS decade, count(t.id) AS count
         FROM albums b
         JOIN tracks t ON t.album_id = b.id AND t.missing_since IS NULL
         WHERE COALESCE(b.display_year, b.year) IS NOT NULL
         GROUP BY 1
         HAVING count(t.id) >= $1
         ORDER BY 1 DESC",
    )
    .bind(MIN_STATION_SONGS)
    .fetch_all(pool)
    .await
}
