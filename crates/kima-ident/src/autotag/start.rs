//! Starting an apply (spec §19.5 "Accept", §19.9 step 1 at request time): the request's checks
//! mirror the apply job's pre-check, so a request that the job would refuse is refused at once
//! with a clear reason and no run is created. The job checks again under its row locks.
//!
//! The §19.6 values are computed here from the candidate's stored snapshot (§19.2 step 6, R3)
//! and stored on the run; nothing is fetched. A snapshot older than the review page's 30-day
//! "stale" mark is still applied: §19.2 says apply works from the old snapshot (the page offers
//! "Refresh candidates", it does not block Accept).

use kima_db::PgPool;
use kima_db::autotag::{self as adb, Applied, CandidateRow, ConflictAlbum, IdentityKeys, Target};
use kima_library::autotag::apply::{ALBUM_BUSY, settle_failed};
use kima_library::autotag::capability::{album_refusal, capabilities};
use kima_library::autotag::{
    APPLIED_MATCH_EXISTS, COPY_FIRST, FILES_READ_ONLY, REORGANIZE_FILES_ONLY, TRACKS_CHANGED,
    enqueue_apply,
};
use kima_library::names;
use kima_library::scan::LibraryCtx;
use std::collections::BTreeSet;
use tracing::warn;
use uuid::Uuid;

use super::proposal;
use super::score::{MappedPair, Mapping, MissingTrack};
use crate::musicbrainz::{Release, ReleaseGroup};

/// The album or the candidate is gone, or the candidate belongs to another album.
pub const NOT_FOUND: &str = "Album or match not found";
/// The candidate's stored snapshot does not parse (it cannot be applied as it is).
pub const SNAPSHOT_UNREADABLE: &str = "The stored match cannot be read; search again";
/// The run was created but its job could not be queued (the run is `failed` with this, then
/// the queue's error).
pub const NOT_QUEUED: &str = "The apply could not be queued";

/// An "Accept" of a candidate (spec §16.5 `POST /library/autotag/albums/{albumId}/apply`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyRequest {
    pub album_id: Uuid,
    pub candidate_id: Uuid,
    pub target: Target,
    /// Re-file the album after tagging (`files` only).
    pub reorganize: bool,
    /// "Apply MBIDs only": target `db`, the album's identity fields unchanged.
    pub mbids_only: bool,
    /// The admin who asked; `None` for an auto-apply.
    pub created_by: Option<Uuid>,
}

/// The run created and its queued job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started {
    pub run_id: Uuid,
    pub job_id: Uuid,
}

/// Why no apply was started. HTTP (Tasks 20/21): `NotFound` 404, `Invalid` 400, `Busy`,
/// `Unavailable` and `Collision` 409 (`Collision` with `conflictAlbumId`), `Db` 500.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{NOT_FOUND}")]
    NotFound,
    /// The request itself is wrong (e.g. `reorganize` without the `files` target).
    #[error("{0}")]
    Invalid(String),
    /// The target cannot be used for this album now, or the album cannot take this match
    /// (an applied match stands, its tracks changed, the snapshot is unreadable).
    #[error("{0}")]
    Unavailable(String),
    /// Another run of the album, or an organizer run that may move its files, is working.
    #[error("{ALBUM_BUSY}")]
    Busy,
    /// Another album already has the new identity (spec §19.7).
    #[error("{}", .0.message())]
    Collision(ConflictAlbum),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// The candidate's stored release, release group and track mapping.
fn snapshot(c: &CandidateRow) -> Result<(Release, ReleaseGroup, Mapping), serde_json::Error> {
    let release: Release = serde_json::from_value(c.release.clone())?;
    let group: ReleaseGroup = serde_json::from_value(c.release_group.clone())?;
    let pairs: Vec<MappedPair> = serde_json::from_value(c.mapping.clone())?;
    let extra: Vec<Uuid> = serde_json::from_value(c.extra_tracks.clone())?;
    let missing: Vec<MissingTrack> = serde_json::from_value(c.missing_tracks.clone())?;
    Ok((
        release,
        group,
        Mapping {
            pairs,
            extra,
            missing,
        },
    ))
}

/// The §19.6 values the run stores: [`proposal::applied`] of the snapshot, or for `mbids_only`
/// its MBIDs-only form filled with the album's own names ([`proposal::with_local_identity`]).
pub fn applied_values(
    candidate: &CandidateRow,
    local: &adb::LocalAlbum,
    mbids_only: bool,
) -> Result<Applied, serde_json::Error> {
    let (release, group, mapping) = snapshot(candidate)?;
    let mut applied = proposal::applied(&release, &group, &mapping);
    if mbids_only {
        applied = proposal::mbids_only(applied);
        proposal::with_local_identity(&mut applied, local);
    }
    Ok(applied)
}

