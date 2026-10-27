//! `autotag_albums` and `autotag_candidates` (spec §6.8): the state of an album's search, the
//! candidates it found and the local view of an album the matcher compares them with.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// `autotag_albums.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumState {
    Queued,
    Searching,
    Proposed,
    NoCandidates,
    Failed,
    Applying,
    Applied,
    Skipped,
}

impl AlbumState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Searching => "searching",
            Self::Proposed => "proposed",
            Self::NoCandidates => "no_candidates",
            Self::Failed => "failed",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Skipped => "skipped",
        }
    }
}

/// `recommendation` of an album or a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recommendation {
    #[serde(rename = "strong")]
    Strong,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "none")]
    Weak,
}

impl Recommendation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Medium => "medium",
            Self::Weak => "none",
        }
    }
}

/// How a candidate was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Tag,
    Search,
    Recording,
    Manual,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Search => "search",
            Self::Recording => "recording",
            Self::Manual => "manual",
        }
    }
}

/// A scored candidate about to be stored.
#[derive(Debug, Clone)]
pub struct NewCandidate {
    pub release_mbid: Uuid,
    pub release_group_mbid: Uuid,
    pub origin: Origin,
    pub distance: f64,
    pub recommendation: Recommendation,
    pub penalties: Value,
    pub mapping: Value,
    pub extra_tracks: Value,
    pub missing_tracks: Value,
    pub release: Value,
    pub release_group: Value,
}

/// A stored candidate: every `autotag_candidates` column.
#[derive(Debug, Clone)]
pub struct CandidateRow {
    pub id: Uuid,
    pub album_id: Uuid,
    pub release_mbid: Uuid,
    pub release_group_mbid: Uuid,
    pub origin: String,
    pub distance: f64,
    pub recommendation: String,
    pub penalties: Value,
    pub mapping: Value,
    pub extra_tracks: Value,
    pub missing_tracks: Value,
    pub release: Value,
    pub release_group: Value,
    pub fetched_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
}

/// The album as the matcher sees it.
#[derive(Debug, Clone)]
pub struct LocalAlbum {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub artist_name: String,
    pub year: Option<i32>,
    pub is_compilation: bool,
    pub release_mbid: Option<Uuid>,
    pub release_mbid_source: Option<String>,
    pub release_group_mbid: Option<Uuid>,
    pub release_group_mbid_source: Option<String>,
    pub identity_source: String,
    pub tracks: Vec<LocalTrack>,
}

/// One present track of a [`LocalAlbum`].
#[derive(Debug, Clone)]
pub struct LocalTrack {
    pub id: Uuid,
    pub path: String,
    pub disc: i32,
    pub track_no: i32,
    pub title: String,
    pub artist_name: String,
    pub duration_secs: i32,
    pub recording_mbid: Option<Uuid>,
    pub recording_source: Option<String>,
    pub tag_release_mbid: Option<Uuid>,
    pub tag_artist_mbids: Vec<Uuid>,
    pub disc_total: Option<i32>,
    pub track_total: Option<i32>,
}

