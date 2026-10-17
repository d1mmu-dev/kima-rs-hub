//! Read-only listings for the browse API. Tracks flagged missing are excluded everywhere, and
//! so are albums and artists left with no present track.

use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    NameAsc,
    NameDesc,
    /// Artists: most tracks first. Albums: newest year first.
    Recent,
}

#[derive(Debug, Clone, FromRow)]
pub struct ArtistRow {
    pub id: Uuid,
    pub name: String,
    pub album_count: i64,
    pub track_count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct AlbumRow {
    pub id: Uuid,
    pub title: String,
    pub year: Option<i32>,
    pub is_compilation: bool,
    pub created_at: OffsetDateTime,
    pub artist_id: Uuid,
    pub artist_name: String,
    pub track_count: i64,
    pub duration_secs: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct TrackRow {
    pub id: Uuid,
    pub title: String,
    pub artist_name: String,
    pub duration_secs: i32,
    pub disc: i32,
    pub track_no: i32,
    pub codec: String,
    pub bitrate: Option<i32>,
    pub year: Option<i32>,
    pub album_id: Uuid,
    pub album_title: String,
    pub album_year: Option<i32>,
    pub album_artist_id: Uuid,
    pub album_artist_name: String,
}

pub(crate) const TRACK_SELECT: &str = "SELECT t.id, t.title, t.artist_name, t.duration_secs, t.disc, t.track_no,
        t.codec, t.bitrate, t.year, al.id AS album_id, al.title AS album_title, al.year AS album_year,
        ar.id AS album_artist_id, ar.name AS album_artist_name
    FROM tracks t
    JOIN albums al ON al.id = t.album_id
    JOIN artists ar ON ar.id = al.artist_id
    WHERE t.missing_since IS NULL";

pub(crate) const ALBUM_SELECT: &str =
    "SELECT al.id, al.title, al.year, al.is_compilation, al.created_at,
        ar.id AS artist_id, ar.name AS artist_name,
        count(t.id) AS track_count, COALESCE(sum(t.duration_secs), 0)::bigint AS duration_secs
    FROM albums al
    JOIN artists ar ON ar.id = al.artist_id
    JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL";

/// `%` and `_` in a user's text must match themselves in `ILIKE`.
fn like_pattern(q: &str) -> String {
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Artists that have at least one present track, with their album and track counts.
/// Returns the page and the total.
pub async fn artists(
    pool: &PgPool,
    query: Option<&str>,
    sort: Sort,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<ArtistRow>, i64)> {
    let pattern = query.filter(|q| !q.is_empty()).map(like_pattern);
    let order = match sort {
        Sort::NameAsc => "lower(ar.name) ASC, ar.id ASC",
        Sort::NameDesc => "lower(ar.name) DESC, ar.id ASC",
        Sort::Recent => "track_count DESC, lower(ar.name) ASC, ar.id ASC",
    };
    let sql = format!(
        "SELECT ar.id, ar.name, count(DISTINCT al.id) AS album_count, count(t.id) AS track_count
         FROM artists ar
         JOIN albums al ON al.artist_id = ar.id
         JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
         WHERE ($1::text IS NULL OR ar.name ILIKE $1)
         GROUP BY ar.id
         ORDER BY {order}
         LIMIT $2 OFFSET $3"
    );
    let rows = sqlx::query_as::<_, ArtistRow>(&sql)
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM artists ar
         WHERE ($1::text IS NULL OR ar.name ILIKE $1)
           AND EXISTS (SELECT 1 FROM albums al JOIN tracks t ON t.album_id = al.id
                       WHERE al.artist_id = ar.id AND t.missing_since IS NULL)",
    )
    .bind(&pattern)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

pub async fn artist(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<ArtistRow>> {
    sqlx::query_as::<_, ArtistRow>(
        "SELECT ar.id, ar.name, count(DISTINCT al.id) AS album_count, count(t.id) AS track_count
         FROM artists ar
         JOIN albums al ON al.artist_id = ar.id
         JOIN tracks t ON t.album_id = al.id AND t.missing_since IS NULL
         WHERE ar.id = $1
         GROUP BY ar.id",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// An artist's albums, oldest year first (no year last), then by title.
pub async fn artist_albums(pool: &PgPool, artist: Uuid) -> sqlx::Result<Vec<AlbumRow>> {
    let sql = format!(
        "{ALBUM_SELECT} WHERE al.artist_id = $1 GROUP BY al.id, ar.id
         ORDER BY al.year ASC NULLS LAST, lower(al.title) ASC, al.id ASC"
    );
    sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(artist)
        .fetch_all(pool)
        .await
}

/// Albums with at least one present track, optionally of one artist. Returns the page and total.
pub async fn albums(
    pool: &PgPool,
    artist: Option<Uuid>,
    sort: Sort,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<AlbumRow>, i64)> {
    let order = match sort {
        Sort::NameAsc => "lower(al.title) ASC, al.id ASC",
        Sort::NameDesc => "lower(al.title) DESC, al.id ASC",
        Sort::Recent => "al.year DESC NULLS LAST, lower(al.title) ASC, al.id ASC",
    };
    let sql = format!(
        "{ALBUM_SELECT} WHERE ($1::uuid IS NULL OR al.artist_id = $1) GROUP BY al.id, ar.id
         ORDER BY {order} LIMIT $2 OFFSET $3"
    );
    let rows = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(artist)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM albums al
         WHERE ($1::uuid IS NULL OR al.artist_id = $1)
           AND EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = al.id AND t.missing_since IS NULL)",
    )
    .bind(artist)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

pub async fn album(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<AlbumRow>> {
    let sql = format!("{ALBUM_SELECT} WHERE al.id = $1 GROUP BY al.id, ar.id");
    sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// The most recently added albums, newest first.
pub async fn recent_albums(pool: &PgPool, limit: i64) -> sqlx::Result<Vec<AlbumRow>> {
    let sql = format!(
        "{ALBUM_SELECT} GROUP BY al.id, ar.id ORDER BY al.created_at DESC, al.id DESC LIMIT $1"
    );
    sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// Present tracks of the given albums, by disc then track number.
pub async fn tracks_of_albums(pool: &PgPool, albums: &[Uuid]) -> sqlx::Result<Vec<TrackRow>> {
    let sql = format!(
        "{TRACK_SELECT} AND t.album_id = ANY($1)
         ORDER BY t.disc ASC, t.track_no ASC, lower(t.title) ASC, t.id ASC"
    );
    sqlx::query_as::<_, TrackRow>(&sql)
        .bind(albums)
        .fetch_all(pool)
        .await
}

/// Present tracks, optionally of one album (then by disc and track number, else by title).
pub async fn tracks(
    pool: &PgPool,
    album: Option<Uuid>,
    sort: Sort,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<TrackRow>, i64)> {
    let order = if album.is_some() {
        "t.disc ASC, t.track_no ASC, t.id ASC"
    } else if sort == Sort::NameDesc {
        "lower(t.title) DESC, t.id ASC"
    } else {
        "lower(t.title) ASC, t.id ASC"
    };
    let sql = format!(
        "{TRACK_SELECT} AND ($1::uuid IS NULL OR t.album_id = $1) ORDER BY {order} LIMIT $2 OFFSET $3"
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(album)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tracks t
         WHERE t.missing_since IS NULL AND ($1::uuid IS NULL OR t.album_id = $1)",
    )
    .bind(album)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

pub async fn track(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<TrackRow>> {
    let sql = format!("{TRACK_SELECT} AND t.id = $1");
    sqlx::query_as::<_, TrackRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Up to `limit` random present tracks, and how many present tracks there are.
pub async fn shuffle(pool: &PgPool, limit: i64) -> sqlx::Result<(Vec<TrackRow>, i64)> {
    let sql = format!("{TRACK_SELECT} ORDER BY random() LIMIT $1");
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks WHERE missing_since IS NULL")
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}

/// A job row as the scan-status endpoint needs it.
#[derive(Debug, Clone, FromRow)]
pub struct JobStatus {
    pub state: String,
    pub last_error: Option<String>,
}

pub async fn job_status(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<JobStatus>> {
    sqlx::query_as::<_, JobStatus>(
        "SELECT state, last_error FROM jobs WHERE id = $1 AND queue = 'library'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}
