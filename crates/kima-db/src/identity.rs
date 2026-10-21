//! Identification data (spec §6, §8.3, §8.5, §9): the AcoustID fingerprint kept apart from `tracks`,
//! the ladder's view of a track and the writes of a recording answer.

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Store (or replace) `track`'s AcoustID fingerprint and the duration sent with it. Runs on
/// the caller's connection so it shares the stage report's transaction.
pub async fn store_fingerprint(
    conn: &mut PgConnection,
    track: Uuid,
    fingerprint: &str,
    duration_secs: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO track_acoustid (track_id, fingerprint, duration_secs)
         VALUES ($1, $2, $3)
         ON CONFLICT (track_id) DO UPDATE SET
           fingerprint = EXCLUDED.fingerprint, duration_secs = EXCLUDED.duration_secs,
           computed_at = now()",
        track,
        fingerprint,
        duration_secs,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `track`'s stored fingerprint and duration, if any.
pub async fn fingerprint_of(pool: &PgPool, track: Uuid) -> sqlx::Result<Option<(String, i32)>> {
    let row = sqlx::query!(
        "SELECT fingerprint, duration_secs FROM track_acoustid WHERE track_id = $1",
        track
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.fingerprint, r.duration_secs)))
}

/// `track`'s path (relative to the library root) and `duration_secs`.
pub async fn track_path_and_duration(
    pool: &PgPool,
    track: Uuid,
) -> sqlx::Result<Option<(String, i32)>> {
    let row = sqlx::query!(
        "SELECT path, duration_secs FROM tracks WHERE id = $1",
        track
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.path, r.duration_secs)))
}

/// What the recording stage's ladder reads about a track (spec §8.3): the track's own fields, its
/// album and the album artist's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LadderTrack {
    pub id: Uuid,
    pub title: String,
    pub artist_name: String,
    pub duration_secs: i32,
    pub isrc: Option<String>,
    /// `id3` (from the file) or `musicbrainz`.
    pub isrc_source: Option<String>,
    pub tag_recording_mbid: Option<Uuid>,
    pub album_id: Uuid,
    pub album_title: String,
    pub album_artist: String,
    pub release_mbid: Option<Uuid>,
    pub release_group_mbid: Option<Uuid>,
    pub album_label: Option<String>,
    /// The album already has `source='musicbrainz'` genres.
    pub album_has_mb_genres: bool,
}

/// The ladder's view of `id`; `None` when the track is gone.
pub async fn ladder_track(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<LadderTrack>> {
    let row = sqlx::query!(
        r#"SELECT t.id, t.title, t.artist_name, t.duration_secs, t.isrc, t.isrc_source,
                  t.tag_recording_mbid, a.id AS album_id, a.title AS album_title,
                  ar.name AS album_artist, a.release_mbid, a.release_group_mbid,
                  a.label AS album_label,
                  EXISTS (SELECT 1 FROM album_genres g
                          WHERE g.album_id = a.id AND g.source = 'musicbrainz')
                    AS "album_has_mb_genres!"
           FROM tracks t
           JOIN albums a ON a.id = t.album_id
           JOIN artists ar ON ar.id = a.artist_id
           WHERE t.id = $1"#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| LadderTrack {
        id: r.id,
        title: r.title,
        artist_name: r.artist_name,
        duration_secs: r.duration_secs,
        isrc: r.isrc,
        isrc_source: r.isrc_source,
        tag_recording_mbid: r.tag_recording_mbid,
        album_id: r.album_id,
        album_title: r.album_title,
        album_artist: r.album_artist,
        release_mbid: r.release_mbid,
        release_group_mbid: r.release_group_mbid,
        album_label: r.album_label,
        album_has_mb_genres: r.album_has_mb_genres,
    }))
}

/// What a recording answer does to the track's ISRC (spec §8.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsrcWrite {
    Keep,
    /// A normalised ISRC from MusicBrainz, stored with `isrc_source='musicbrainz'`.
    Set(String),
    /// The recording has none: an earlier MusicBrainz ISRC is cleared.
    Clear,
}

/// `^[A-Z]{2}[A-Z0-9]{3}[0-9]{7}$`, the `tracks.isrc` check.
fn is_isrc(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[5..].iter().all(u8::is_ascii_digit)
}

/// Locks `track`'s row for the answer's transaction and returns its album; `None` when the track
/// is gone. Taken first, so a concurrent purge waits for the answer (or the answer sees the
/// purge) instead of deadlocking with it.
pub async fn lock_track(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!(
        "SELECT album_id FROM tracks WHERE id = $1 FOR NO KEY UPDATE",
        track
    )
    .fetch_optional(&mut *conn)
    .await
}

/// Stores the identification answer: `recording_mbid`, `recording_source`, and the ISRC per
/// `isrc`. Whatever the caller asks, an ISRC whose source is neither NULL nor `musicbrainz` (the
/// file's) is never replaced or cleared, and a value that is not an ISRC is stored as a clear.
pub async fn store_recording_answer(
    conn: &mut PgConnection,
    track: Uuid,
    recording: Uuid,
    source: &str,
    isrc: IsrcWrite,
) -> sqlx::Result<()> {
    let (mode, code) = match isrc {
        IsrcWrite::Keep => ("keep", None),
        IsrcWrite::Set(code) if is_isrc(&code) => ("set", Some(code)),
        IsrcWrite::Set(_) | IsrcWrite::Clear => ("clear", None),
    };
    sqlx::query!(
        "UPDATE tracks
         SET recording_mbid = $2, recording_source = $3,
             isrc = CASE WHEN $4 = 'keep' OR isrc_source IS DISTINCT FROM 'musicbrainz'
                                              AND isrc_source IS NOT NULL THEN isrc
                         ELSE $5 END,
             isrc_source = CASE WHEN $4 = 'keep' OR isrc_source IS DISTINCT FROM 'musicbrainz'
                                                     AND isrc_source IS NOT NULL THEN isrc_source
                                WHEN $5::text IS NULL THEN NULL
                                ELSE 'musicbrainz' END
         WHERE id = $1",
        track,
        recording,
        source,
        mode,
        code,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Fills the album's label when it is blank (NULL or only whitespace); a present label is kept.
/// Always locks the album row for the rest of the transaction, so concurrent answers for tracks
/// of one album fill it one after the other.
pub async fn fill_album_blank(
    conn: &mut PgConnection,
    album: Uuid,
    label: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE albums
         SET label = CASE WHEN (label IS NULL OR btrim(label) = '') AND $2::text IS NOT NULL
                          THEN $2 ELSE label END
         WHERE id = $1",
        album,
        label,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Whether the album has `source='musicbrainz'` genres, read on the caller's connection.
pub async fn album_has_mb_genres(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM album_genres
                          WHERE album_id = $1 AND source = 'musicbrainz') AS "has!""#,
        album
    )
    .fetch_one(&mut *conn)
    .await
}
