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
    /// The file was indexed through a symlink (itself or a folder above it): the owner should
    /// replace the link with the real file (needs-attention `symlink`). Set by the scanner,
    /// never by tags, so a re-read of the real file clears it.
    pub via_symlink: bool,
    /// What the file named as its album's artist when it was read: the sibling's vote in the
    /// album-artist election (D-164). Absent on rows written before the election.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit: Option<StoredCredit>,
}

/// One file's vote for its album's artist, kept in `guessed.credit` so a changed file can be
/// elected with its unchanged siblings without re-reading them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCredit {
    /// Identity key of the album tag; `None` for a song with no album tag (its own album).
    pub album_key: Option<String>,
    pub name: String,
    pub mbid: Option<Uuid>,
    /// The name came from an album-artist tag.
    pub from_album_artist: bool,
    /// The file is tagged a compilation.
    pub compilation: bool,
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
    /// MusicBrainz ids as read from the file (spec §8.1).
    pub ids: TagIds,
    /// Normalised; stored with `isrc_source = 'id3'`.
    pub isrc: Option<String>,
    pub disc_total: Option<i32>,
    pub track_total: Option<i32>,
    /// The album artist's MBID by spec §8.1's artist rule, decided by the scanner.
    pub artist_mbid_candidate: Option<Uuid>,
}

/// The MusicBrainz ids a file carries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TagIds {
    pub recording: Option<Uuid>,
    pub release_track: Option<Uuid>,
    pub release: Option<Uuid>,
    pub release_group: Option<Uuid>,
    pub artists: Vec<Uuid>,
    pub album_artists: Vec<Uuid>,
}

/// What [`upsert_track`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Upserted {
    pub track: Uuid,
    pub album: Uuid,
    pub artist: Uuid,
    /// The row existed and its stored content fingerprint differed.
    pub content_changed: bool,
    /// The album row was created by this upsert.
    pub album_created: bool,
}

/// MusicBrainz's Various Artists id; never looked up.
pub const VARIOUS_ARTISTS_MBID: Uuid = uuid::uuid!("89ad4ac3-39f7-470e-963a-56509c546377");
const VARIOUS_ARTISTS_KEY: &str = "various artists";

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

/// Finds or creates the artist and album, then upserts the track by path, with its tag ids,
/// ISRC and totals. A re-seen path clears `missing_since`. A file ISRC is stored with
/// `isrc_source = 'id3'`; a file without one clears an `id3` value and keeps any other. A new
/// Various Artists row is created with its MBID and `enrichment_status = 'completed'`.
pub async fn upsert_track(tx: &mut PgConnection, t: &ResolvedTrack) -> sqlx::Result<Upserted> {
    let va = t.album_artist_key == VARIOUS_ARTISTS_KEY;
    let artist_id = sqlx::query_scalar!(
        r#"INSERT INTO artists (id, name, name_key, mbid, enrichment_status) VALUES ($1, $2, $3, $5, $6)
           ON CONFLICT (name_key) DO UPDATE
           SET name = CASE WHEN $4 AND NOT (artists.name <> lower(artists.name)
                                            AND artists.name <> upper(artists.name))
                           THEN EXCLUDED.name ELSE artists.name END
           RETURNING id"#,
        Uuid::now_v7(),
        t.album_artist,
        t.album_artist_key,
        is_well_capitalised(&t.album_artist),
        va.then_some(VARIOUS_ARTISTS_MBID),
        if va { "completed" } else { "pending" }
    )
    .fetch_one(&mut *tx)
    .await?;

    let album = sqlx::query!(
        r#"INSERT INTO albums (id, artist_id, title, title_key, year, is_compilation)
           VALUES ($1, $2, $3, $4, $5, $6)
           ON CONFLICT (artist_id, title_key, (COALESCE(year, -1))) DO UPDATE
           SET is_compilation = albums.is_compilation OR EXCLUDED.is_compilation
           RETURNING id, (xmax = 0) AS "created!""#,
        Uuid::now_v7(),
        artist_id,
        t.album,
        t.album_key,
        t.album_year,
        t.is_compilation
    )
    .fetch_one(&mut *tx)
    .await?;
    let (track_id, content_changed) = upsert_track_row(tx, t, album.id).await?;
    Ok(Upserted {
        track: track_id,
        album: album.id,
        artist: artist_id,
        content_changed,
        album_created: album.created,
    })
}

