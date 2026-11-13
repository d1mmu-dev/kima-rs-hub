//! Library search over the `search` tsvector columns (`simple` config, unaccented). Tracks
//! flagged missing are excluded, and so are albums and artists with no present track.

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::browse::{ALBUM_SELECT, ARTIST_COLUMNS, AlbumRow, ArtistRow, TRACK_SELECT, TrackRow};

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
    let sql = format!(
        "SELECT ar.id, ar.name, count(DISTINCT al.id) AS album_count, count(t.id) AS track_count,
             {ARTIST_COLUMNS}
         FROM artists ar
         JOIN albums al ON al.artist_id = ar.id
         JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
         WHERE ar.search @@ $1::tsquery
         GROUP BY ar.id
         ORDER BY ts_rank(ar.search, $1::tsquery) DESC, lower(ar.name) ASC, ar.id ASC
         LIMIT $2 OFFSET $3"
    );
    sqlx::query_as::<_, ArtistRow>(&sql)
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

/// A followed show matching a search (M6 Task 16, upstream `searchPodcastsFTS`): only shows
/// `user` follows are searched.
#[derive(sqlx::FromRow)]
pub struct PodcastSearchRow {
    pub id: Uuid,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub episode_count: i32,
}

pub async fn podcasts(
    pool: &PgPool,
    user: Uuid,
    query: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<PodcastSearchRow>> {
    sqlx::query_as(
        "SELECT p.id, p.title, p.author, p.description, p.image_url, p.episode_count
         FROM podcasts p
         JOIN podcast_subscriptions s ON s.podcast_id = p.id AND s.user_id = $1
         WHERE p.search @@ $2::tsquery
         ORDER BY ts_rank(p.search, $2::tsquery) DESC, lower(p.title) ASC, p.id ASC
         LIMIT $3 OFFSET $4",
    )
    .bind(user)
    .bind(query)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// An episode of a followed show matching a search (upstream `searchEpisodes`).
#[derive(sqlx::FromRow)]
pub struct EpisodeSearchRow {
    pub id: Uuid,
    pub podcast_id: Uuid,
    pub podcast_title: String,
    pub title: String,
    pub description: Option<String>,
    pub published_at: OffsetDateTime,
    pub duration: i32,
    pub audio_url: String,
}

pub async fn episodes(
    pool: &PgPool,
    user: Uuid,
    query: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<EpisodeSearchRow>> {
    sqlx::query_as(
        "SELECT e.id, e.podcast_id, p.title AS podcast_title, e.title, e.description,
                e.published_at, e.duration, e.audio_url
         FROM podcast_episodes e
         JOIN podcasts p ON p.id = e.podcast_id
         JOIN podcast_subscriptions s ON s.podcast_id = p.id AND s.user_id = $1
         WHERE e.search @@ $2::tsquery
         ORDER BY ts_rank(e.search, $2::tsquery) DESC, e.published_at DESC, e.id ASC
         LIMIT $3 OFFSET $4",
    )
    .bind(user)
    .bind(query)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}
