//! The undo job (spec §19.9 "Undo", §19.7 "Undo, in reverse", on the concurrency-1 `library`
//! queue): puts an applied run's files, album row and artist back as they were.
//!
//! **Which run.** A `done` run; a `failed` run that left something to undo (`written` journal
//! rows, or a committed identity: `album_before` set); an `undo_failed` run (a retry). Undo
//! goes newest first: while a newer run of the album is undoable the older one is refused
//! ([`UNDO_NEWER_FIRST`]). The run moves to `undoing` (`start_undo`, which remembers where it
//! came from) before anything is touched.
//!
//! **Before anything is touched** (under the album's track and row locks), the job refuses,
//! putting the run back to its earlier state with the message: another working run of the album
//! ([`ALBUM_BUSY`]), a newer undoable run, a `failed` run with nothing to undo
//! ([`NOTHING_TO_UNDO`]), the old identity taken by another album (the §19.7 collision message);
//! then a `copy` run whose copy is gone or was switched to and is no longer the library
//! ([`COPY_GONE`]: the copy's files are never written outside the copy run's tree, and the
//! original library is never touched by a copy undo), a reorganize run still applying under its
//! own job ([`REORGANIZE_IN_PROGRESS`]), and a read-only mount where files are to be restored
//! ([`UNDO_READ_ONLY`]).
//!
//! **Steps.**
//! 1. The reorganize (`organizer_run_id`) is undone first through the organizer's own undo, so
//!    the files are back at the paths the apply journalled: `done` or `failed` (with moved
//!    rows) → `undoing` → undone; `undoing` (an interrupted undo) resumes; `applying` (an
//!    interrupted apply no job drives) is finished first, then undone; `previewed`,
//!    `superseded`, `planning` and `undone` moved nothing. An organizer refusal (the library
//!    root changed) refuses the whole undo with nothing touched; an organizer undo that leaves
//!    files at their new names ends the run `undo_failed` with the organizer's reason (the tags
//!    and identity are still restored).
//! 2. The `written` rows (and `undo_failed` ones, on a retry) in reverse `applied_seq`. The file
//!    is looked for at its track's current path, then at the journalled path (the reorganize may
//!    have moved it); the one hashing to `after_sha` gets `old_tags` back through the safe write
//!    (with the recorded ID3v2 version; `NULL` is v2.4), expecting `after_sha` on the copy, then
//!    re-read: `undone`. A file already carrying `old_tags` with the journalled audio (a crash
//!    after the rename) is `undone` without a write. Anything else is `undo_failed`
//!    [`UNDO_CHANGED`] for that row only. Leftover `.kima-tag-<row id>.*` copies are deleted
//!    first. The cancel token is checked between files.
//! 3. One transaction: `undo_identity` (album row, artist step, overrides; the collision is
//!    checked again under the locks) with `summary.identityRestored`; for a run whose identity
//!    was never committed only its overrides are deleted. Then `prune_empty` and the re-read:
//!    the restored library files as `TagWrite`, the album's other files as `Scan` (files whose
//!    restore failed are left to the next scan). A collision found here leaves the database as
//!    it is and re-reads nothing.
//! 4. The run `undone` and the album `proposed`; or the run `undo_failed` with the reason and
//!    the album `failed` with it (needs attention): such a run keeps blocking new applies while
//!    a file is left to restore or its identity was not restored, until an undo retry succeeds.
//!
//! **Crash recovery.** The run stays `undoing` and startup re-queues it; each step is
//! idempotent: the checks are skipped once passed (`summary.undoChecked`), the organizer step
//! once done (`organizerUndone`), the identity once restored (`identityRestored`); `undone` rows
//! are not touched again. A cancelled undo stays `undoing` (resumable); a database or I/O error
//! on the job's final attempt ends it `undo_failed` (see [`run_job`]).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use kima_db::autotag::journal::{self as jdb, JournalRow};
use kima_db::autotag::{self as adb, AlbumState, IdentityError, RunRow, RunState, Target};
use kima_db::organizer as odb;
use kima_jobs::worker::JobError;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::apply::{
    ALBUM_BUSY, ApplyHooks, AutotagError, FailPoint, io, mark_renamed_planned, target_of,
};
use super::events::{AutotagEvents, AutotagJob};
use super::journal::{join_rel, scrub, tags_of};
use super::{
    COPY_GONE, NOTHING_TO_UNDO, REORGANIZE_IN_PROGRESS, UNDO_CHANGED, UNDO_INCOMPLETE,
    UNDO_NEWER_FIRST, UNDO_READ_ONLY,
};
use crate::jobs;
use crate::organize::apply::{
    self as organize, Action, OrganizeError, SYMLINK_NOTE, sha256_file, writable,
};
use crate::scan::{self, LibraryCtx, ReadCause};
use crate::tagwrite::{
    self, Id3Version, NativeTags, TagWriteError, WriteHooks, audio_md5, container_of, copy_path,
    read_native,
};

