//! Artists, albums and tracks: upserts keyed on identity, missing-file tracking.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

/// Which fields of a track came from fallbacks rather than tags. Shared by the scanner
/// and the needs-attention report; serialised with camelCase keys.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Guessed {
    pub artist_from_path: bool,
    pub album_from_path: bool,
    pub year_from_path: bool,
    pub track_from_name: bool,
    pub title_from_name: bool,
    pub unknown_artist: bool,
    pub unknown_album: bool,
    /// This track's own year differs from its album's chosen year.
    pub year_conflict: bool,
}

/// A track with every field resolved, ready to store. The `*_key` fields are the
/// normalised identity keys (NFC, trimmed, whitespace-collapsed, lowercase), computed
/// by the caller.
#[derive(Debug, Clone)]
pub struct ResolvedTrack {
    pub album_artist: String,
    pub album_artist_key: String,
    pub track_artist: String,
    pub album: String,
    pub album_key: String,
    /// The album's chosen year (part of album identity).
    pub album_year: Option<i32>,
    /// This track's own tagged or folder year.
    pub track_year: Option<i32>,
    pub is_compilation: bool,
    pub disc: i32,
    pub track_no: i32,
    pub title: String,
    pub duration_secs: i32,
    pub codec: String,
    pub bitrate: Option<i32>,
    pub path: String,
    pub size: i64,
    pub mtime: OffsetDateTime,
    pub fingerprint: [u8; 32],
    pub guessed: Guessed,
}

#[derive(Debug, Clone)]
pub struct TrackRow {
    pub id: Uuid,
    pub album_id: Uuid,
    pub artist_name: String,
    pub title: String,
    pub year: Option<i32>,
    pub disc: i32,
    pub track_no: i32,
    pub duration_secs: i32,
    pub codec: String,
    pub bitrate: Option<i32>,
    pub path: String,
    pub size: i64,
    pub mtime: OffsetDateTime,
    pub fingerprint: Vec<u8>,
    pub guessed: Json<Guessed>,
    pub missing_since: Option<OffsetDateTime>,
}

/// A spelling with both upper and lower case letters beats an all-lower or all-upper one.
fn is_well_capitalised(s: &str) -> bool {
    s.chars().any(char::is_lowercase) && s.chars().any(char::is_uppercase)
}

/// Finds or creates the artist and album, then upserts the track by path. A re-seen
/// path clears `missing_since`. Returns the track id.
pub async fn upsert_track(tx: &mut PgConnection, t: &ResolvedTrack) -> sqlx::Result<Uuid> {
    let artist_id = sqlx::query_scalar!(
        r#"INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $3)
           ON CONFLICT (name_key) DO UPDATE
           SET name = CASE WHEN $4 AND NOT (artists.name <> lower(artists.name)
                                            AND artists.name <> upper(artists.name))
                           THEN EXCLUDED.name ELSE artists.name END
           RETURNING id"#,
        Uuid::now_v7(),
        t.album_artist,
        t.album_artist_key,
        is_well_capitalised(&t.album_artist)
    )
    .fetch_one(&mut *tx)
    .await?;

    let album_id = sqlx::query_scalar!(
        r#"INSERT INTO albums (id, artist_id, title, title_key, year, is_compilation)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (artist_id, title_key, (COALESCE(year, -1))) DO UPDATE
           SET is_compilation = albums.is_compilation OR EXCLUDED.is_compilation
           RETURNING id"#,
        Uuid::now_v7(),
        artist_id,
        t.album,
        t.album_key,
        t.album_year,
        t.is_compilation
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query_scalar!(
        r#"INSERT INTO tracks (id, album_id, artist_name, title, year, disc, track_no, duration_secs,
                               codec, bitrate, path, size, mtime, fingerprint, guessed)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
           ON CONFLICT (path) DO UPDATE
           SET album_id = EXCLUDED.album_id, artist_name = EXCLUDED.artist_name,
               title = EXCLUDED.title, year = EXCLUDED.year, disc = EXCLUDED.disc, track_no = EXCLUDED.track_no,
               duration_secs = EXCLUDED.duration_secs, codec = EXCLUDED.codec,
               bitrate = EXCLUDED.bitrate, size = EXCLUDED.size, mtime = EXCLUDED.mtime,
               fingerprint = EXCLUDED.fingerprint, guessed = EXCLUDED.guessed,
               missing_since = NULL, updated_at = now()
           RETURNING id"#,
        Uuid::now_v7(),
        album_id,
        t.track_artist,
        t.title,
        t.track_year,
        t.disc,
        t.track_no,
        t.duration_secs,
        t.codec,
        t.bitrate,
        t.path,
        t.size,
        t.mtime,
        t.fingerprint.as_slice(),
        Json(&t.guessed) as Json<&Guessed>,
    )
    .fetch_one(&mut *tx)
    .await
}

pub async fn find_by_path(pool: &PgPool, path: &str) -> sqlx::Result<Option<TrackRow>> {
    sqlx::query_as!(
        TrackRow,
        r#"SELECT id, album_id, artist_name, title, year, disc, track_no, duration_secs, codec,
                  bitrate, path, size, mtime, fingerprint,
                  guessed AS "guessed: Json<Guessed>", missing_since
           FROM tracks WHERE path = $1"#,
        path
    )
    .fetch_optional(pool)
    .await
}

