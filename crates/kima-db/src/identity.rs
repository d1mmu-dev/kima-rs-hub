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

/// Locks `album`'s row for the answer's transaction, the scanner's strength (`NO KEY UPDATE`).
/// Taken after the track and before any genre is written, so the answer and a re-read go
/// track, album, genres in the same order and queue up instead of deadlocking.
pub async fn lock_album(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<()> {
    sqlx::query_scalar!(
        "SELECT id FROM albums WHERE id = $1 FOR NO KEY UPDATE",
        album
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(())
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

// ---- artist enrichment (spec §10.5, §12) ----

/// Artist enrichment claims every artist with a present track except this one (spec §6.3).
const VARIOUS_ARTISTS_KEY: &str = "various artists";
/// The most album titles and album folders a claim carries per artist.
pub const MAX_CLAIM_ALBUMS: i64 = 100;

/// An artist claimed for enrichment (`enrichment_status = 'enriching'`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistForEnrichment {
    pub id: Uuid,
    pub name: String,
    pub mbid: Option<Uuid>,
    /// `tag`, `search`, `user` or `autotag`.
    pub mbid_source: Option<String>,
    /// `local`, `wikidata`, `fanart`, `deezer` or `user`.
    pub image_source: Option<String>,
    pub enrichment_failures: i16,
    /// Titles of the artist's albums that have a present track, at most [`MAX_CLAIM_ALBUMS`].
    pub album_titles: Vec<String>,
    /// The folders (relative to the library root, `""` for the root) holding the artist's
    /// present tracks, at most [`MAX_CLAIM_ALBUMS`].
    pub album_dirs: Vec<String>,
}

/// How long a `failed` artist waits before it is claimed again (upstream
/// `FAILED_JOB_RETRY_GRACE_MS`, 15 min: a failed job holds its id until then). The claim
/// writes the same 15 minutes as an SQL interval, on the database clock.
pub const FAILED_RETRY_GRACE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Claims up to `n` artists for enrichment (spec §10.5): `pending`, `failed` and last tried
/// over [`FAILED_RETRY_GRACE`] ago, or `unresolvable` and last tried over 7 days ago, with a
/// present track, Various Artists aside; oldest `last_enriched_at` first (never tried first),
/// then id. Each is set `enriching`; a row another claimer holds is skipped.
pub async fn claim_artists(pool: &PgPool, n: i64) -> sqlx::Result<Vec<ArtistForEnrichment>> {
    let rows = sqlx::query!(
        r#"WITH due AS (
               SELECT a.id, a.last_enriched_at FROM artists a
               WHERE a.name_key <> $2
                 AND (a.enrichment_status = 'pending'
                      OR (a.enrichment_status = 'failed'
                          AND (a.last_enriched_at IS NULL
                               OR a.last_enriched_at < now() - interval '15 minutes'))
                      OR (a.enrichment_status = 'unresolvable'
                          AND (a.last_enriched_at IS NULL
                               OR a.last_enriched_at < now() - interval '7 days')))
                 AND EXISTS (SELECT 1 FROM albums al JOIN tracks t ON t.album_id = al.id
                             WHERE al.artist_id = a.id AND t.missing_since IS NULL)
               ORDER BY a.last_enriched_at NULLS FIRST, a.id
               LIMIT $1
               FOR NO KEY UPDATE OF a SKIP LOCKED)
           UPDATE artists a SET enrichment_status = 'enriching'
           FROM due
           WHERE a.id = due.id AND a.enrichment_status <> 'enriching'
           RETURNING a.id, a.name, a.mbid, a.mbid_source, a.image_source,
                     a.enrichment_failures, due.last_enriched_at,
                     ARRAY(SELECT al.title FROM albums al
                           WHERE al.artist_id = a.id
                             AND EXISTS (SELECT 1 FROM tracks t
                                         WHERE t.album_id = al.id AND t.missing_since IS NULL)
                           ORDER BY al.title, al.id LIMIT $3) AS "album_titles!",
                     ARRAY(SELECT DISTINCT CASE WHEN strpos(t.path, '/') > 0
                                                THEN regexp_replace(t.path, '/[^/]*$', '')
                                                ELSE '' END AS dir
                           FROM albums al JOIN tracks t ON t.album_id = al.id
                           WHERE al.artist_id = a.id AND t.missing_since IS NULL
                           ORDER BY dir LIMIT $3) AS "album_dirs!""#,
        n,
        VARIOUS_ARTISTS_KEY,
        MAX_CLAIM_ALBUMS,
    )
    .fetch_all(pool)
    .await?;
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_by(|a, b| {
        a.last_enriched_at
            .is_some()
            .cmp(&b.last_enriched_at.is_some())
            .then(a.last_enriched_at.cmp(&b.last_enriched_at))
            .then(a.id.cmp(&b.id))
    });
    Ok(rows
        .into_iter()
        .map(|r| ArtistForEnrichment {
            id: r.id,
            name: r.name,
            mbid: r.mbid,
            mbid_source: r.mbid_source,
            image_source: r.image_source,
            enrichment_failures: r.enrichment_failures,
            album_titles: r.album_titles,
            album_dirs: r.album_dirs,
        })
        .collect())
}

/// Gives claimed artists back (`enriching` → `pending`): claims a stopped or cancelled batch did
/// not finish.
pub async fn release_artists(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let r = sqlx::query!(
        "UPDATE artists SET enrichment_status = 'pending'
         WHERE id = ANY($1) AND enrichment_status = 'enriching'",
        ids
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// An artist's enrichment status after a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistStatus {
    Pending,
    Completed,
    Failed,
    Unresolvable,
}

impl ArtistStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtistStatus::Pending => "pending",
            ArtistStatus::Completed => "completed",
            ArtistStatus::Failed => "failed",
            ArtistStatus::Unresolvable => "unresolvable",
        }
    }
}