/// The reorganize's organizer run cannot be undone (the library root changed since).
pub const REORGANIZE_ROOT_CHANGED: &str =
    "The library root changed since the reorganize; this match cannot be undone";
/// A row whose file already carried its old tags (a crash after the restore's rename).
const ALREADY_RESTORED: &str = "already restored";
/// A `copy` row whose tagged copy is gone: nothing of the library to restore.
const COPY_TAG_GONE: &str = "the tagged copy is gone";
/// The restored file did not read back its old values.
const NOT_READ_BACK: &str = "the restored tags did not read back; see the journal";
/// Summary marks of the steps done (see the module doc), dropped when the run ends.
const MARKS: [&str; 3] = ["undoFrom", "undoChecked", "organizerUndone"];

type Result<T> = std::result::Result<T, AutotagError>;

/// The `autotag-undo` job body: [`run_undo`] without hooks. A refusal (already recorded on the
/// run) fails the job for good; an interruption is retried; on the `final_attempt` a database
/// or I/O error ends the run `undo_failed` with the error and the album `failed` (it needs
/// attention), never leaving it `undoing` with no job. Answers whether the library changed.
pub async fn run_job(
    ctx: &LibraryCtx,
    run_id: Uuid,
    cancel: &CancellationToken,
    final_attempt: bool,
) -> std::result::Result<bool, JobError> {
    run_job_with(ctx, run_id, &ApplyHooks::default(), cancel, final_attempt).await
}