/// Starts the apply of `req` (see the module doc). In order: the candidate must be the album's
/// and the album must have a present track (`NotFound`); `mbids_only` forces `db`; `reorganize`
/// needs `files` (`Invalid`); the target must be available and the album writable for it
/// (`Unavailable` with the capability's reason); the snapshot must parse. Then, in one
/// transaction under the job's row locks (tracks, then the album): no other run of the album is
/// working and no organizer run may move its files (`Busy`); no applied match stands (another
/// run's overrides, a run that needs undo, or the album's `identity_source='autotag'`:
/// `Unavailable(APPLIED_MATCH_EXISTS)`); the present tracks are the snapshot's
/// (`Unavailable(TRACKS_CHANGED)`); unless `mbids_only`, no other album has the new identity
/// (`Collision`); the run is created (`queued`, the album `applying`). Last, the job is queued:
/// if that fails the run is settled `failed` (the album back to what it was before) and `Db` is
/// answered.
pub async fn start(
    pool: &PgPool,
    lib: &LibraryCtx,
    req: &ApplyRequest,
) -> Result<Started, StartError> {
    let album = req.album_id;
    let candidate = adb::candidate(pool, req.candidate_id)
        .await?
        .filter(|c| c.album_id == album)
        .ok_or(StartError::NotFound)?;
    let local = adb::local_album(pool, album)
        .await?
        .ok_or(StartError::NotFound)?;
    let target = if req.mbids_only {
        Target::Db
    } else {
        req.target
    };
    if req.reorganize && target != Target::Files {
        return Err(StartError::Invalid(REORGANIZE_FILES_ONLY.to_string()));
    }
    if target != Target::Db {
        let caps = capabilities(lib).await?;
        let (available, fallback) = match target {
            Target::Files => (&caps.files, FILES_READ_ONLY),
            _ => (&caps.copy, COPY_FIRST),
        };
        if !available.available {
            return Err(StartError::Unavailable(
                available
                    .reason
                    .clone()
                    .unwrap_or_else(|| fallback.to_string()),
            ));
        }
    }
    if let Some(reason) = album_refusal(lib, &local, target).await? {
        return Err(StartError::Unavailable(reason));
    }
    let applied = applied_values(&candidate, &local, req.mbids_only)
        .map_err(|_| StartError::Unavailable(SNAPSHOT_UNREADABLE.to_string()))?;
    let keys = IdentityKeys {
        artist_key: names::key(&applied.album.artist),
        title_key: names::key(&applied.album.title),
    };

    let run_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    let Some(present) = adb::lock_album_tracks(&mut tx, album).await? else {
        return Err(StartError::NotFound);
    };
    if adb::other_active_run(&mut tx, album, run_id)
        .await?
        .is_some()
        || adb::organizer_busy(&mut tx, album).await?
    {
        return Err(StartError::Busy);
    }
    if adb::foreign_override(&mut tx, album, run_id).await?
        || adb::needs_undo_run(&mut tx, album, run_id).await?.is_some()
        || adb::album_identity_applied(&mut tx, album).await?
    {
        return Err(StartError::Unavailable(APPLIED_MATCH_EXISTS.to_string()));
    }
    // The pre-lock capability check used `local` paths and symlink flags. Do not create
    // a run from that verdict if the locked snapshot no longer describes those files.
    if target != Target::Db
        && (present.iter().any(|t| t.via_symlink)
            || present.iter().any(|t| {
                !local
                    .tracks
                    .iter()
                    .any(|old| old.id == t.id && old.path == t.path)
            }))
    {
        return Err(StartError::Unavailable(TRACKS_CHANGED.to_string()));
    }
    let want: BTreeSet<Uuid> = applied.tracks.iter().map(|t| t.track_id).collect();
    let have: BTreeSet<Uuid> = present.iter().map(|t| t.id).collect();
    if want != have || want.len() != applied.tracks.len() {
        return Err(StartError::Unavailable(TRACKS_CHANGED.to_string()));
    }
    if !applied.mbids_only
        && let Some(c) = adb::find_collision(&mut tx, album, &keys, applied.album.year).await?
    {
        return Err(StartError::Collision(c));
    }
    let run = adb::create_run(
        &mut tx,
        &adb::NewRun {
            id: run_id,
            album_id: album,
            candidate_id: candidate.id,
            applied,
            target,
            reorganize: req.reorganize,
            created_by: req.created_by,
        },
    )
    .await?;
    tx.commit().await?;

    match enqueue_apply(pool, run.id).await {
        Ok(job_id) => Ok(Started { run_id, job_id }),
        Err(e) => {
            // `finish_run` (inside `settle_failed`) moves the run from any state: here it is
            // still `queued` with no job, which nothing else would pick up before a restart.
            let msg = format!("{NOT_QUEUED}: {e}");
            warn!(run = %run.id, album = %album, error = %e, "autotag apply not queued");
            if let Err(e) = settle_failed(lib, &run, &msg).await {
                warn!(run = %run.id, "cannot settle the unqueued run: {e}");
            }
            Err(StartError::Db(e))
        }
    }
}
