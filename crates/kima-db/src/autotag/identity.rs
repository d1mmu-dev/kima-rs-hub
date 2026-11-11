//! Album and artist identity on apply and undo (spec §19.6, §19.7, R1/R2): the album row is
//! updated in place and keeps its id; the album artist is re-parented, renamed in place or
//! created; a collision with another album's identity is refused, never merged.
//!
//! **Lock order.** [`apply_identity`] and [`undo_identity`] lock, in this order:
//! 1. the run row (`FOR UPDATE`; nothing else locks it, it only makes a repeated call see the
//!    first one's `album_before`);
//! 2. the album's tracks, missing ones included, plus the tracks the run names, in id order
//!    (`FOR NO KEY UPDATE`);
//! 3. the album row (`FOR UPDATE`: its identity key changes);
//! 4. the artists involved (the album's artist, the row holding the target name, the run's old
//!    artist), in id order (`FOR UPDATE`: a rename changes `name_key`).
//!
//! This is the order of the scanner and the identity paths (track, then album, then artists:
//! `library::album_of_path` then `autotag_identity`, `identity::lock_track` then `lock_album`),
//! so a scan holding a track row makes the apply wait instead of deadlocking. The scanner's
//! normal (unpinned) upsert takes the artist before the album; a new file whose key lands on
//! the very album being applied can therefore still deadlock with it, and Postgres then aborts
//! one side. Every function here runs entirely on the caller's transaction and keeps no state
//! across calls, so the caller wraps the transaction in `retry_once_on_conflict`.
//!
//! The collision is checked again under the locks: once the target artist row is locked, no
//! scan can add an album to it (the scanner's artist upsert waits for the lock).
//!
//! Callers prune artists left with no album with `library::prune_empty` after the commit.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Applied, RunRow};
use crate::genres::{self, GenreSource};
use crate::library::{AppliedIdentity, VARIOUS_ARTISTS_MBID};

/// `names::key("Various Artists")`: never renamed into, always its own row with its MBID.
const VARIOUS_ARTISTS_KEY: &str = "various artists";

/// The identity keys of the applied album artist and title (`names::key`, computed by the
/// caller, since `kima-db` has no name rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityKeys {
    pub artist_key: String,
    pub title_key: String,
}

/// Another album that already holds an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictAlbum {
    pub album_id: Uuid,
    pub artist: String,
    pub title: String,
    pub year: Option<i32>,
}

impl ConflictAlbum {
    /// `Another album already has this identity: <artist> – <title> (<year>)` (spec §19.7).
    pub fn message(&self) -> String {
        let year = self.year.map(|y| format!(" ({y})")).unwrap_or_default();
        format!(
            "Another album already has this identity: {} \u{2013} {}{year}",
            self.artist, self.title
        )
    }
}

#[derive(Debug)]
pub enum IdentityError {
    Collision(ConflictAlbum),
    /// The album row (and so the run) is gone.
    AlbumGone,
    Db(sqlx::Error),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collision(c) => f.write_str(&c.message()),
            Self::AlbumGone => f.write_str("The album no longer exists"),
            Self::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IdentityError {}

impl From<sqlx::Error> for IdentityError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// What an apply did to the album artist (`autotag_runs.artist_action` and friends).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistStep {
    /// `none`, `reparented`, `renamed` or `created`.
    pub action: &'static str,
    pub old_artist_id: Option<Uuid>,
    pub new_artist_id: Option<Uuid>,
    /// The name before a rename in place.
    pub old_artist_name: Option<String>,
}

/// `autotag_runs.album_before`: everything [`apply_identity`] may change, as it was.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumBefore {
    title: String,
    title_key: String,
    year: Option<i32>,
    is_compilation: bool,
    release_mbid: Option<Uuid>,
    release_mbid_source: Option<String>,
    release_group_mbid: Option<Uuid>,
    release_group_mbid_source: Option<String>,
    identity_source: String,
    applied_identity: Option<Value>,
    artist_id: Uuid,
    artist_name: String,
    artist_key: String,
    artist_mbid: Option<Uuid>,
    artist_mbid_source: Option<String>,
    mb_genres: Vec<String>,
    tracks: Vec<TrackBefore>,
    /// The one artist row whose MBID the apply changed, with its value before and the value the
    /// apply left on it: undo restores it only while the row still holds that value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artist_mbid_write: Option<ArtistMbidWrite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackBefore {
    track_id: Uuid,
    recording_mbid: Option<Uuid>,
    recording_source: Option<String>,
    /// The `recording` stage row, when the track had one.
    #[serde(default)]
    recording_stage: Option<StageBefore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StageBefore {
    status: String,
    source: Option<String>,
    evidence: Option<Value>,
    /// `to_jsonb(finished_at)`: an ISO 8601 string, or null.
    finished_at: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtistMbidWrite {
    id: Uuid,
    mbid: Option<Uuid>,
    mbid_source: Option<String>,
    /// What the apply wrote: an MBID with source `autotag`, or none (a renamed row losing the
    /// Various Artists MBID).
    written: Option<Uuid>,
}

fn decode(e: serde_json::Error) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(e))
}