/// The album identity an autotag apply set (`albums.applied_identity`, spec §19.7): the keys
/// and year the scanner derives from the applied values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedIdentity {
    pub artist_key: String,
    pub title_key: String,
    #[serde(default)]
    pub year: Option<i32>,
}

/// The album's applied identity, only while `identity_source = 'autotag'`. A stored value that
/// does not parse counts as none (the track is then resolved normally). An autotagged row is
/// locked (`FOR NO KEY UPDATE`) for the rest of the caller's transaction, so an undo or re-apply
/// cannot change the identity between this read and a pinned upsert. The scanner calls it after
/// locking the track row (`album_of_path`), keeping the track-then-album lock order.
pub async fn autotag_identity(
    conn: &mut PgConnection,
    album: Uuid,
) -> sqlx::Result<Option<AppliedIdentity>> {
    let v = sqlx::query_scalar!(
        "SELECT applied_identity FROM albums WHERE id = $1 AND identity_source = 'autotag'
         FOR NO KEY UPDATE",
        album
    )
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    Ok(v.and_then(|v| match serde_json::from_value(v) {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(album = %album, error = %e, "unreadable applied_identity; ignored");
            None
        }
    }))
}

/// [`upsert_track`] onto a known album row (an autotagged album the track agrees with, spec
/// §19.7): no artist or album upsert, so the row's title, year and compilation flag stay as
/// the apply set them.
pub async fn upsert_track_pinned(
    tx: &mut PgConnection,
    t: &ResolvedTrack,
    album: Uuid,
) -> sqlx::Result<Upserted> {
    let artist = sqlx::query_scalar!("SELECT artist_id FROM albums WHERE id = $1", album)
        .fetch_one(&mut *tx)
        .await?;
    let (track, content_changed) = upsert_track_row(tx, t, album).await?;
    Ok(Upserted {
        track,
        album,
        artist,
        content_changed,
        album_created: false,
    })
}