/// The album with its present tracks ordered `disc, track_no, path`; `None` when it does not
/// exist or has no present track.
pub async fn local_album(pool: &PgPool, album: Uuid) -> sqlx::Result<Option<LocalAlbum>> {
    let Some(a) = sqlx::query!(
        "SELECT al.id, al.title, al.artist_id, ar.name AS artist_name, al.year, al.is_compilation,
                al.release_mbid, al.release_mbid_source, al.release_group_mbid,
                al.release_group_mbid_source, al.identity_source
         FROM albums al JOIN artists ar ON ar.id = al.artist_id
         WHERE al.id = $1",
        album
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let tracks = sqlx::query!(
        "SELECT id, path, disc, track_no, title, artist_name, duration_secs, recording_mbid,
                recording_source, tag_release_mbid, tag_artist_mbids, disc_total, track_total
         FROM tracks WHERE album_id = $1 AND missing_since IS NULL
         ORDER BY disc, track_no, path",
        album
    )
    .fetch_all(pool)
    .await?;
    if tracks.is_empty() {
        return Ok(None);
    }
    Ok(Some(LocalAlbum {
        id: a.id,
        title: a.title,
        artist_id: a.artist_id,
        artist_name: a.artist_name,
        year: a.year,
        is_compilation: a.is_compilation,
        release_mbid: a.release_mbid,
        release_mbid_source: a.release_mbid_source,
        release_group_mbid: a.release_group_mbid,
        release_group_mbid_source: a.release_group_mbid_source,
        identity_source: a.identity_source,
        tracks: tracks
            .into_iter()
            .map(|t| LocalTrack {
                id: t.id,
                path: t.path,
                disc: t.disc,
                track_no: t.track_no,
                title: t.title,
                artist_name: t.artist_name,
                duration_secs: t.duration_secs,
                recording_mbid: t.recording_mbid,
                recording_source: t.recording_source,
                tag_release_mbid: t.tag_release_mbid,
                tag_artist_mbids: t.tag_artist_mbids,
                disc_total: t.disc_total,
                track_total: t.track_total,
            })
            .collect(),
    }))
}

/// Sets the album's search state (creating its row), with the failure text when there is one.
pub async fn set_state(
    pool: &PgPool,
    album: Uuid,
    state: AlbumState,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO autotag_albums (album_id, state, error) VALUES ($1, $2, $3)
         ON CONFLICT (album_id) DO UPDATE SET state = EXCLUDED.state, error = EXCLUDED.error,
           updated_at = now()",
        album,
        state.as_str(),
        error
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The caller must serialize calls per album (one search job per album).
/// Replaces the album's candidates with `ordered` (best first) in one transaction. A release
/// already stored keeps its id and is refreshed; the others are deleted. Returns the best id.
pub async fn store_search(
    pool: &PgPool,
    album: Uuid,
    ordered: &[NewCandidate],
) -> sqlx::Result<Option<Uuid>> {
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(ordered.len());
    for c in ordered {
        let id = sqlx::query_scalar!(
            "INSERT INTO autotag_candidates
               (id, album_id, release_mbid, release_group_mbid, origin, distance, recommendation,
                penalties, mapping, extra_tracks, missing_tracks, release, release_group)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             ON CONFLICT (album_id, release_mbid) DO UPDATE SET
               release_group_mbid = EXCLUDED.release_group_mbid, origin = EXCLUDED.origin,
               distance = EXCLUDED.distance, recommendation = EXCLUDED.recommendation,
               penalties = EXCLUDED.penalties, mapping = EXCLUDED.mapping,
               extra_tracks = EXCLUDED.extra_tracks, missing_tracks = EXCLUDED.missing_tracks,
               release = EXCLUDED.release, release_group = EXCLUDED.release_group,
               fetched_at = now()
             RETURNING id",
            Uuid::now_v7(),
            album,
            c.release_mbid,
            c.release_group_mbid,
            c.origin.as_str(),
            c.distance,
            c.recommendation.as_str(),
            c.penalties,
            c.mapping,
            c.extra_tracks,
            c.missing_tracks,
            c.release,
            c.release_group
        )
        .fetch_one(&mut *tx)
        .await?;
        ids.push(id);
    }
    sqlx::query!(
        "DELETE FROM autotag_candidates WHERE album_id = $1 AND id <> ALL($2)",
        album,
        &ids
    )
    .execute(&mut *tx)
    .await?;
    let best = ids.first().copied();
    let (state, recommendation) = match ordered.first() {
        Some(c) => (AlbumState::Proposed, Some(c.recommendation.as_str())),
        None => (AlbumState::NoCandidates, None),
    };
    sqlx::query!(
        "INSERT INTO autotag_albums
           (album_id, state, recommendation, best_candidate, searched_at, error)
         VALUES ($1, $2, $3, $4, now(), NULL)
         ON CONFLICT (album_id) DO UPDATE SET state = EXCLUDED.state,
           recommendation = EXCLUDED.recommendation, best_candidate = EXCLUDED.best_candidate,
           searched_at = now(), error = NULL, updated_at = now()",
        album,
        state.as_str(),
        recommendation,
        best
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(best)
}

/// The album's stored candidates, best distance first.
pub async fn candidates(pool: &PgPool, album: Uuid) -> sqlx::Result<Vec<CandidateRow>> {
    sqlx::query_as!(
        CandidateRow,
        "SELECT id, album_id, release_mbid, release_group_mbid, origin, distance, recommendation,
                penalties, mapping, extra_tracks, missing_tracks, release, release_group,
                fetched_at, created_at
         FROM autotag_candidates WHERE album_id = $1 ORDER BY distance, created_at, id",
        album
    )
    .fetch_all(pool)
    .await
}

pub async fn candidate(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<CandidateRow>> {
    sqlx::query_as!(
        CandidateRow,
        "SELECT id, album_id, release_mbid, release_group_mbid, origin, distance, recommendation,
                penalties, mapping, extra_tracks, missing_tracks, release, release_group,
                fetched_at, created_at
         FROM autotag_candidates WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
}

/// Albums with a present track and no release MBID.
pub async fn unmatched_albums(pool: &PgPool) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar!(
        r#"SELECT al.id AS "id!" FROM albums al
           WHERE al.release_mbid IS NULL
             AND EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = al.id AND t.missing_since IS NULL)
           ORDER BY al.id"#
    )
    .fetch_all(pool)
    .await
}