/// Another album row whose artist's `name_key` is `keys.artist_key`, with `keys.title_key` and
/// the same `COALESCE(year, -1)`. `year` is the applied year; `None` means the album keeps its
/// own year (the §19.6 "leave alone" rule), which is then compared.
pub async fn find_collision(
    conn: &mut PgConnection,
    album: Uuid,
    keys: &IdentityKeys,
    year: Option<i32>,
) -> sqlx::Result<Option<ConflictAlbum>> {
    let row = sqlx::query!(
        "SELECT al.id, ar.name, al.title, al.year
         FROM albums al JOIN artists ar ON ar.id = al.artist_id
         WHERE ar.name_key = $2 AND al.title_key = $3 AND al.id <> $1
           AND COALESCE(al.year, -1)
               = COALESCE($4, (SELECT b.year FROM albums b WHERE b.id = $1), -1)
         ORDER BY al.id LIMIT 1",
        album,
        keys.artist_key,
        keys.title_key,
        year
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| ConflictAlbum {
        album_id: r.id,
        artist: r.name,
        title: r.title,
        year: r.year,
    }))
}

/// Another album of `artist` with this title key and year.
async fn collision_on_artist(
    conn: &mut PgConnection,
    album: Uuid,
    artist: Uuid,
    title_key: &str,
    year: Option<i32>,
) -> sqlx::Result<Option<ConflictAlbum>> {
    let row = sqlx::query!(
        "SELECT al.id, ar.name, al.title, al.year
         FROM albums al JOIN artists ar ON ar.id = al.artist_id
         WHERE al.artist_id = $2 AND al.title_key = $3 AND al.id <> $1
           AND COALESCE(al.year, -1) = COALESCE($4, -1)
         ORDER BY al.id LIMIT 1",
        album,
        artist,
        title_key,
        year
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| ConflictAlbum {
        album_id: r.id,
        artist: r.name,
        title: r.title,
        year: r.year,
    }))
}