/// Albums among `albums` with `identity_source = 'autotag'` and no present track go back to
/// `scan` with `applied_identity = NULL` (no track agrees with the applied identity any more,
/// spec §19.7). Returns how many.
pub async fn release_autotag_albums(conn: &mut PgConnection, albums: &[Uuid]) -> sqlx::Result<u64> {
    if albums.is_empty() {
        return Ok(0);
    }
    let r = sqlx::query!(
        "UPDATE albums a SET identity_source = 'scan', applied_identity = NULL
         WHERE a.id = ANY($1) AND a.identity_source = 'autotag'
           AND NOT EXISTS (SELECT 1 FROM tracks t
                           WHERE t.album_id = a.id AND t.missing_since IS NULL)",
        albums
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// The track row of [`upsert_track`] on `album_id`: answers the track id and whether its stored
/// content fingerprint differed.
async fn upsert_track_row(
    tx: &mut PgConnection,
    t: &ResolvedTrack,
    album_id: Uuid,
) -> sqlx::Result<(Uuid, bool)> {
    let previous: Option<Vec<u8>> =
        sqlx::query_scalar!("SELECT fingerprint FROM tracks WHERE path = $1", t.path)
            .fetch_optional(&mut *tx)
            .await?;

    let track_id = sqlx::query_scalar!(
        r#"INSERT INTO tracks (id, album_id, artist_name, title, year, disc, track_no, duration_secs,
                               codec, bitrate, path, size, mtime, fingerprint, guessed,
                               tag_recording_mbid, tag_release_track_mbid, tag_release_mbid,
                               tag_release_group_mbid, tag_artist_mbids, tag_album_artist_mbids,
                               disc_total, track_total, isrc, isrc_source)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                   $16, $17, $18, $19, $20, $21, $22, $23, $24,
                   CASE WHEN $24::text IS NULL THEN NULL ELSE 'id3' END)
           ON CONFLICT (path) DO UPDATE
           SET album_id = EXCLUDED.album_id, artist_name = EXCLUDED.artist_name,
               title = EXCLUDED.title, year = EXCLUDED.year, disc = EXCLUDED.disc, track_no = EXCLUDED.track_no,
               duration_secs = EXCLUDED.duration_secs, codec = EXCLUDED.codec,
               bitrate = EXCLUDED.bitrate, size = EXCLUDED.size, mtime = EXCLUDED.mtime,
               fingerprint = EXCLUDED.fingerprint, guessed = EXCLUDED.guessed,
               tag_recording_mbid = EXCLUDED.tag_recording_mbid,
               tag_release_track_mbid = EXCLUDED.tag_release_track_mbid,
               tag_release_mbid = EXCLUDED.tag_release_mbid,
               tag_release_group_mbid = EXCLUDED.tag_release_group_mbid,
               tag_artist_mbids = EXCLUDED.tag_artist_mbids,
               tag_album_artist_mbids = EXCLUDED.tag_album_artist_mbids,
               disc_total = EXCLUDED.disc_total, track_total = EXCLUDED.track_total,
               isrc = CASE WHEN EXCLUDED.isrc IS NOT NULL THEN EXCLUDED.isrc
                           WHEN tracks.isrc_source = 'id3' THEN NULL
                           ELSE tracks.isrc END,
               isrc_source = CASE WHEN EXCLUDED.isrc IS NOT NULL THEN EXCLUDED.isrc_source
                                  WHEN tracks.isrc_source = 'id3' THEN NULL
                                  ELSE tracks.isrc_source END,
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
        t.ids.recording,
        t.ids.release_track,
        t.ids.release,
        t.ids.release_group,
        &t.ids.artists,
        &t.ids.album_artists,
        t.disc_total,
        t.track_total,
        t.isrc,
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok((
        track_id,
        previous.is_some_and(|f| f.as_slice() != t.fingerprint.as_slice()),
    ))
}

/// Sets the album's `release_mbid` and `release_group_mbid` to the mode of its present tracks'
/// tag values, ties to the smallest UUID. Each is written (with source `tag`) only while the
/// stored source is NULL or `tag`: an `autotag` or `user` value wins over tags (R2). The
/// release-group id also yields to a tag while its source is `acoustid` (the weakest, Task 11);
/// with no tag id left an `acoustid` one stays, a `tag` one goes.
pub async fn refresh_album_mbids(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        r#"WITH rel AS (
               SELECT tag_release_mbid AS m FROM tracks
               WHERE album_id = $1 AND missing_since IS NULL AND tag_release_mbid IS NOT NULL
               GROUP BY tag_release_mbid ORDER BY count(*) DESC, tag_release_mbid LIMIT 1),
           rg AS (
               SELECT tag_release_group_mbid AS m FROM tracks
               WHERE album_id = $1 AND missing_since IS NULL AND tag_release_group_mbid IS NOT NULL
               GROUP BY tag_release_group_mbid ORDER BY count(*) DESC, tag_release_group_mbid LIMIT 1)
           UPDATE albums SET
               release_mbid = CASE WHEN release_mbid_source IS NULL OR release_mbid_source = 'tag'
                                   THEN (SELECT m FROM rel) ELSE release_mbid END,
               release_mbid_source = CASE WHEN release_mbid_source IS NULL OR release_mbid_source = 'tag'
                                          THEN CASE WHEN EXISTS (SELECT 1 FROM rel) THEN 'tag' END
                                          ELSE release_mbid_source END,
               release_group_mbid = CASE
                   WHEN release_group_mbid_source IS NULL OR release_group_mbid_source = 'tag'
                       THEN (SELECT m FROM rg)
                   WHEN release_group_mbid_source = 'acoustid'
                       THEN coalesce((SELECT m FROM rg), release_group_mbid)
                   ELSE release_group_mbid END,
               release_group_mbid_source = CASE
                   WHEN release_group_mbid_source IS NULL OR release_group_mbid_source = 'tag'
                       THEN CASE WHEN EXISTS (SELECT 1 FROM rg) THEN 'tag' END
                   WHEN release_group_mbid_source = 'acoustid'
                       THEN CASE WHEN EXISTS (SELECT 1 FROM rg) THEN 'tag' ELSE 'acoustid' END
                   ELSE release_group_mbid_source END
           WHERE id = $1"#,
        album
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Sets the artist's MBID from tags (`mbid_source = 'tag'`) only while it has none. Returns
/// whether it was set.
pub async fn set_artist_mbid_if_unset(
    conn: &mut PgConnection,
    artist: Uuid,
    mbid: Uuid,
) -> sqlx::Result<bool> {
    let r = sqlx::query!(
        "UPDATE artists SET mbid = $2, mbid_source = 'tag' WHERE id = $1 AND mbid IS NULL",
        artist,
        mbid
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Puts the given `completed` artists back to `pending`, Various Artists aside, so a new album
/// gets its cover fallback (final review I1). The rows are locked in id order. Answers how many
/// were reopened.
pub async fn reopen_completed_artists(
    conn: &mut PgConnection,
    artists: &[Uuid],
) -> sqlx::Result<u64> {
    if artists.is_empty() {
        return Ok(0);
    }
    let r = sqlx::query!(
        r#"WITH locked AS (
               SELECT id FROM artists
               WHERE id = ANY($1) AND enrichment_status = 'completed' AND name_key <> $2
               ORDER BY id
               FOR NO KEY UPDATE)
           UPDATE artists a SET enrichment_status = 'pending', cover_deferrals = 0
           FROM locked WHERE a.id = locked.id AND a.enrichment_status = 'completed'"#,
        artists,
        VARIOUS_ARTISTS_KEY
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// The artists of the given albums.
pub async fn artists_of_albums(
    conn: &mut PgConnection,
    albums: &[Uuid],
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar!(
        "SELECT DISTINCT artist_id FROM albums WHERE id = ANY($1)",
        albums
    )
    .fetch_all(&mut *conn)
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
    let mut conn = pool.acquire().await?;
    mark_missing_in(&mut conn, paths).await
}

/// [`mark_missing`] on a connection, so it can share a transaction.
/// Lock by id before updating: an unordered multi-row UPDATE can lock B then A while
/// a playlist's `add_locked` holds A then waits for B, deadlocking the request.
pub async fn mark_missing_in(conn: &mut PgConnection, paths: &[String]) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "WITH locked AS MATERIALIZED (
             SELECT id FROM tracks WHERE path = ANY($1) AND missing_since IS NULL
             ORDER BY id FOR NO KEY UPDATE
         )
         UPDATE tracks t SET missing_since = now()
         FROM locked WHERE t.id = locked.id",
        paths
    )
    .execute(conn)
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

/// Moves exactly this missing track to `new_path`, clearing its missing flag; waits for the
/// row's lock (no `SKIP LOCKED`), so the scanner never stores a file under another track than
/// the one it picked (and whose override, if any, it merged). `None` when the track is gone,
/// no longer missing, or `new_path` already has a row.
pub async fn repoint_exact_in(
    conn: &mut PgConnection,
    track: Uuid,
    new_path: &str,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!(
        r#"UPDATE tracks SET path = $2, missing_since = NULL, updated_at = now()
           WHERE id = $1 AND missing_since IS NOT NULL
             AND NOT EXISTS (SELECT 1 FROM tracks WHERE path = $2)
           RETURNING id"#,
        track,
        new_path
    )
    .fetch_optional(&mut *conn)
    .await
}

/// The album of the track at `path`, if there is one, locking that track row for the rest of
/// the transaction. Taken before the artist and album upserts, so a re-read locks track then
/// album, in the same order as the recording stage's answer (which locks the track, then
/// fills the album): the two queue up instead of deadlocking.
pub async fn album_of_path(conn: &mut PgConnection, path: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!(
        "SELECT album_id FROM tracks WHERE path = $1 FOR NO KEY UPDATE",
        path
    )
    .fetch_optional(&mut *conn)
    .await
}

/// A mass disappearance (a dropped mount, not a deletion) stands the sweep down when at least
/// this many tracks, and more than [`MASS_MISSING_ABORT`] of the library, are flagged missing.
pub const MASS_MISSING_FLOOR: i64 = 10;
/// See [`MASS_MISSING_FLOOR`] (upstream `libraryReconcile.ts`: 10 % and 10).
pub const MASS_MISSING_ABORT: f64 = 0.10;

/// What [`retire_missing`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Retired {
    /// Tracks deleted.
    pub tracks: usize,
    /// The album of each deleted track (one id per track), to refresh what derives from them.
    pub albums: Vec<Uuid>,
    /// `(owner, playlist)` of every playlist that held a retired track, for live updates.
    pub playlists: Vec<(Uuid, Uuid)>,
    /// Transcode-cache files of the retired tracks; the caller unlinks them after the commit.
    pub artifacts: Vec<std::path::PathBuf>,
    /// The guard fired: more than 10 % (and at least 10) of all tracks are flagged missing.
    pub stood_down: bool,
    /// How many tracks had their history handed to another copy.
    pub handed_over: usize,
}

/// Deletes tracks missing for longer than `older_than` (D-040, D-161), in one transaction:
/// stands down on a mass disappearance; hands plays, stars, ratings, playlist places and
/// bookmarks to a live copy of the same song (same album, disc, track number, title ignoring
/// case) unless the user already has that on the copy; writes `removal_log`; clears
/// `playback_state` references; deletes. `cache_dir` is where transcodes are cached as
/// `<track id>-<quality>-<size>-<mtime>.mp3`; those files are listed, not unlinked.
pub async fn retire_missing(
    pool: &PgPool,
    older_than: Duration,
    cache_dir: Option<&std::path::Path>,
) -> sqlx::Result<Retired> {
    let mut tx = pool.begin().await?;
    sqlx::query!("SET LOCAL statement_timeout = '120s'")
        .execute(&mut *tx)
        .await?;
    let counts = sqlx::query!(
        r#"SELECT count(*) AS "total!", count(missing_since) AS "flagged!" FROM tracks"#
    )
    .fetch_one(&mut *tx)
    .await?;
    if counts.flagged >= MASS_MISSING_FLOOR
        && counts.flagged as f64 / counts.total as f64 > MASS_MISSING_ABORT
    {
        return Ok(Retired {
            stood_down: true,
            ..Default::default()
        });
    }
    let gone = sqlx::query!(
        r#"SELECT t.id, t.album_id, t.path, t.title, al.title AS album_title, ar.name AS artist_name
           FROM tracks t JOIN albums al ON al.id = t.album_id JOIN artists ar ON ar.id = al.artist_id
           WHERE t.missing_since IS NOT NULL
             AND t.missing_since < now() - make_interval(secs => $1::float8)
           ORDER BY t.id
           FOR UPDATE OF t"#,
        older_than.as_secs_f64()
    )
    .fetch_all(&mut *tx)
    .await?;
    if gone.is_empty() {
        return Ok(Retired::default());
    }
    let ids: Vec<Uuid> = gone.iter().map(|g| g.id).collect();

    let playlists: Vec<(Uuid, Uuid)> = sqlx::query!(
        "SELECT DISTINCT p.user_id, p.id FROM playlist_items i JOIN playlists p ON p.id = i.playlist_id
         WHERE i.track_id = ANY($1) ORDER BY 1, 2",
        &ids
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.id))
    .collect();

    // Titles are compared here, not in SQL: the database lower-cases ASCII only.
    let slots = sqlx::query!(
        r#"SELECT g.id AS gone, k.id AS kept, g.title AS gone_title, k.title AS kept_title
           FROM tracks g JOIN tracks k
             ON k.album_id = g.album_id AND k.track_no = g.track_no AND k.disc = g.disc
            AND k.missing_since IS NULL AND k.id <> g.id
           WHERE g.id = ANY($1) ORDER BY g.id, k.id"#,
        &ids
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut pairs: Vec<(Uuid, Uuid)> = Vec::new();
    for s in slots {
        if s.gone_title.to_lowercase() != s.kept_title.to_lowercase() {
            continue;
        }
        if pairs.last().map(|p| p.0) != Some(s.gone) {
            pairs.push((s.gone, s.kept));
        }
    }
    for &(from, to) in &pairs {
        hand_over(&mut tx, from, to).await?;
    }
    let copy_of: std::collections::HashMap<Uuid, Uuid> = pairs.iter().copied().collect();

    let mut artifacts = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if let Some(dir) = cache_dir
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        files = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
    }
    for g in &gone {
        let prefix = format!("{}-", g.id);
        let mine: Vec<std::path::PathBuf> = files
            .iter()
            .filter(|f| f.starts_with(&prefix))
            .filter_map(|f| cache_dir.map(|d| d.join(f)))
            .collect();
        let listing = Json(
            mine.iter()
                .map(|p| serde_json::json!({"table": "transcodes", "path": p.to_string_lossy()}))
                .collect::<Vec<_>>(),
        );
        let reason = if copy_of.contains_key(&g.id) {
            "replaced-by-another-file"
        } else {
            "file-missing-grace"
        };
        sqlx::query!(
            "INSERT INTO removal_log (id, file_path, title, artist_name, album_title, reason, artifacts)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            Uuid::now_v7(),
            g.path,
            g.title,
            g.artist_name,
            g.album_title,
            reason,
            listing as Json<Vec<serde_json::Value>>
        )
        .execute(&mut *tx)
        .await?;
        artifacts.extend(mine);
    }

    // `playback_state.track_id` is text without a foreign key (D-061): clear it like upstream.
    let text_ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    sqlx::query!(
        "UPDATE playback_state SET track_id = NULL WHERE track_id = ANY($1)",
        &text_ids
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM tracks WHERE id = ANY($1)", &ids)
        .execute(&mut *tx)
        .await?;
    // A playlist that lost a place closes the gap, keeping `sort` dense (0..n-1).
    let lists: Vec<Uuid> = playlists.iter().map(|p| p.1).collect();
    sqlx::query!(
        "UPDATE playlist_items i SET sort = n.rn
         FROM (SELECT id, (row_number() OVER (PARTITION BY playlist_id ORDER BY sort) - 1)::int AS rn
               FROM playlist_items WHERE playlist_id = ANY($1)) n
         WHERE i.id = n.id AND i.sort <> n.rn",
        &lists
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Retired {
        tracks: gone.len(),
        albums: gone.iter().map(|g| g.album_id).collect(),
        playlists,
        artifacts,
        stood_down: false,
        handed_over: pairs.len(),
    })
}

/// Moves `from`'s history onto `to`, each kind unless the same user already has it on `to`
/// (that row stays; the duplicate goes with the retiring track).
async fn hand_over(conn: &mut PgConnection, from: Uuid, to: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE plays SET track_id = $2 WHERE track_id = $1",
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE stars s SET track_id = $2 WHERE s.track_id = $1
           AND NOT EXISTS (SELECT 1 FROM stars o WHERE o.user_id = s.user_id AND o.track_id = $2)",
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE ratings r SET track_id = $2 WHERE r.track_id = $1
           AND NOT EXISTS (SELECT 1 FROM ratings o WHERE o.user_id = r.user_id AND o.track_id = $2)",
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE playlist_items i SET track_id = $2 WHERE i.track_id = $1
           AND NOT EXISTS (SELECT 1 FROM playlist_items o WHERE o.playlist_id = i.playlist_id AND o.track_id = $2)",
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE subsonic_bookmarks b SET track_id = $2 WHERE b.track_id = $1
           AND NOT EXISTS (SELECT 1 FROM subsonic_bookmarks o WHERE o.user_id = b.user_id AND o.track_id = $2)",
        from,
        to
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The albums of the tracks at these paths.
pub async fn albums_of_paths(pool: &PgPool, paths: &[String]) -> sqlx::Result<Vec<Uuid>> {
    let mut conn = pool.acquire().await?;
    albums_of_paths_in(&mut conn, paths).await
}

/// [`albums_of_paths`] on a connection, so it can share a transaction.
pub async fn albums_of_paths_in(
    conn: &mut PgConnection,
    paths: &[String],
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar!(
        "SELECT DISTINCT album_id FROM tracks WHERE path = ANY($1)",
        paths
    )
    .fetch_all(conn)
    .await
}

/// What [`prune_empty`] deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    pub albums: Vec<Uuid>,
    pub artists: Vec<Uuid>,
}

/// Deletes albums with no tracks, then artists with no albums, in one transaction.
/// Must not run concurrently with upserts (the FKs are RESTRICT, so a race fails loudly
/// instead of cascading); the `library` queue serialises it. Returns the deleted ids, so the
/// caller can delete their files (cover cache, artist images) once the rows are gone.
pub async fn prune_empty(pool: &PgPool) -> sqlx::Result<Pruned> {
    let mut tx = pool.begin().await?;
    let albums = sqlx::query_scalar!(
        "DELETE FROM albums a WHERE NOT EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = a.id)
         RETURNING id"
    )
    .fetch_all(&mut *tx)
    .await?;
    let artists = sqlx::query_scalar!(
        "DELETE FROM artists a WHERE NOT EXISTS (SELECT 1 FROM albums b WHERE b.artist_id = a.id)
         RETURNING id"
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Pruned { albums, artists })
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
    /// `guessed.viaSymlink`: the row stands for a file reached through a symlink.
    pub via_symlink: bool,
    /// The track's album has `identity_source = 'autotag'` and its `applied_identity` names the
    /// row's own artist key, title key and year (spec §19.7): a track agreeing with it is pinned.
    pub album_autotag: bool,
    /// The name of the artist the track's album is filed under.
    pub artist_name: String,
    /// `guessed.credit`: the file's vote in the album-artist election (D-164), if it has one.
    pub credit: Option<Json<StoredCredit>>,
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
                  COALESCE((t.guessed ->> 'yearConflict')::boolean, false) AS "year_conflict!",
                  COALESCE((t.guessed ->> 'viaSymlink')::boolean, false) AS "via_symlink!",
                  COALESCE(b.identity_source = 'autotag'
                           AND b.applied_identity ->> 'artistKey' = a.name_key
                           AND b.applied_identity ->> 'titleKey' = b.title_key
                           AND COALESCE(b.applied_identity -> 'year', 'null'::jsonb)
                               = COALESCE(to_jsonb(b.year), 'null'::jsonb),
                           false) AS "album_autotag!",
                  a.name AS artist_name,
                  t.guessed -> 'credit' AS "credit: Json<StoredCredit>"
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

/// The tracks of one album folder (D-164): those directly inside it (`folder_prefix` is
/// `"Artist/Album/"`, or `""` for the library root) and those inside a disc subfolder of it
/// (`CD 1`, `Disc 2`, ...). The match on the subfolder's name is a loose pre-filter; the caller
/// keeps the rows whose album folder is exactly this one.
pub async fn scan_index_album_folder(
    pool: &PgPool,
    folder_prefix: &str,
) -> sqlx::Result<Vec<IndexedTrack>> {
    sqlx::query_as!(
        IndexedTrack,
        r#"SELECT t.id, t.path, t.size, t.mtime,
                  t.missing_since IS NOT NULL AS "missing!",
                  t.year AS track_year, b.year AS album_year,
                  b.title_key AS album_key, a.name_key AS artist_key,
                  COALESCE((t.guessed ->> 'yearConflict')::boolean, false) AS "year_conflict!",
                  COALESCE((t.guessed ->> 'viaSymlink')::boolean, false) AS "via_symlink!",
                  COALESCE(b.identity_source = 'autotag'
                           AND b.applied_identity ->> 'artistKey' = a.name_key
                           AND b.applied_identity ->> 'titleKey' = b.title_key
                           AND COALESCE(b.applied_identity -> 'year', 'null'::jsonb)
                               = COALESCE(to_jsonb(b.year), 'null'::jsonb),
                           false) AS "album_autotag!",
                  a.name AS artist_name,
                  t.guessed -> 'credit' AS "credit: Json<StoredCredit>"
           FROM tracks t
           JOIN albums b ON b.id = t.album_id
           JOIN artists a ON a.id = b.artist_id
           WHERE starts_with(t.path, $1)
             AND (strpos(substr(t.path, length($1) + 1), '/') = 0
                  OR substr(t.path, length($1) + 1) ~* '^(cd|dis[ck]|digital media|vinyl|side)[^/]*/[^/]+$')"#,
        folder_prefix
    )
    .fetch_all(pool)
    .await
}

/// Sets (`on`) or clears `guessed.viaSymlink` on the tracks at these paths; rows already in
/// that state are left alone. Returns the rows changed.
pub async fn set_via_symlink(pool: &PgPool, paths: &[String], on: bool) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "UPDATE tracks SET guessed = jsonb_set(guessed, '{viaSymlink}', to_jsonb($2::boolean))
         WHERE path = ANY($1)
           AND COALESCE((guessed ->> 'viaSymlink')::boolean, false) <> $2",
        paths,
        on
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Paths of the present tracks whose file had this size and modification time when read.
pub async fn present_paths_with_stat(
    pool: &PgPool,
    size: i64,
    mtime: OffsetDateTime,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT path FROM tracks WHERE size = $1 AND mtime = $2 AND missing_since IS NULL",
        size,
        mtime
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

/// Clears the missing flag of tracks whose file is back at the same path, unchanged, and
/// reopens their `FILE_MISSING` stage rows. Returns how many tracks changed.
pub async fn clear_missing(pool: &PgPool, paths: &[String]) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let n = clear_missing_in(&mut tx, paths).await?;
    tx.commit().await?;
    Ok(n)
}

/// [`clear_missing`] on a connection (the caller's transaction commits it).
pub async fn clear_missing_in(conn: &mut PgConnection, paths: &[String]) -> sqlx::Result<u64> {
    let ids = sqlx::query_scalar!(
        "UPDATE tracks SET missing_since = NULL, updated_at = now()
         WHERE path = ANY($1) AND missing_since IS NOT NULL
         RETURNING id",
        paths
    )
    .fetch_all(&mut *conn)
    .await?;
    crate::stages::reopen_file_missing_many(conn, &ids).await?;
    Ok(ids.len() as u64)
}