/// What a run does to one stored value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Change<T> {
    /// Leave it as stored (a failed source never records "none").
    #[default]
    Keep,
    Set(T),
    Clear,
}

impl<T> Change<T> {
    fn mode(&self) -> &'static str {
        match self {
            Change::Keep => "keep",
            Change::Set(_) => "set",
            Change::Clear => "clear",
        }
    }

    fn value(&self) -> Option<&T> {
        match self {
            Change::Set(v) => Some(v),
            _ => None,
        }
    }
}

/// The outcome of one artist's enrichment, written by [`save_artist_outcome`].
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistWrite {
    pub status: ArtistStatus,
    pub failures: i16,
    /// `(code, message)` for `last_error_code` and `last_error`.
    pub error: Change<(String, String)>,
    /// `(mbid, mbid_source)`. Never applied over a `user` or `autotag` MBID.
    pub mbid: Change<(Uuid, String)>,
    pub bio: Change<String>,
    /// `(image_path, image_source)`. Never applied over a `user` image.
    pub image: Change<(String, String)>,
    /// `artists."similar"`: `[{name, mbid|null, match}]`.
    pub similar: Change<serde_json::Value>,
    /// The artist's `musicbrainz` genres: `Set` replaces them (top first), `Clear` deletes them.
    pub mb_genres: Change<Vec<String>>,
}

impl ArtistWrite {
    /// A write that changes nothing but the status and the failure count.
    pub fn status(status: ArtistStatus, failures: i16) -> ArtistWrite {
        ArtistWrite {
            status,
            failures,
            error: Change::Keep,
            mbid: Change::Keep,
            bio: Change::Keep,
            image: Change::Keep,
            similar: Change::Keep,
            mb_genres: Change::Keep,
        }
    }
}

/// Writes a run's outcome in one transaction: the artist row first (locked by the update),
/// then its genres, the order every writer of artists and genres keeps. Applied only while the
/// artist is still `enriching` (a wipe or a restart in between wins); returns whether it was.
/// `last_enriched_at` is now.
pub async fn save_artist_outcome(pool: &PgPool, id: Uuid, out: &ArtistWrite) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let saved = save_artist_outcome_in(&mut tx, id, out).await?;
    if saved.is_some() {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    Ok(saved.is_some())
}

/// The row's image after [`save_artist_outcome_in`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedArtist {
    /// `image_source` as written: the run's own source when its image was accepted, `user`
    /// when a user image kept its place, `None` when there is no image.
    pub image_source: Option<String>,
}