async fn capture(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<AlbumBefore> {
    let a = sqlx::query!(
        "SELECT al.title, al.title_key, al.year, al.is_compilation, al.release_mbid,
                al.release_mbid_source, al.release_group_mbid, al.release_group_mbid_source,
                al.identity_source, al.applied_identity, al.artist_id, ar.name, ar.name_key,
                ar.mbid, ar.mbid_source
         FROM albums al JOIN artists ar ON ar.id = al.artist_id WHERE al.id = $1",
        album
    )
    .fetch_one(&mut *conn)
    .await?;
    let mb_genres = sqlx::query_scalar!(
        "SELECT g.name FROM album_genres ag JOIN genres g ON g.id = ag.genre_id
         WHERE ag.album_id = $1 AND ag.source = 'musicbrainz' ORDER BY ag.rank, g.name",
        album
    )
    .fetch_all(&mut *conn)
    .await?;
    let tracks = sqlx::query!(
        r#"SELECT t.id, t.recording_mbid, t.recording_source,
                  s.status AS "status?", s.source AS "stage_source?", s.evidence AS "evidence?",
                  to_jsonb(s.finished_at) AS "finished_at?"
           FROM tracks t
           LEFT JOIN track_stages s ON s.track_id = t.id AND s.stage = 'recording'
           WHERE t.album_id = $1 ORDER BY t.id"#,
        album
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|t| TrackBefore {
        track_id: t.id,
        recording_mbid: t.recording_mbid,
        recording_source: t.recording_source,
        recording_stage: t.status.map(|status| StageBefore {
            status,
            source: t.stage_source,
            evidence: t.evidence,
            finished_at: t.finished_at.unwrap_or(Value::Null),
        }),
    })
    .collect();
    Ok(AlbumBefore {
        title: a.title,
        title_key: a.title_key,
        year: a.year,
        is_compilation: a.is_compilation,
        release_mbid: a.release_mbid,
        release_mbid_source: a.release_mbid_source,
        release_group_mbid: a.release_group_mbid,
        release_group_mbid_source: a.release_group_mbid_source,
        identity_source: a.identity_source,
        applied_identity: a.applied_identity,
        artist_id: a.artist_id,
        artist_name: a.name,
        artist_key: a.name_key,
        artist_mbid: a.mbid,
        artist_mbid_source: a.mbid_source,
        mb_genres,
        tracks,
        artist_mbid_write: None,
    })
}

/// The album's identity fields, MBIDs and sources, artist, MusicBrainz genres and its tracks'
/// recording answers (with their `recording` stage rows), as `autotag_runs.album_before`
/// stores them. Holds ids, names and MusicBrainz values only, never a secret.
pub async fn capture_before(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<Value> {
    serde_json::to_value(capture(conn, album).await?).map_err(decode)
}

/// Locks the run row; answers its `album_before` and recorded artist step, `None` when the run
/// is gone.
async fn lock_run(conn: &mut PgConnection, run: Uuid) -> sqlx::Result<Option<RunRecorded>> {
    let r = sqlx::query!(
        "SELECT album_before, artist_action, old_artist_id, new_artist_id, old_artist_name
         FROM autotag_runs WHERE id = $1 FOR UPDATE",
        run
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(r.map(|r| RunRecorded {
        album_before: r.album_before,
        artist_action: r.artist_action,
        old_artist_id: r.old_artist_id,
        new_artist_id: r.new_artist_id,
        old_artist_name: r.old_artist_name,
    }))
}

struct RunRecorded {
    album_before: Option<Value>,
    artist_action: Option<String>,
    old_artist_id: Option<Uuid>,
    new_artist_id: Option<Uuid>,
    old_artist_name: Option<String>,
}

fn action_str(s: Option<&str>) -> &'static str {
    match s {
        Some("reparented") => "reparented",
        Some("renamed") => "renamed",
        Some("created") => "created",
        _ => "none",
    }
}

/// Locks the album's tracks (missing ones included) and `extra`, in id order.
async fn lock_tracks(conn: &mut PgConnection, album: Uuid, extra: &[Uuid]) -> sqlx::Result<()> {
    sqlx::query!(
        "SELECT id FROM tracks WHERE album_id = $1 OR id = ANY($2) ORDER BY id FOR NO KEY UPDATE",
        album,
        extra
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(())
}

/// Locks the album row; its artist and year, `None` when it is gone.
async fn lock_album(
    conn: &mut PgConnection,
    album: Uuid,
) -> sqlx::Result<Option<(Uuid, Option<i32>)>> {
    let r = sqlx::query!(
        "SELECT artist_id, year FROM albums WHERE id = $1 FOR UPDATE",
        album
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(r.map(|r| (r.artist_id, r.year)))
}

/// Locks these artist rows (those that exist) in id order.
async fn lock_artists(conn: &mut PgConnection, ids: &BTreeSet<Uuid>) -> sqlx::Result<()> {
    let ids: Vec<Uuid> = ids.iter().copied().collect();
    sqlx::query!(
        "SELECT id FROM artists WHERE id = ANY($1) ORDER BY id FOR UPDATE",
        &ids
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(())
}

async fn artist_by_key(conn: &mut PgConnection, key: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!("SELECT id FROM artists WHERE name_key = $1", key)
        .fetch_optional(&mut *conn)
        .await
}

async fn artist_exists(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<bool> {
    Ok(
        sqlx::query_scalar!("SELECT id FROM artists WHERE id = $1", id)
            .fetch_optional(&mut *conn)
            .await?
            .is_some(),
    )
}

/// Whether the artist has an album other than `album` (one whose tracks are all missing counts).
async fn has_other_album(conn: &mut PgConnection, artist: Uuid, album: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM albums WHERE artist_id = $1 AND id <> $2) AS "e!""#,
        artist,
        album
    )
    .fetch_one(&mut *conn)
    .await
}

/// What to do with the album artist, apply or undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtistPlan {
    /// The album stays on its artist.
    Keep,
    /// Move the album to this existing row.
    Reparent(Uuid),
    /// Rename the album's own artist row in place.
    Rename,
    /// Create a row for the name and move the album to it.
    Create,
}

/// Spec §19.7's three rules for the name `key` on an album now on `current`: a row with the
/// key exists → re-parent (or keep, when it is `current`); else `current` left with no album →
/// rename in place; else create. The Various Artists row is never renamed, and no row is renamed
/// into it: both create instead.
async fn plan_for_name(
    conn: &mut PgConnection,
    album: Uuid,
    current: Uuid,
    key: &str,
) -> sqlx::Result<ArtistPlan> {
    Ok(match artist_by_key(conn, key).await? {
        Some(id) if id == current => ArtistPlan::Keep,
        Some(id) => ArtistPlan::Reparent(id),
        None if key == VARIOUS_ARTISTS_KEY || is_various(conn, current).await? => {
            ArtistPlan::Create
        }
        None if !has_other_album(conn, current, album).await? => ArtistPlan::Rename,
        None => ArtistPlan::Create,
    })
}

async fn is_various(conn: &mut PgConnection, artist: Uuid) -> sqlx::Result<bool> {
    Ok(
        sqlx::query_scalar!("SELECT name_key FROM artists WHERE id = $1", artist)
            .fetch_optional(&mut *conn)
            .await?
            .as_deref()
            == Some(VARIOUS_ARTISTS_KEY),
    )
}

/// Creates an artist row (enrichment `pending`; Various Artists with its MBID, `completed`);
/// answers its id and whether it was created (`false`: a row with the key appeared meanwhile).
async fn create_artist(
    conn: &mut PgConnection,
    name: &str,
    key: &str,
) -> sqlx::Result<(Uuid, bool)> {
    let va = key == VARIOUS_ARTISTS_KEY;
    let created = sqlx::query_scalar!(
        "INSERT INTO artists (id, name, name_key, mbid, enrichment_status)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (name_key) DO NOTHING RETURNING id",
        Uuid::now_v7(),
        name,
        key,
        va.then_some(VARIOUS_ARTISTS_MBID),
        if va { "completed" } else { "pending" }
    )
    .fetch_optional(&mut *conn)
    .await?;
    match created {
        Some(id) => Ok((id, true)),
        None => {
            let id =
                sqlx::query_scalar!("SELECT id FROM artists WHERE name_key = $1 FOR UPDATE", key)
                    .fetch_one(&mut *conn)
                    .await?;
            Ok((id, false))
        }
    }
}

async fn rename_artist(
    conn: &mut PgConnection,
    id: Uuid,
    name: &str,
    key: &str,
) -> sqlx::Result<()> {
    // Touching `name` fires the M1 rename trigger: the search vectors follow.
    sqlx::query!(
        "UPDATE artists SET name = $2, name_key = $3 WHERE id = $1",
        id,
        name,
        key
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Applies the run's album identity (spec §19.6, §19.7) on the caller's transaction: locks (see
/// the module doc), re-checks the collision, records `album_before` and the artist step on the
/// run, updates the album row in place (title, `title_key`, year, `is_compilation`, artist,
/// `identity_source='autotag'`, `applied_identity`), its MBIDs with source `autotag` unless the
/// current source is `user` (R2), its MusicBrainz genres (when the applied list is not empty),
/// the album artist's MBID (one credit, source not `user`), each named track's recording MBID
/// (closing its `recording` stage), and supersedes previewed organizer runs.
///
/// With `applied.mbids_only` the identity fields and the artist step are skipped
/// (`artist_action='none'`) and the album's artist row is left alone (no artist MBID either).
///
/// Re-applying a run is a no-op: a run that already has `album_before` answers its recorded
/// step and writes nothing, so the first capture is never overwritten. This only makes a crash
/// between this commit and the run's end safe; callers (Task 16) must never re-apply a run on
/// purpose (a new apply is a new run).
pub async fn apply_identity(
    conn: &mut PgConnection,
    run: &RunRow,
    keys: &IdentityKeys,
) -> Result<ArtistStep, IdentityError> {
    let applied: Applied = run.applied().map_err(decode)?;
    let album = run.album_id;
    let Some(recorded) = lock_run(conn, run.id).await? else {
        return Err(IdentityError::AlbumGone);
    };
    if recorded.album_before.is_some() {
        return Ok(ArtistStep {
            action: action_str(recorded.artist_action.as_deref()),
            old_artist_id: recorded.old_artist_id,
            new_artist_id: recorded.new_artist_id,
            old_artist_name: recorded.old_artist_name,
        });
    }
    let named: Vec<Uuid> = applied.tracks.iter().map(|t| t.track_id).collect();
    lock_tracks(conn, album, &named).await?;
    let Some((current, current_year)) = lock_album(conn, album).await? else {
        return Err(IdentityError::AlbumGone);
    };

    let plan = if applied.mbids_only {
        lock_artists(conn, &BTreeSet::from([current])).await?;
        ArtistPlan::Keep
    } else {
        let mut ids = BTreeSet::from([current]);
        ids.extend(artist_by_key(conn, &keys.artist_key).await?);
        lock_artists(conn, &ids).await?;
        // Under the locks: no scan can add an album to these artists any more.
        if let Some(c) = find_collision(conn, album, keys, applied.album.year).await? {
            return Err(IdentityError::Collision(c));
        }
        plan_for_name(conn, album, current, &keys.artist_key).await?
    };

    let mut before = capture(conn, album).await?;
    let (step, artist) = match plan {
        ArtistPlan::Keep => (
            ArtistStep {
                action: "none",
                old_artist_id: Some(current),
                new_artist_id: Some(current),
                old_artist_name: None,
            },
            current,
        ),
        ArtistPlan::Reparent(id) => (reparented(current, id), id),
        ArtistPlan::Rename => {
            rename_artist(conn, current, &applied.album.artist, &keys.artist_key).await?;
            if before.artist_mbid == Some(VARIOUS_ARTISTS_MBID) {
                // The Various Artists MBID never stays on a renamed row.
                sqlx::query!(
                    "UPDATE artists SET mbid = NULL, mbid_source = NULL WHERE id = $1",
                    current
                )
                .execute(&mut *conn)
                .await?;
                before.artist_mbid_write = Some(ArtistMbidWrite {
                    id: current,
                    mbid: before.artist_mbid,
                    mbid_source: before.artist_mbid_source.clone(),
                    written: None,
                });
            }
            (
                ArtistStep {
                    action: "renamed",
                    old_artist_id: Some(current),
                    new_artist_id: Some(current),
                    old_artist_name: Some(before.artist_name.clone()),
                },
                current,
            )
        }
        ArtistPlan::Create => {
            let (id, created) =
                create_artist(conn, &applied.album.artist, &keys.artist_key).await?;
            if created {
                (
                    ArtistStep {
                        action: "created",
                        old_artist_id: Some(current),
                        new_artist_id: Some(id),
                        old_artist_name: None,
                    },
                    id,
                )
            } else {
                (reparented(current, id), id)
            }
        }
    };

    if !applied.mbids_only {
        let year = applied.album.year.or(current_year);
        let identity = serde_json::to_value(AppliedIdentity {
            artist_key: keys.artist_key.clone(),
            title_key: keys.title_key.clone(),
            year,
        })
        .map_err(decode)?;
        sqlx::query!(
            "UPDATE albums SET title = $2, title_key = $3, year = $4, is_compilation = $5,
                    artist_id = $6, identity_source = 'autotag', applied_identity = $7
             WHERE id = $1",
            album,
            applied.album.title,
            keys.title_key,
            year,
            applied.album.compilation,
            artist,
            identity
        )
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query!(
        "UPDATE albums SET
           release_mbid = CASE WHEN release_mbid_source = 'user' THEN release_mbid ELSE $2 END,
           release_mbid_source = CASE WHEN release_mbid_source = 'user' THEN 'user'
                                      ELSE 'autotag' END,
           release_group_mbid = CASE WHEN release_group_mbid_source = 'user'
                                     THEN release_group_mbid ELSE $3 END,
           release_group_mbid_source = CASE WHEN release_group_mbid_source = 'user' THEN 'user'
                                            ELSE 'autotag' END
         WHERE id = $1",
        album,
        applied.album.release_mbid,
        applied.album.release_group_mbid
    )
    .execute(&mut *conn)
    .await?;
    if !applied.album.genres.is_empty() {
        genres::replace_album_genres(conn, album, GenreSource::MusicBrainz, &applied.album.genres)
            .await?;
    }
    // mbids_only leaves the album's artist row alone (C3 ruling).
    if let ([mbid], false) = (applied.album.artist_mbids.as_slice(), applied.mbids_only) {
        let prior = sqlx::query!(
            "SELECT mbid, mbid_source FROM artists WHERE id = $1",
            artist
        )
        .fetch_one(&mut *conn)
        .await?;
        let wrote = sqlx::query!(
            "UPDATE artists SET mbid = $2, mbid_source = 'autotag'
             WHERE id = $1 AND mbid_source IS DISTINCT FROM 'user'",
            artist,
            mbid
        )
        .execute(&mut *conn)
        .await?
        .rows_affected()
            > 0;
        // A row this apply created has nothing to give back.
        if wrote && step.action != "created" {
            match &mut before.artist_mbid_write {
                Some(w) if w.id == artist => w.written = Some(*mbid),
                _ => {
                    before.artist_mbid_write = Some(ArtistMbidWrite {
                        id: artist,
                        mbid: prior.mbid,
                        mbid_source: prior.mbid_source,
                        written: Some(*mbid),
                    })
                }
            }
        }
    }
    // The recording rows of several tracks are written below. A recording claim holds the
    // existing rows it takes and waits only for rows being created (`stages::claim`), so lock
    // the existing rows first and then create the missing ones in track-id order, as the claim
    // does: otherwise the claim waits for a row created here while this waits for one it holds.
    let mut recorded: Vec<(Uuid, Uuid)> = applied
        .tracks
        .iter()
        .filter_map(|t| t.recording_mbid.map(|rec| (t.track_id, rec)))
        .collect();
    recorded.sort_unstable();
    let ids: Vec<Uuid> = recorded.iter().map(|(t, _)| *t).collect();
    sqlx::query!(
        "SELECT track_id FROM track_stages WHERE stage = 'recording' AND track_id = ANY($1)
         ORDER BY track_id FOR NO KEY UPDATE",
        &ids
    )
    .fetch_all(&mut *conn)
    .await?;
    for (track, rec) in recorded {
        let hit = sqlx::query_scalar!(
            "UPDATE tracks SET recording_mbid = $3, recording_source = 'autotag', updated_at = now()
             WHERE id = $1 AND album_id = $2 RETURNING id",
            track,
            album,
            rec
        )
        .fetch_optional(&mut *conn)
        .await?;
        if hit.is_some() {
            crate::stages::close_recording_autotag(conn, track).await?;
        }
    }

    let before = serde_json::to_value(&before).map_err(decode)?;
    sqlx::query!(
        "UPDATE autotag_runs SET album_before = $2, artist_action = $3, old_artist_id = $4,
                new_artist_id = $5, old_artist_name = $6
         WHERE id = $1",
        run.id,
        before,
        step.action,
        step.old_artist_id,
        step.new_artist_id,
        step.old_artist_name
    )
    .execute(&mut *conn)
    .await?;
    crate::organizer::supersede_previewed(conn).await?;
    Ok(step)
}

fn reparented(old: Uuid, new: Uuid) -> ArtistStep {
    ArtistStep {
        action: "reparented",
        old_artist_id: Some(old),
        new_artist_id: Some(new),
        old_artist_name: None,
    }
}

/// How undo moves the album artist back (spec §19.7 "Undo, in reverse"): `reparented` or
/// `created` → back to `old_artist_id`, or by the three rules for the old name when that row is
/// gone; `renamed` → the old name back on the row when its key is free and the row has no other
/// album, else to the row holding the name, else a new row for it; `none` and `mbids_only` →
/// the album stays.
async fn undo_plan(
    conn: &mut PgConnection,
    album: Uuid,
    current: Uuid,
    applied: &Applied,
    action: Option<&str>,
    old_artist: Option<Uuid>,
    before: &AlbumBefore,
) -> sqlx::Result<ArtistPlan> {
    if applied.mbids_only {
        return Ok(ArtistPlan::Keep);
    }
    match action {
        Some("reparented" | "created") => {
            if let Some(old) = old_artist
                && artist_exists(conn, old).await?
            {
                return Ok(if old == current {
                    ArtistPlan::Keep
                } else {
                    ArtistPlan::Reparent(old)
                });
            }
            plan_for_name(conn, album, current, &before.artist_key).await
        }
        Some("renamed") => plan_for_name(conn, album, current, &before.artist_key).await,
        _ => Ok(ArtistPlan::Keep),
    }
}

/// The artist the undo would put the album on, when it already exists.
fn undo_target(plan: ArtistPlan, current: Uuid) -> Option<Uuid> {
    match plan {
        ArtistPlan::Keep => Some(current),
        ArtistPlan::Reparent(id) => Some(id),
        // A renamed-back row has no other album; a created row has none.
        ArtistPlan::Rename | ArtistPlan::Create => None,
    }
}

fn parse_before(v: &Option<Value>) -> sqlx::Result<Option<AlbumBefore>> {
    v.as_ref()
        .map(|v| serde_json::from_value(v.clone()).map_err(decode))
        .transpose()
}

/// The album that already holds the identity the undo would restore (from `album_before`,
/// against the artist row the undo would use), checked before any file is touched. `None` when
/// the run was never applied, the album is gone, or the identity is free.
pub async fn undo_collision(
    conn: &mut PgConnection,
    run: &RunRow,
) -> sqlx::Result<Option<ConflictAlbum>> {
    let applied: Applied = run.applied().map_err(decode)?;
    let Some(before) = parse_before(&run.album_before)? else {
        return Ok(None);
    };
    if applied.mbids_only {
        return Ok(None);
    }
    let Some(current) =
        sqlx::query_scalar!("SELECT artist_id FROM albums WHERE id = $1", run.album_id)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(None);
    };
    let plan = undo_plan(
        conn,
        run.album_id,
        current,
        &applied,
        run.artist_action.as_deref(),
        run.old_artist_id,
        &before,
    )
    .await?;
    match undo_target(plan, current) {
        Some(artist) => {
            collision_on_artist(conn, run.album_id, artist, &before.title_key, before.year).await
        }
        None => Ok(None),
    }
}

/// Undoes [`apply_identity`] on the caller's transaction (same lock order): restores the album
/// row (title, `title_key`, year, compilation flag, `identity_source`, `applied_identity`),
/// reverses the artist step, puts back the album MBIDs and sources, the MusicBrainz album
/// genres, the artist MBID the apply changed and each track's recording MBID and source, each
/// only while it still holds the very value this run wrote (source `autotag` and the same id):
/// a later `user` edit, another run's write or a new identification answer stays. A track
/// whose answer comes back gets its `done` stage row back; one that had none gets its
/// `recording` stage back to `pending`. Deletes the run's overrides, supersedes previewed
/// organizer runs and sets the album `proposed`. A collision of the old identity refuses the
/// undo with nothing changed.
pub async fn undo_identity(conn: &mut PgConnection, run: &RunRow) -> Result<(), IdentityError> {
    let applied: Applied = run.applied().map_err(decode)?;
    let album = run.album_id;
    let Some(recorded) = lock_run(conn, run.id).await? else {
        return Err(IdentityError::AlbumGone);
    };
    let before = parse_before(&recorded.album_before)?;
    let mut named: Vec<Uuid> = applied.tracks.iter().map(|t| t.track_id).collect();
    if let Some(b) = &before {
        named.extend(b.tracks.iter().map(|t| t.track_id));
    }
    lock_tracks(conn, album, &named).await?;
    let Some((current, _)) = lock_album(conn, album).await? else {
        return Err(IdentityError::AlbumGone);
    };

    if let Some(before) = &before {
        let mut ids = BTreeSet::from([current]);
        ids.extend(recorded.old_artist_id);
        ids.extend(artist_by_key(conn, &before.artist_key).await?);
        if let Some(w) = &before.artist_mbid_write {
            ids.insert(w.id);
        }
        lock_artists(conn, &ids).await?;
        let plan = undo_plan(
            conn,
            album,
            current,
            &applied,
            recorded.artist_action.as_deref(),
            recorded.old_artist_id,
            before,
        )
        .await?;
        if let Some(artist) = undo_target(plan, current)
            && let Some(c) =
                collision_on_artist(conn, album, artist, &before.title_key, before.year).await?
        {
            return Err(IdentityError::Collision(c));
        }
        restore(conn, album, current, &applied, plan, before).await?;
    }

    delete_overrides(conn, run.id).await?;
    crate::organizer::supersede_previewed(conn).await?;
    sqlx::query!(
        "INSERT INTO autotag_albums (album_id, state) VALUES ($1, 'proposed')
         ON CONFLICT (album_id) DO UPDATE SET state = 'proposed', error = NULL,
           updated_at = now()",
        album
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn restore(
    conn: &mut PgConnection,
    album: Uuid,
    current: Uuid,
    applied: &Applied,
    plan: ArtistPlan,
    before: &AlbumBefore,
) -> sqlx::Result<()> {
    let mut created_row = None;
    let artist = match plan {
        ArtistPlan::Keep => current,
        ArtistPlan::Reparent(id) => id,
        ArtistPlan::Rename => {
            rename_artist(conn, current, &before.artist_name, &before.artist_key).await?;
            current
        }
        ArtistPlan::Create => {
            let (id, created) =
                create_artist(conn, &before.artist_name, &before.artist_key).await?;
            created_row = created.then_some(id);
            id
        }
    };
    if !applied.mbids_only {
        sqlx::query!(
            "UPDATE albums SET title = $2, title_key = $3, year = $4, is_compilation = $5,
                    artist_id = $6, identity_source = $7, applied_identity = $8
             WHERE id = $1",
            album,
            before.title,
            before.title_key,
            before.year,
            before.is_compilation,
            artist,
            before.identity_source,
            before.applied_identity
        )
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query!(
        "UPDATE albums SET
           release_mbid = CASE WHEN release_mbid_source = 'autotag' AND release_mbid = $6
                               THEN $2 ELSE release_mbid END,
           release_mbid_source = CASE WHEN release_mbid_source = 'autotag' AND release_mbid = $6
                                      THEN $3 ELSE release_mbid_source END,
           release_group_mbid = CASE WHEN release_group_mbid_source = 'autotag'
                                          AND release_group_mbid = $7
                                     THEN $4 ELSE release_group_mbid END,
           release_group_mbid_source = CASE WHEN release_group_mbid_source = 'autotag'
                                                 AND release_group_mbid = $7
                                            THEN $5 ELSE release_group_mbid_source END
         WHERE id = $1",
        album,
        before.release_mbid,
        before.release_mbid_source,
        before.release_group_mbid,
        before.release_group_mbid_source,
        applied.album.release_mbid,
        applied.album.release_group_mbid
    )
    .execute(&mut *conn)
    .await?;
    if !applied.album.genres.is_empty() {
        genres::replace_album_genres(conn, album, GenreSource::MusicBrainz, &before.mb_genres)
            .await?;
    }
    // Only the row the apply wrote, and only while it still holds the apply's value: a later
    // run or a user edit on the same artist is left alone. A row the apply renamed that keeps
    // its new name (it gained albums, or another row holds the old name) keeps its MBID: that
    // MBID belongs to its new identity. The old identity's MBID goes to a row created for the
    // old name instead, never crossing the Various Artists line (the VA MBID only on the VA
    // name, and only the VA name gets it).
    let renamed_away = before.artist_mbid_write.as_ref().is_some_and(|w| {
        w.id == current
            && w.id == before.artist_id
            && matches!(plan, ArtistPlan::Reparent(_) | ArtistPlan::Create)
    });
    if renamed_away {
        if let (Some(id), Some(mbid)) = (created_row, before.artist_mbid)
            && (mbid == VARIOUS_ARTISTS_MBID) == (before.artist_key == VARIOUS_ARTISTS_KEY)
        {
            sqlx::query!(
                "UPDATE artists SET mbid = $2, mbid_source = $3 WHERE id = $1",
                id,
                mbid,
                before.artist_mbid_source
            )
            .execute(&mut *conn)
            .await?;
        }
    } else if let Some(w) = &before.artist_mbid_write {
        sqlx::query!(
            "UPDATE artists SET mbid = $2, mbid_source = $3
             WHERE id = $1 AND mbid IS NOT DISTINCT FROM $4
               AND mbid_source IS NOT DISTINCT FROM
                   (CASE WHEN $4::uuid IS NULL THEN NULL ELSE 'autotag' END)",
            w.id,
            w.mbid,
            w.mbid_source,
            w.written
        )
        .execute(&mut *conn)
        .await?;
    }
    let written: HashMap<Uuid, Uuid> = applied
        .tracks
        .iter()
        .filter_map(|t| t.recording_mbid.map(|r| (t.track_id, r)))
        .collect();
    for t in &before.tracks {
        let Some(rec) = written.get(&t.track_id) else {
            continue;
        };
        let hit = sqlx::query_scalar!(
            "UPDATE tracks SET recording_mbid = $2, recording_source = $3, updated_at = now()
             WHERE id = $1 AND recording_source = 'autotag' AND recording_mbid = $4
             RETURNING id",
            t.track_id,
            t.recording_mbid,
            t.recording_source,
            rec
        )
        .fetch_optional(&mut *conn)
        .await?;
        if hit.is_none() {
            continue;
        }
        match (&t.recording_mbid, &t.recording_stage) {
            (Some(_), Some(s)) if s.status == "done" => {
                let finished = s.finished_at.as_str();
                sqlx::query!(
                    "UPDATE track_stages
                     SET status = 'done', source = $2, evidence = $3,
                         finished_at = $4::text::timestamptz, next_attempt_at = NULL,
                         last_error = NULL, error_code = NULL, error_class = NULL,
                         updated_at = now()
                     WHERE track_id = $1 AND stage = 'recording'",
                    t.track_id,
                    s.source,
                    s.evidence,
                    finished
                )
                .execute(&mut *conn)
                .await?;
            }
            _ => {
                sqlx::query!(
                    r#"UPDATE track_stages
                       SET status = 'pending', attempts = 0, redelivered = 0,
                           next_attempt_at = NULL, last_error = NULL, error_code = NULL,
                           error_class = NULL, evidence = '{"reason":"autotag undone"}'::jsonb,
                           source = NULL, started_at = NULL, finished_at = NULL,
                           updated_at = now()
                       WHERE track_id = $1 AND stage = 'recording'"#,
                    t.track_id
                )
                .execute(&mut *conn)
                .await?;
            }
        }
    }
    Ok(())
}

/// Stores the database-only target's tag overrides (`track_tag_overrides`, spec §19.7), one
/// per track, replacing a track's earlier override.
pub async fn put_overrides(
    conn: &mut PgConnection,
    run_id: Uuid,
    overrides: &[(Uuid, Value)],
) -> sqlx::Result<()> {
    for (track, fields) in overrides {
        sqlx::query!(
            "INSERT INTO track_tag_overrides (track_id, run_id, fields) VALUES ($1, $2, $3)
             ON CONFLICT (track_id) DO UPDATE SET run_id = EXCLUDED.run_id,
               fields = EXCLUDED.fields, created_at = now()",
            track,
            run_id,
            fields
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Deletes the run's overrides; answers how many.
pub async fn delete_overrides(conn: &mut PgConnection, run_id: Uuid) -> sqlx::Result<u64> {
    let r = sqlx::query!("DELETE FROM track_tag_overrides WHERE run_id = $1", run_id)
        .execute(&mut *conn)
        .await?;
    Ok(r.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_with_and_without_year() {
        let mut c = ConflictAlbum {
            album_id: Uuid::nil(),
            artist: "A".into(),
            title: "T".into(),
            year: Some(1999),
        };
        assert_eq!(
            c.message(),
            "Another album already has this identity: A \u{2013} T (1999)"
        );
        c.year = None;
        assert_eq!(
            c.message(),
            "Another album already has this identity: A \u{2013} T"
        );
    }
}
