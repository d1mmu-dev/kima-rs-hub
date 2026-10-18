//! Library search over the `search` tsvector columns (`simple` config, unaccented). Tracks
//! flagged missing are excluded, and so are albums and artists with no present track.

use sqlx::PgPool;

use crate::browse::{ALBUM_SELECT, AlbumRow, ArtistRow, TRACK_SELECT, TrackRow};

/// Turns user text into a prefix tsquery string: `websearch_to_tsquery('simple', unaccent(q))`
/// with `:*` on the last term when it is a plain lexeme. `None` when the text has no searchable term.
pub async fn prefix_query(pool: &PgPool, q: &str) -> sqlx::Result<Option<String>> {
    let text: String =
        sqlx::query_scalar("SELECT websearch_to_tsquery('simple', unaccent($1))::text")
            .bind(q)
            .fetch_one(pool)
            .await?;
    // `:*` is only valid right after a lexeme; a query ending in `)` is used as is.
    Ok(match text {
        t if t.is_empty() => None,
        t if t.ends_with('\'') => Some(format!("{t}:*")),
        t => Some(t),
    })
}

pub async fn artists(
    pool: &PgPool,
    query: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<ArtistRow>> {
    sqlx::query_as::<_, ArtistRow>(
        "SELECT ar.id, ar.name, count(DISTINCT al.id) AS album_count, count(t.id) AS track_count
         FROM artists ar
         JOIN albums al ON al.artist_id = ar.id
         JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
         WHERE ar.search @@ $1::tsquery
         GROUP BY ar.id
         ORDER BY ts_rank(ar.search, $1::tsquery) DESC, lower(ar.name) ASC, ar.id ASC
         LIMIT $2 OFFSET $3",
    )
    .bind(query)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

pub async fn albums(
    pool: &PgPool,
    query: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<AlbumRow>> {
    let sql = format!(
        "{ALBUM_SELECT} WHERE al.search @@ $1::tsquery GROUP BY al.id, ar.id
         ORDER BY ts_rank(al.search, $1::tsquery) DESC, lower(al.title) ASC, al.id ASC
         LIMIT $2 OFFSET $3"
    );
    sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(query)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
}

pub async fn tracks(
    pool: &PgPool,
    query: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<TrackRow>> {
    let sql = format!(
        "{TRACK_SELECT} AND t.search @@ $1::tsquery
         ORDER BY ts_rank(t.search, $1::tsquery) DESC, lower(t.title) ASC, t.id ASC
         LIMIT $2 OFFSET $3"
    );
    sqlx::query_as::<_, TrackRow>(&sql)
        .bind(query)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
}