/// [`run_job`] with test hooks.
pub async fn run_job_with(
    ctx: &LibraryCtx,
    run_id: Uuid,
    hooks: &ApplyHooks,
    cancel: &CancellationToken,
    final_attempt: bool,
) -> std::result::Result<bool, JobError> {
    match run_undo(ctx, run_id, hooks, cancel).await {
        Ok(()) => Ok(true),
        Err(AutotagError::Refused(m)) => Err(JobError::Permanent(m)),
        Err(AutotagError::Interrupted) => {
            Err(JobError::Retry(AutotagError::Interrupted.to_string()))
        }
        Err(e) if final_attempt => {
            let msg = scrub(&ctx.root.get(), &e.to_string());
            warn!(run = %run_id, error = %msg, "autotag undo failed on its final attempt");
            match adb::get_run(&ctx.pool, run_id).await {
                Ok(Some(run)) if run.state == RunState::Undoing.as_str() => {
                    if let Err(e) = finish(ctx, &run, Some(&msg), json!({})).await {
                        warn!(run = %run_id, "cannot settle the failed undo: {e}");
                    }
                }
                _ => {}
            }
            Err(JobError::Permanent(msg))
        }
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

/// Runs (or resumes) the undo of `run_id`; see the module doc. A `done`, `failed` or
/// `undo_failed` run is moved to `undoing` first; an `undoing` one resumes; any other state is
/// left alone.
pub async fn run_undo(
    ctx: &LibraryCtx,
    run_id: Uuid,
    hooks: &ApplyHooks,
    cancel: &CancellationToken,
) -> Result<()> {
    let Some(run) = adb::get_run(&ctx.pool, run_id).await? else {
        info!(run = %run_id, "autotag run gone; nothing to undo");
        return Ok(());
    };
    match run.state.as_str() {
        "done" | "failed" | "undo_failed" => {
            if !adb::start_undo(&ctx.pool, run.id).await? {
                return Ok(());
            }
        }
        "undoing" => {}
        other => {
            info!(run = %run.id, state = other, "autotag run not undoable; nothing to do");
            return Ok(());
        }
    }
    let Some(run) = adb::get_run(&ctx.pool, run_id).await? else {
        return Ok(());
    };
    let mut job = Undo {
        ctx,
        run: &run,
        hooks,
        cancel,
        events: AutotagEvents::new(ctx.events.clone()),
        total: 0,
        done: 0,
    };
    match job.undo().await {
        Err(AutotagError::Refused(msg)) => {
            warn!(run = %run.id, reason = %msg, "autotag undo refused");
            adb::revert_undo(&ctx.pool, run.id, &msg).await?;
            let state = adb::get_run(&ctx.pool, run.id)
                .await?
                .map(|r| r.state)
                .unwrap_or_default();
            job.finish_event(&state);
            Err(AutotagError::Refused(msg))
        }
        r => r,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RequestUndoError {
    #[error("Run not found")]
    NotFound,
    #[error("{}", .0.message())]
    Collision(adb::ConflictAlbum),
    #[error(transparent)]
    Other(#[from] AutotagError),
}
impl From<sqlx::Error> for RequestUndoError {
    fn from(e: sqlx::Error) -> Self {
        Self::Other(e.into())
    }
}

/// Request-time undo checks and claim. Reuses the job's refusal policy; the job
/// checks again after dequeue. A queue failure restores the prior run state.
pub async fn request_undo(
    ctx: &LibraryCtx,
    run_id: Uuid,
) -> std::result::Result<Uuid, RequestUndoError> {
    let run = adb::get_run(&ctx.pool, run_id)
        .await?
        .ok_or(RequestUndoError::NotFound)?;
    if !matches!(run.state.as_str(), "done" | "failed" | "undo_failed") {
        return Err(AutotagError::Refused("Only a finished run can be undone".into()).into());
    }
    mark_renamed_planned(ctx, &run).await?;
    let rows = jdb::undo_rows_desc(&ctx.pool, run.id).await?;
    let hooks = ApplyHooks::default();
    let cancel = CancellationToken::new();
    let job = Undo {
        ctx,
        run: &run,
        hooks: &hooks,
        cancel: &cancel,
        events: AutotagEvents::new(ctx.events.clone()),
        total: 0,
        done: 0,
    };
    let place = job.place(target_of(&run)?, !rows.is_empty()).await?;
    if let Some(c) = job.check(&rows, place.as_ref()).await? {
        return Err(RequestUndoError::Collision(c));
    }
    let mut tx = ctx.pool.begin().await?;
    if let Some(c) = job.check_identity(&mut tx, &rows).await? {
        return Err(RequestUndoError::Collision(c));
    }
    if !adb::start_undo_tx(&mut tx, run.id).await? {
        return Err(AutotagError::Refused("Only a finished run can be undone".into()).into());
    }
    let job = kima_jobs::queue::enqueue_tx(
        &mut tx,
        jobs::QUEUE,
        json!({"kind":"autotag-undo","runId":run.id}),
        jobs::MAX_ATTEMPTS,
    )
    .await?;
    tx.commit().await?;
    Ok(job)
}

/// Ends an `undoing` run: `undone` (album `proposed`) without `error`, else `undo_failed` with
/// it (album `failed` with it). The summary keeps the apply's counts and gains the journal's
/// `undone`/`undoFailed` counts and `identityRestored`, plus `extra`; the step marks go.
async fn finish(ctx: &LibraryCtx, run: &RunRow, error: Option<&str>, extra: Value) -> Result<()> {
    let rows = jdb::rows(&ctx.pool, run.id).await?;
    let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
    let current = adb::get_run(&ctx.pool, run.id)
        .await?
        .and_then(|r| r.summary)
        .unwrap_or_else(|| json!({}));
    let mut summary = match current {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    for k in MARKS {
        summary.remove(k);
    }
    if summary.get("organizeUndoError").is_some_and(Value::is_null) {
        summary.remove("organizeUndoError");
    }
    let restored = summary
        .get("identityRestored")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    summary.insert("undone".into(), json!(n("undone")));
    summary.insert("undoFailed".into(), json!(n("undo_failed")));
    summary.insert("identityRestored".into(), json!(restored));
    if let Value::Object(extra) = extra {
        summary.extend(extra);
    }
    let summary = Value::Object(summary);
    let (state, album) = match error {
        None => (RunState::Undone, AlbumState::Proposed),
        Some(_) => (RunState::UndoFailed, AlbumState::Failed),
    };
    adb::finish_run(&ctx.pool, run.id, state, &summary, error).await?;
    if let Err(e) = adb::set_state(&ctx.pool, run.album_id, album, error).await {
        info!(run = %run.id, "album state not set: {e}");
    }
    info!(run = %run.id, album = %run.album_id, state = state.as_str(), %summary,
          "autotag undo finished");
    Ok(())
}

/// Where the run's files are restored.
#[derive(Debug, Clone)]
enum Place {
    /// The library files (`files`), at `ctx.root`.
    Library(PathBuf),
    /// The copies of an unswitched copy run, at its target.
    Copy(PathBuf),
    /// The copies of a copy run the library was switched to: library files now.
    SwitchedCopy(PathBuf),
}

impl Place {
    fn root(&self) -> &Path {
        match self {
            Place::Library(p) | Place::Copy(p) | Place::SwitchedCopy(p) => p,
        }
    }

    /// Library files: located through their tracks and re-read as tag writes.
    fn in_library(&self) -> bool {
        !matches!(self, Place::Copy(_))
    }
}

/// What one row came to.
enum RowUndo {
    Restored,
    AlreadyRestored,
    CopyGone,
    Failed(String),
    /// The `stop_after_rename` hook: stop as a crash would.
    Crash,
}

/// How the identity step ended.
enum IdentityOutcome {
    Restored,
    Collision(String),
    AlbumGone,
}

struct Undo<'a> {
    ctx: &'a LibraryCtx,
    run: &'a RunRow,
    hooks: &'a ApplyHooks,
    cancel: &'a CancellationToken,
    events: AutotagEvents,
    total: usize,
    done: usize,
}

impl Undo<'_> {
    fn progress(&mut self, state: &str) {
        self.events.progress(
            AutotagJob::Undo,
            Some(self.run.album_id),
            Some(self.run.id),
            self.done,
            self.total,
            Some(state),
        );
    }

    fn finish_event(&mut self, state: &str) {
        self.events.finish(
            AutotagJob::Undo,
            Some(self.run.album_id),
            Some(self.run.id),
            self.done,
            self.total,
            Some(state),
        );
    }

    fn mark(&self, key: &str) -> bool {
        self.run
            .summary
            .as_ref()
            .and_then(|s| s.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn fail_point(&self, at: FailPoint) -> Result<()> {
        if self.hooks.fail_at == Some(at) {
            return Err(AutotagError::Io("forced failure (test hook)".to_string()));
        }
        Ok(())
    }

    async fn merge_summary(&self, fields: Value) -> Result<()> {
        let mut conn = self.ctx.pool.acquire().await?;
        adb::merge_run_summary(&mut conn, self.run.id, &fields).await?;
        Ok(())
    }

    async fn undo(&mut self) -> Result<()> {
        let target = target_of(self.run)?;
        // A file renamed before its row's commit is written (and restored below).
        if !self.mark("undoChecked") {
            mark_renamed_planned(self.ctx, self.run).await?;
        }
        let rows = jdb::undo_rows_desc(&self.ctx.pool, self.run.id).await?;
        let place = match self.place(target, !rows.is_empty()).await {
            // A resumed undo already touched things: it ends `undo_failed`, never back.
            Err(AutotagError::Refused(m)) if self.mark("undoChecked") => {
                finish(self.ctx, self.run, Some(&m), json!({})).await?;
                self.finish_event(RunState::UndoFailed.as_str());
                return Ok(());
            }
            r => r?,
        };
        if !self.mark("undoChecked") {
            if let Some(c) = self.check(&rows, place.as_ref()).await? {
                return Err(AutotagError::Refused(c.message()));
            }
            self.merge_summary(json!({"undoChecked": true})).await?;
        }

        // Step 1: the reorganize, so the files are back where the apply journalled them.
        let organize_error = if self.mark("organizerUndone") {
            self.run
                .summary
                .as_ref()
                .and_then(|s| s.get("organizeUndoError"))
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            self.progress(RunState::Undoing.as_str());
            let e = self.undo_organizer().await?;
            self.merge_summary(json!({"organizerUndone": true, "organizeUndoError": e}))
                .await?;
            e
        };

        // Step 2: the files.
        self.fail_point(FailPoint::BeforeWrites)?;
        if let Some(place) = &place {
            self.restore_rows(place, &rows).await?;
        }

        // Step 3: the identity, then the re-read.
        self.fail_point(FailPoint::BeforeIdentity)?;
        if let Some(h) = &self.hooks.before_identity {
            h();
        }
        let identity_error = if self.mark("identityRestored") {
            None
        } else {
            match self.identity().await? {
                IdentityOutcome::Restored => None,
                IdentityOutcome::Collision(m) => Some(m),
                IdentityOutcome::AlbumGone => {
                    info!(run = %self.run.id, "album gone during the undo");
                    return Ok(());
                }
            }
        };
        self.fail_point(FailPoint::AfterIdentity)?;
        if identity_error.is_none() {
            let library_files = self.library_files(target).await?;
            self.reread(library_files).await?;
        }

        // Step 4.
        let failed_rows = jdb::rows(&self.ctx.pool, self.run.id)
            .await?
            .iter()
            .any(|r| r.state == "undo_failed");
        let error = identity_error
            .or_else(|| failed_rows.then(|| UNDO_INCOMPLETE.to_string()))
            .or_else(|| organize_error.clone());
        let extra = match &organize_error {
            Some(e) => json!({"organizeUndoError": e}),
            None => json!({}),
        };
        finish(self.ctx, self.run, error.as_deref(), extra).await?;
        let state = match error {
            None => RunState::Undone,
            Some(_) => RunState::UndoFailed,
        };
        self.finish_event(state.as_str());
        Ok(())
    }

    /// Whether the run's journalled files are library files (re-read as tag writes): the
    /// `files` target, or a `copy` whose copy run the library was switched to and still is.
    /// From the run's target, not from what is left to restore (a resume may have none).
    async fn library_files(&self, target: Target) -> Result<bool> {
        Ok(match target {
            Target::Files => true,
            Target::Db => false,
            Target::Copy => match self.run.copy_run_id {
                Some(id) => odb::get_run(&self.ctx.pool, id).await?.is_some_and(|c| {
                    c.switched_at.is_some() && self.ctx.root.get() == Path::new(&c.target_root)
                }),
                None => false,
            },
        })
    }

    /// Where the files are restored, `None` for nothing to restore (`db`, or no row left).
    /// A `copy` run needs its copy run: unswitched (its target), or switched and still the
    /// library (the library root); else [`COPY_GONE`].
    async fn place(&self, target: Target, has_rows: bool) -> Result<Option<Place>> {
        if !has_rows {
            return Ok(None);
        }
        match target {
            Target::Db => Ok(None),
            Target::Files => Ok(Some(Place::Library(self.ctx.root.get()))),
            Target::Copy => {
                let gone = || AutotagError::Refused(COPY_GONE.to_string());
                let id = self.run.copy_run_id.ok_or_else(gone)?;
                let copy = odb::get_run(&self.ctx.pool, id).await?.ok_or_else(gone)?;
                let target_root = PathBuf::from(&copy.target_root);
                if copy.switched_at.is_some() {
                    if self.ctx.root.get() != target_root {
                        return Err(gone());
                    }
                    Ok(Some(Place::SwitchedCopy(target_root)))
                } else {
                    Ok(Some(Place::Copy(target_root)))
                }
            }
        }
    }

    async fn check_identity(
        &self,
        conn: &mut sqlx::PgConnection,
        rows: &[JournalRow],
    ) -> Result<Option<adb::ConflictAlbum>> {
        let (run, album) = (self.run, self.run.album_id);
        if adb::lock_album_tracks(conn, album).await?.is_none() {
            return Err(AutotagError::Refused(IdentityError::AlbumGone.to_string()));
        }
        if adb::other_active_run(conn, album, run.id).await?.is_some() {
            return Err(AutotagError::Refused(ALBUM_BUSY.to_string()));
        }
        if adb::newer_undoable_run(conn, album, run.id)
            .await?
            .is_some()
        {
            return Err(AutotagError::Refused(UNDO_NEWER_FIRST.to_string()));
        }
        let from = run
            .summary
            .as_ref()
            .and_then(|s| s.get("undoFrom"))
            .and_then(Value::as_str);
        if from.unwrap_or(&run.state) == "failed" && run.album_before.is_none() && rows.is_empty() {
            return Err(AutotagError::Refused(NOTHING_TO_UNDO.to_string()));
        }
        Ok(adb::undo_collision(conn, run).await?)
    }

    /// The refusals before anything is touched (see the module doc).
    async fn check(
        &self,
        rows: &[JournalRow],
        place: Option<&Place>,
    ) -> Result<Option<adb::ConflictAlbum>> {
        let (pool, run) = (&self.ctx.pool, self.run);
        let mut tx = pool.begin().await?;
        if let Some(c) = self.check_identity(&mut tx, rows).await? {
            return Ok(Some(c));
        }
        tx.commit().await?;

        if let Some(id) = run.organizer_run_id
            && let Some(org) = odb::get_run(pool, id).await?
            && org.state == "applying"
            && odb::job_pending(pool, jobs::QUEUE, id).await?
        {
            return Err(AutotagError::Refused(REORGANIZE_IN_PROGRESS.to_string()));
        }
        if let Some(place) = place {
            let root = place.root().to_path_buf();
            let ok = tokio::task::spawn_blocking(move || writable(&root))
                .await
                .unwrap_or(false);
            if !ok {
                return Err(AutotagError::Refused(UNDO_READ_ONLY.to_string()));
            }
        }
        Ok(None)
    }

    /// Step 1. Answers why the reorganize could not be fully undone, if it could not; a
    /// refusal by the organizer (nothing moved) refuses the undo.
    async fn undo_organizer(&self) -> Result<Option<String>> {
        let pool = &self.ctx.pool;
        let Some(id) = self.run.organizer_run_id else {
            return Ok(None);
        };
        let load = || async { Ok::<_, AutotagError>(odb::get_run(pool, id).await?) };
        let Some(mut org) = load().await? else {
            return Ok(None);
        };
        let root = self.ctx.root.get();
        if org.state == "applying" {
            // An interrupted apply that no job drives (checked before): finish the move, so
            // its journal is whole, then undo it.
            match organize::preview_then_apply_with(self.ctx, id, &self.hooks.organize, self.cancel)
                .await
            {
                Ok(()) => {}
                Err(OrganizeError::Interrupted) => return Err(AutotagError::Interrupted),
                Err(OrganizeError::Db(e)) => return Err(AutotagError::Db(e)),
                // The organizer run is `failed` now; what it moved is undone below.
                Err(e) => warn!(run = %self.run.id, organizer_run = %id,
                                "the interrupted reorganize did not complete: {e}"),
            }
            let Some(o) = load().await? else {
                return Ok(None);
            };
            org = o;
        }
        match org.state.as_str() {
            "done" | "failed" => {
                if org.state == "failed" && !moved_rows(pool, id).await? {
                    return Ok(None);
                }
                if !odb::transition(pool, id, &["done", "failed"], "undoing").await? {
                    warn!(organizer_run = %id, "the reorganize run changed state; not undone");
                    return Ok(Some(format!(
                        "the reorganize run is {}; see Settings > Organize",
                        load().await?.map(|o| o.state).unwrap_or_default()
                    )));
                }
            }
            "undoing" => {}
            // Nothing moved (or it is undone already).
            _ => return Ok(None),
        }
        info!(run = %self.run.id, organizer_run = %id, "undoing the reorganize");
        match organize::run_action_cancellable(
            self.ctx,
            id,
            Action::Undo,
            &self.hooks.organize,
            self.cancel,
        )
        .await
        {
            Ok(()) => {}
            Err(OrganizeError::Interrupted) => return Err(AutotagError::Interrupted),
            Err(OrganizeError::Db(e)) => return Err(AutotagError::Db(e)),
            Err(OrganizeError::Refused(m)) => {
                // Put back to `done`/`failed` by the organizer; nothing moved.
                info!(organizer_run = %id, reason = %m, "the reorganize undo was refused");
                return Err(AutotagError::Refused(REORGANIZE_ROOT_CHANGED.to_string()));
            }
            Err(e) => {
                let m = e.to_string();
                warn!(organizer_run = %id, error = %m, "the reorganize undo failed");
                odb::fail_run(pool, id, &m).await?;
                return Ok(Some(scrub(&root, &m)));
            }
        }
        match load().await? {
            Some(o) if o.state != "undone" => Ok(Some(scrub(
                &root,
                &o.error
                    .unwrap_or_else(|| format!("the reorganize run is {}", o.state)),
            ))),
            _ => Ok(None),
        }
    }

    /// Step 2: each row, newest write first, checkpointing between files.
    async fn restore_rows(&mut self, place: &Place, rows: &[JournalRow]) -> Result<()> {
        self.total = rows.len();
        self.done = 0;
        self.progress(RunState::Undoing.as_str());
        for (handled, row) in rows.iter().enumerate() {
            if self.cancel.is_cancelled() || self.hooks.stop_after_files == Some(handled) {
                return Err(AutotagError::Interrupted);
            }
            let mut candidates = Vec::new();
            if place.in_library()
                && let Some(track) = row.track_id
                && let Some(p) = odb::track_path(&self.ctx.pool, track).await?
            {
                candidates.push(p);
            }
            if !candidates.contains(&row.path) {
                candidates.push(row.path.clone());
            }
            let (root, r) = (place.root().to_path_buf(), row.clone());
            let copy = matches!(place, Place::Copy(_));
            let (ignore, hooks) = (self.ctx.ignore.clone(), self.hooks.write);
            let outcome = tokio::task::spawn_blocking(move || {
                undo_row(&root, &r, &candidates, copy, &ignore, &hooks)
            })
            .await
            .unwrap_or_else(|e| {
                warn!("the tag writer panicked: {e}");
                RowUndo::Failed("the tag writer failed unexpectedly; nothing written".to_string())
            });
            let pool = &self.ctx.pool;
            match outcome {
                RowUndo::Restored => jdb::set_row_state(pool, row.id, "undone", None).await?,
                RowUndo::AlreadyRestored => {
                    jdb::set_row_state(pool, row.id, "undone", Some(ALREADY_RESTORED)).await?
                }
                RowUndo::CopyGone => {
                    jdb::set_row_state(pool, row.id, "undone", Some(COPY_TAG_GONE)).await?
                }
                RowUndo::Failed(note) => {
                    let note = scrub(place.root(), &note);
                    info!(run = %self.run.id, row = row.id, %note, "autotag undo row failed");
                    jdb::set_row_state(pool, row.id, "undo_failed", Some(&note)).await?
                }
                RowUndo::Crash => return Err(AutotagError::Interrupted),
            }
            self.done += 1;
            self.progress(RunState::Undoing.as_str());
        }
        Ok(())
    }

    /// Step 3's transaction, retried once on a deadlock.
    async fn identity(&self) -> Result<IdentityOutcome> {
        let (pool, run) = (&self.ctx.pool, self.run);
        let restored = json!({"identityRestored": true});
        let restored = &restored;
        let outcome = kima_db::retry_once_on_conflict(|| async move {
            let mut tx = pool.begin().await?;
            if run.album_before.is_some() {
                match adb::undo_identity(&mut tx, run).await {
                    Ok(()) => {}
                    Err(IdentityError::Db(e)) => return Err(e),
                    Err(IdentityError::Collision(c)) => {
                        return Ok(IdentityOutcome::Collision(c.message()));
                    }
                    Err(IdentityError::AlbumGone) => return Ok(IdentityOutcome::AlbumGone),
                }
            } else {
                // The identity was never committed: only the run's overrides (if any) go.
                adb::delete_overrides(&mut tx, run.id).await?;
            }
            adb::merge_run_summary(&mut tx, run.id, restored).await?;
            tx.commit().await?;
            Ok(IdentityOutcome::Restored)
        })
        .await?;
        if let IdentityOutcome::Restored = outcome {
            info!(run = %run.id, "album identity restored");
        }
        Ok(outcome)
    }

    /// Prunes what the undo left empty, then re-reads the album's library files: the restored
    /// ones as tag writes, the others as a scan; files whose restore failed are left alone.
    async fn reread(&self, library_files: bool) -> Result<()> {
        let pool = &self.ctx.pool;
        kima_db::library::prune_empty(pool).await?;
        let rows = jdb::rows(pool, self.run.id).await?;
        let track_paths = |state: &'static str| {
            let rows = &rows;
            async move {
                let mut out = BTreeSet::new();
                for r in rows.iter().filter(|r| r.state == state) {
                    if let Some(t) = r.track_id
                        && let Some(p) = odb::track_path(pool, t).await?
                    {
                        out.insert(p);
                    }
                }
                Ok::<_, AutotagError>(out)
            }
        };
        let (tag_write, skip) = if library_files {
            (
                track_paths("undone").await?,
                track_paths("undo_failed").await?,
            )
        } else {
            (BTreeSet::new(), BTreeSet::new())
        };
        let album_paths: BTreeSet<String> = adb::local_album(pool, self.run.album_id)
            .await?
            .map(|a| a.tracks.into_iter().map(|t| t.path).collect())
            .unwrap_or_default();
        let scan_paths: Vec<String> = album_paths
            .iter()
            .filter(|p| !tag_write.contains(*p) && !skip.contains(*p))
            .cloned()
            .collect();
        let tag_write: Vec<String> = tag_write.into_iter().collect();
        self.reread_paths(&tag_write, ReadCause::TagWrite).await?;
        self.reread_paths(&scan_paths, ReadCause::Scan).await
    }

    async fn reread_paths(&self, paths: &[String], cause: ReadCause) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let report = scan::reread_paths(self.ctx, paths, cause)
            .await
            .map_err(|e| match e {
                scan::ScanError::Db(e) => AutotagError::Db(e),
                e => io(e),
            })?;
        if !report.failed.is_empty() {
            warn!(run = %self.run.id, failed = report.failed.len(), "files not re-read after the undo");
        }
        Ok(())
    }
}

/// Whether a `failed` organizer run moved anything (a row `done`, `staged` or touched by an
/// earlier undo).
async fn moved_rows(pool: &kima_db::PgPool, run: Uuid) -> Result<bool> {
    Ok(odb::all_rows(pool, run).await?.iter().any(|r| {
        matches!(
            r.state.as_str(),
            "done" | "staged" | "undo_staged" | "undo_failed"
        )
    }))
}

/// Blocking: one row of step 2 (see the module doc). `candidates`: where the file may be,
/// relative to `root`, in order; `copy`: an unswitched copy, whose missing file is nothing to
/// restore.
fn undo_row(
    root: &Path,
    row: &JournalRow,
    candidates: &[String],
    copy: bool,
    ignore: &crate::watch::IgnoreSet,
    hooks: &WriteHooks,
) -> RowUndo {
    let old = match tags_of(&row.old_tags) {
        Ok(o) => o,
        Err(e) => return RowUndo::Failed(e.to_string()),
    };
    let Some(after) = row.after_sha.as_deref() else {
        return RowUndo::Failed(UNDO_CHANGED.to_string());
    };
    // A crash in an earlier write of this row (apply or undo) left its hidden copy.
    for c in candidates {
        if let Ok(abs) = join_rel(root, c)
            && let Ok(leftover) = copy_path(&abs, row.id)
            && fs::symlink_metadata(&leftover).is_ok()
            && let Err(e) = fs::remove_file(&leftover)
        {
            warn!(row = row.id, "cannot delete a leftover tag copy: {e}");
        }
    }
    let (mut found, mut symlink) = (Vec::new(), false);
    for c in candidates {
        match tagwrite::refuse_symlink(root, c) {
            Ok(()) => {}
            Err(TagWriteError::Symlink) => {
                symlink = true;
                continue;
            }
            Err(_) => continue,
        }
        let Ok(abs) = join_rel(root, c) else {
            continue;
        };
        if fs::symlink_metadata(&abs).is_ok_and(|m| m.is_file()) {
            found.push(abs);
        }
    }
    for abs in &found {
        if sha256_file(abs).is_ok_and(|sha| sha == after) {
            return write_back(abs, row, &old, after, ignore, hooks);
        }
    }
    for abs in &found {
        if carries(abs, row, &old) {
            return RowUndo::AlreadyRestored;
        }
    }
    if symlink {
        return RowUndo::Failed(SYMLINK_NOTE.to_string());
    }
    if found.is_empty() && copy {
        return RowUndo::CopyGone;
    }
    RowUndo::Failed(UNDO_CHANGED.to_string())
}

/// The safe write of `old` over the file at `abs` (which hashed to `after`), then the re-read.
fn write_back(
    abs: &Path,
    row: &JournalRow,
    old: &NativeTags,
    after: &[u8],
    ignore: &crate::watch::IgnoreSet,
    hooks: &WriteHooks,
) -> RowUndo {
    let container = match container_of(abs) {
        Ok(c) => c,
        Err(e) => return RowUndo::Failed(e.to_string()),
    };
    // The version the file had before the apply (NULL: no ID3v2 tag, written as v2.4).
    let id3 = row.id3_version.as_deref().and_then(Id3Version::parse);
    match tagwrite::safe_write_expecting(
        abs,
        row.id,
        container,
        id3,
        old,
        ignore,
        hooks,
        Some(after),
    ) {
        Ok(_) => {}
        Err(_) if hooks.stop_after_rename => return RowUndo::Crash,
        Err(TagWriteError::ChangedOutside) => return RowUndo::Failed(UNDO_CHANGED.to_string()),
        Err(e) => return RowUndo::Failed(e.to_string()),
    }
    let keys: Vec<String> = old.keys().cloned().collect();
    match read_native(abs, container, &keys) {
        Ok(got) if &got == old => RowUndo::Restored,
        _ => RowUndo::Failed(NOT_READ_BACK.to_string()),
    }
}

/// Whether the file's touched keys hold the old values and its audio is the journalled audio.
fn carries(abs: &Path, row: &JournalRow, old: &NativeTags) -> bool {
    let check = || -> std::result::Result<bool, TagWriteError> {
        let keys: Vec<String> = old.keys().cloned().collect();
        let container = container_of(abs)?;
        if &read_native(abs, container, &keys)? != old {
            return Ok(false);
        }
        Ok(row.audio_md5.as_deref() == Some(audio_md5(abs)?.as_slice()))
    };
    check().unwrap_or(false)
}