/// Marks the given paths missing (keeping the original time for those already missing).
/// Returns how many rows changed.
pub async fn mark_missing(pool: &PgPool, paths: &[String]) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "UPDATE tracks SET missing_since = now()
         WHERE path = ANY($1) AND missing_since IS NULL",
        paths
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Moves a missing track with this fingerprint to `new_path`, clearing its missing flag.
/// Only missing tracks match, and nothing happens (`None`) if `new_path` already has a row. Returns the track id.
pub async fn repoint_by_fingerprint(
    pool: &PgPool,
    fingerprint: &[u8; 32],
    new_path: &str,
) -> sqlx::Result<Option<Uuid>> {
    let mut conn = pool.acquire().await?;
    repoint_by_fingerprint_in(&mut conn, fingerprint, new_path).await
}

/// [`repoint_by_fingerprint`] on a caller's connection or transaction, so the re-point and
/// the metadata upsert that follows commit or roll back together.
pub async fn repoint_by_fingerprint_in(
    conn: &mut PgConnection,
    fingerprint: &[u8; 32],
    new_path: &str,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!(
        r#"UPDATE tracks SET path = $2, missing_since = NULL, updated_at = now()
           WHERE id = (SELECT id FROM tracks
                       WHERE fingerprint = $1 AND missing_since IS NOT NULL
                       ORDER BY missing_since, id LIMIT 1 FOR UPDATE SKIP LOCKED)
             AND NOT EXISTS (SELECT 1 FROM tracks WHERE path = $2)
           RETURNING id"#,
        fingerprint.as_slice(),
        new_path
    )
    .fetch_optional(&mut *conn)
    .await
}

/// The album of the track at `path`, if there is one.
pub async fn album_of_path(conn: &mut PgConnection, path: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!("SELECT album_id FROM tracks WHERE path = $1", path)
        .fetch_optional(&mut *conn)
        .await
}

/// Deletes tracks missing for longer than `older_than`. Returns how many.
pub async fn purge_missing(pool: &PgPool, older_than: Duration) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "DELETE FROM tracks
         WHERE missing_since IS NOT NULL
           AND missing_since < now() - make_interval(secs => $1::float8)",
        older_than.as_secs_f64()
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Deletes albums with no tracks, then artists with no albums, in one transaction.
/// Must not run concurrently with upserts (the FKs are RESTRICT, so a race fails loudly
/// instead of cascading); the `library` queue serialises it.
pub async fn prune_empty(pool: &PgPool) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM albums a WHERE NOT EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = a.id)"
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM artists a WHERE NOT EXISTS (SELECT 1 FROM albums b WHERE b.artist_id = a.id)"
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Paths (relative to the library root) of the album's present tracks, in play order.
pub async fn album_track_paths(pool: &PgPool, album_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT path FROM tracks
         WHERE album_id = $1 AND missing_since IS NULL
         ORDER BY disc, track_no, path",
        album_id
    )
    .fetch_all(pool)
    .await
}

/// What the scanner needs to know about a stored track to decide whether its file changed
/// and which album group it belongs to.
#[derive(Debug, Clone)]
pub struct IndexedTrack {
    pub id: Uuid,
    pub path: String,
    pub size: i64,
    pub mtime: OffsetDateTime,
    pub missing: bool,
    /// The track's own year.
    pub track_year: Option<i32>,
    /// The year of the album the track is on.
    pub album_year: Option<i32>,
    pub album_key: String,
    pub artist_key: String,
    pub year_conflict: bool,
}

/// Every track (`dir_prefix = None`), or only the tracks directly inside one folder:
/// `Some("Artist/Album/")`, or `Some("")` for files at the library root.
pub async fn scan_index(
    pool: &PgPool,
    dir_prefix: Option<&str>,
) -> sqlx::Result<Vec<IndexedTrack>> {
    sqlx::query_as!(
        IndexedTrack,
        r#"SELECT t.id, t.path, t.size, t.mtime,
                  t.missing_since IS NOT NULL AS "missing!",
                  t.year AS track_year, b.year AS album_year,
                  b.title_key AS album_key, a.name_key AS artist_key,
                  COALESCE((t.guessed ->> 'yearConflict')::boolean, false) AS "year_conflict!"
           FROM tracks t
           JOIN albums b ON b.id = t.album_id
           JOIN artists a ON a.id = b.artist_id
           WHERE $1::text IS NULL
              OR (starts_with(t.path, $1) AND strpos(substr(t.path, length($1) + 1), '/') = 0)"#,
        dir_prefix
    )
    .fetch_all(pool)
    .await
}

/// Paths of the present (not missing) tracks anywhere under the folder `dir_prefix`
/// (`"Artist/Album/"`, with the trailing slash). Used to expand a deleted or renamed folder.
pub async fn paths_under(pool: &PgPool, dir_prefix: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT path FROM tracks WHERE starts_with(path, $1) AND missing_since IS NULL",
    )
    .bind(dir_prefix)
    .fetch_all(pool)
    .await
}

/// Clears the missing flag of tracks whose file is back at the same path, unchanged.
/// Returns how many rows changed.
pub async fn clear_missing(pool: &PgPool, paths: &[String]) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "UPDATE tracks SET missing_since = NULL, updated_at = now()
         WHERE path = ANY($1) AND missing_since IS NOT NULL",
        paths
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}