/// [`save_artist_outcome`] on the caller's transaction, which it leaves open: the artist row
/// stays locked until the caller commits, so a stored image file can be put in place between
/// the precedence check and the commit. `None` (nothing written) when the artist is no longer
/// `enriching`.
pub async fn save_artist_outcome_in(
    tx: &mut PgConnection,
    id: Uuid,
    out: &ArtistWrite,
) -> sqlx::Result<Option<SavedArtist>> {
    let error = out.error.value();
    let mbid = out.mbid.value();
    let image = out.image.value();
    let updated = sqlx::query_scalar!(
        r#"UPDATE artists SET
               enrichment_status = $2, enrichment_failures = $3, last_enriched_at = now(),
               last_error = CASE $4 WHEN 'keep' THEN last_error WHEN 'set' THEN $5 ELSE NULL END,
               last_error_code = CASE $4 WHEN 'keep' THEN last_error_code WHEN 'set' THEN $6
                                         ELSE NULL END,
               mbid = CASE WHEN $7 = 'keep' OR mbid_source IN ('user', 'autotag') THEN mbid
                           WHEN $7 = 'set' THEN $8 ELSE NULL END,
               mbid_source = CASE WHEN $7 = 'keep' OR mbid_source IN ('user', 'autotag')
                                  THEN mbid_source
                                  WHEN $7 = 'set' THEN $9 ELSE NULL END,
               bio = CASE $10 WHEN 'keep' THEN bio WHEN 'set' THEN $11 ELSE NULL END,
               image_path = CASE WHEN $12 = 'keep' OR image_source = 'user' THEN image_path
                                 WHEN $12 = 'set' THEN $13 ELSE NULL END,
               image_source = CASE WHEN $12 = 'keep' OR image_source = 'user' THEN image_source
                                   WHEN $12 = 'set' THEN $14 ELSE NULL END,
               "similar" = CASE $15 WHEN 'keep' THEN "similar" WHEN 'set' THEN $16
                                    ELSE NULL END
           WHERE id = $1 AND enrichment_status = 'enriching'
           RETURNING image_source"#,
        id,
        out.status.as_str(),
        out.failures,
        out.error.mode(),
        error.map(|e| e.1.as_str()),
        error.map(|e| e.0.as_str()),
        out.mbid.mode(),
        mbid.map(|m| m.0),
        mbid.map(|m| m.1.as_str()),
        out.bio.mode(),
        out.bio.value().map(String::as_str),
        out.image.mode(),
        image.map(|i| i.0.as_str()),
        image.map(|i| i.1.as_str()),
        out.similar.mode(),
        out.similar.value(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(image_source) = updated else {
        return Ok(None);
    };
    let source = crate::genres::GenreSource::MusicBrainz;
    match &out.mb_genres {
        Change::Keep => {}
        Change::Set(names) => {
            crate::genres::replace_artist_genres(&mut *tx, id, source, names).await?;
        }
        Change::Clear => {
            crate::genres::replace_artist_genres(&mut *tx, id, source, &[]).await?;
        }
    }
    Ok(Some(SavedArtist { image_source }))
}

// ---- album cover fallback (spec §12.7) ----

/// An album the cover fallback may fill: no stored image and a present track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumForCover {
    pub id: Uuid,
    pub title: String,
    /// The album artist's name.
    pub artist_name: String,
    pub release_group_mbid: Option<Uuid>,
    /// The album's first present track (disc, number, path), relative to the library root:
    /// the one file probed for embedded or folder art.
    pub track_path: Option<String>,
}

/// `artist`'s albums with no stored image (`image_path IS NULL`) and a present track, by title
/// then id, at most [`MAX_CLAIM_ALBUMS`]. Whether one has embedded or folder art is for the
/// caller to check (on [`AlbumForCover::track_path`]).
pub async fn albums_without_cover(pool: &PgPool, artist: Uuid) -> sqlx::Result<Vec<AlbumForCover>> {
    let rows = sqlx::query!(
        r#"SELECT al.id, al.title, ar.name AS artist_name, al.release_group_mbid,
                  (SELECT t.path FROM tracks t
                   WHERE t.album_id = al.id AND t.missing_since IS NULL
                   ORDER BY t.disc, t.track_no, t.path LIMIT 1) AS track_path
           FROM albums al JOIN artists ar ON ar.id = al.artist_id
           WHERE al.artist_id = $1 AND al.image_path IS NULL
             AND EXISTS (SELECT 1 FROM tracks t
                         WHERE t.album_id = al.id AND t.missing_since IS NULL)
           ORDER BY al.title, al.id
           LIMIT $2"#,
        artist,
        MAX_CLAIM_ALBUMS,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AlbumForCover {
            id: r.id,
            title: r.title,
            artist_name: r.artist_name,
            release_group_mbid: r.release_group_mbid,
            track_path: r.track_path,
        })
        .collect())
}

/// Records a fallback cover (`path` relative to `<data_dir>/album-images`, `source` one of
/// `deezer`, `coverartarchive`, `fanart`) on the caller's connection. Never over a `user`
/// image. The update locks the album row (album only, as every writer of albums does after
/// tracks), so a caller in a transaction holds it until it commits: the file is put in place
/// after this answers `true` and before the commit. Returns whether it was applied (`false`:
/// a user image, or the album is gone).
pub async fn set_album_image(
    conn: &mut PgConnection,
    album: Uuid,
    path: &str,
    source: &str,
) -> sqlx::Result<bool> {
    let updated = sqlx::query_scalar!(
        "UPDATE albums SET image_path = $2, image_source = $3
         WHERE id = $1 AND image_source IS DISTINCT FROM 'user'
         RETURNING id",
        album,
        path,
        source,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(updated.is_some())
}

/// What `GET /library/cover-art/<id>` may serve for an id (D-113).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverOwner {
    /// No album and no artist has this id.
    None,
    /// An album; `image_source` of its stored image, when it has one.
    Album { image_source: Option<String> },
    /// An artist; whether it has a stored image.
    Artist { has_image: bool },
}

/// Whose id `id` is, for the cover route: an album first, else an artist.
pub async fn cover_owner(pool: &PgPool, id: Uuid) -> sqlx::Result<CoverOwner> {
    let album = sqlx::query!(
        r#"SELECT CASE WHEN image_path IS NOT NULL THEN image_source END AS image_source
           FROM albums WHERE id = $1"#,
        id
    )
    .fetch_optional(pool)
    .await?;
    if let Some(a) = album {
        return Ok(CoverOwner::Album {
            image_source: a.image_source,
        });
    }
    let artist = sqlx::query_scalar!(
        r#"SELECT image_path IS NOT NULL AS "has!" FROM artists WHERE id = $1"#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(match artist {
        Some(has_image) => CoverOwner::Artist { has_image },
        None => CoverOwner::None,
    })
}
