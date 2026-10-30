//! The apply job (spec §19.9, on the concurrency-1 `library` queue): writes a run's §19.6 values
//! into the album's files (`files`), into their organized copies (`copy`) or as tag overrides
//! (`db`), then updates the album row in place (§19.7), re-reads the files and, with
//! `reorganize`, re-files the album.
//!
//! Steps (`files`; `copy` the same on the copies):
//! 1. pre-check, before any file is touched: the target is available, the candidate belongs to
//!    the album, no other run of the album is working, no applied match stands (another run's
//!    overrides, a failed run that needs undo, or the album row's `identity_source='autotag'`;
//!    else [`APPLIED_MATCH_EXISTS`]), the album's present tracks are exactly the run's tracks
//!    (else [`TRACKS_CHANGED`]), every file is writable by kima-rs ([`album_refusal`]) and the
//!    new identity collides with no other album;
//! 2. journal: one `planned` row per file, committed before any file is touched;
//! 3. per file: the file must still hash to `before_sha` (else the row fails
//!    [`CHANGED_OUTSIDE`] and the others go on), then the safe write and, in one transaction, the
//!    row `written`; the cancel token is checked between files;
//! 4. one transaction: the album identity (`apply_identity`) and the tag overrides (all §19.6
//!    values for `db` and `copy`; the applied year for every file the `files` target wrote, so a
//!    reissue's tracks, whose files say the release date, keep deriving the applied year); then
//!    `prune_empty` and the re-read;
//! 5. `reorganize` (`files` only): the run goes `organizing`, a scoped move organizer run of the
//!    album (`origin='autotag'`) is created with its id stored on the run in one transaction,
//!    then previewed and applied with no human step
//!    ([`organize::apply::preview_then_apply`](crate::organize::apply::preview_then_apply));
//! 6. the album `applied`, the run `done` with `{written, skipped, failed, organized}`.
//!
//! **The `copy` target** writes only the copies (the `done` rows) of the newest finished,
//! unswitched whole-library copy run (`copy_run_id`, recorded with the journal): journal rows are
//! `root='copy'` with paths relative to that run's target, and nothing under the library root
//! is ever opened for writing. The library files keep their tags; the overrides of the tracks
//! whose copy holds the new tags show the match until the organizer's `switch-root` makes the
//! copies the library and deletes them.
//!
//! **A failed reorganize** never hides or rolls back the tags: the files, identity and
//! re-read are done by then, so the run still ends `done`, with what moved as `organized` and
//! the reason as `organizeError` in its summary (spec §19.9 step 5 has no failure path of its
//! own; step 6 follows step 4 whatever the re-filing does). The organizer run stays as the
//! organizer left it (`previewed` for a refused plan, `failed` after a failed apply), so it can
//! be reviewed in Settings > Organize. A database error is retried; when the organizer's own
//! apply raised it, that run is already `failed` and the retry ends the run `done` the same way.
//!
//! **Crash recovery.** The run stays `writing` until step 5. A job that finds journal rows
//! resumes: it deletes leftover `.kima-tag-<id>.*` copies, redoes a `planned` row whose file is
//! still at `before_sha`, marks `written` one whose touched keys already read back the new values
//! with the planned audio, and fails any other with [`CHANGED_OUTSIDE`]. A run is never applied
//! twice: a finished run's job does nothing, and `apply_identity` answers a run that already
//! has `album_before` without writing (crash safety only). A run found `organizing` goes
//! straight to step 5 and resumes its organizer run (never a second one).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kima_db::autotag::journal::{self as jdb, JournalRow, NewJournalRow};
use kima_db::autotag::{
    self as adb, AlbumState, Applied, ArtistStep, ConflictAlbum, IdentityError, IdentityKeys,
    RunRow, RunState, Target,
};
use kima_jobs::worker::JobError;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::capability::{album_refusal, copy_refusal, usable_copy_run};
use super::events::{AutotagEvents, AutotagJob};
use super::journal::{self, RowCheck};
use super::{
    APPLIED_MATCH_EXISTS, CHANGED_OUTSIDE, COPY_FIRST, FILES_READ_ONLY, REORGANIZE_FILES_ONLY,
    TRACKS_CHANGED,
};
use crate::jobs;
use crate::names;
use crate::organize::apply::{self as organize, OrganizeError, SYMLINK_NOTE, writable};
use crate::scan::{self, LibraryCtx, ReadCause};
use crate::tags::TagOverrides;
use crate::tagwrite::{self, Id3Version, TagValues, TagWriteError, WriteHooks};
use kima_db::organizer::{self as odb, Run as OrganizerRun};

/// Another apply or undo of the album is still working.
pub const ALBUM_BUSY: &str = "Another apply or undo of this album is in progress";
/// The run's candidate belongs to another album.
pub const WRONG_ALBUM: &str = "The chosen match belongs to another album";
/// Every row of a `files` apply failed: the identity is left as it was.
pub const NOTHING_WRITTEN: &str = "No file of the album could be written; see the journal";
/// A `planned` row already holding the new values.
const ALREADY_TAGGED: &str = "already tagged";

/// Test hooks; production passes [`ApplyHooks::default`].
#[derive(Default, Clone)]
pub struct ApplyHooks {
    /// Passed to every safe write. With `stop_after_rename` the job stops right after the first
    /// rename, as a crash before the row's commit would.
    pub write: WriteHooks,
    /// Stop (as an interruption) once this many files were handled in this call.
    pub stop_after_files: Option<usize>,
    /// Called once, right after the journal commit and before the first write.
    pub after_journal: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Fail there as a database or I/O error would.
    pub fail_at: Option<FailPoint>,
    /// Passed to the reorganize's organizer apply (step 5).
    pub organize: organize::Hooks,
}

/// Where [`ApplyHooks::fail_at`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    /// Right before the files are written (a fresh or a resumed run).
    BeforeWrites,
    /// After the files, right before the identity transaction.
    BeforeIdentity,
    /// Right after the identity transaction committed.
    AfterIdentity,
    /// An `organizing` run, right before its organizer run is made or resumed.
    BeforeOrganize,
}

#[derive(Debug, thiserror::Error)]
pub enum AutotagError {
    /// Refused before (or instead of) touching the album: the run is `failed` with this message
    /// and the album back to `proposed`. Not retried.
    #[error("{0}")]
    Refused(String),
    /// The cancel token fired or a test hook stopped the job; the run stays resumable.
    #[error("autotag apply interrupted")]
    Interrupted,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Io(String),
}

type Result<T> = std::result::Result<T, AutotagError>;

/// Queues the apply job of `run_id` on the `library` queue.
pub async fn enqueue_apply(pool: &kima_db::PgPool, run_id: Uuid) -> sqlx::Result<Uuid> {
    kima_jobs::queue::enqueue(
        pool,
        jobs::QUEUE,
        json!({ "kind": "autotag-apply", "runId": run_id }),
        jobs::MAX_ATTEMPTS,
    )
    .await
}

/// Queues the undo job of `run_id` on the `library` queue (the job is Task 18's).
pub async fn enqueue_undo(pool: &kima_db::PgPool, run_id: Uuid) -> sqlx::Result<Uuid> {
    kima_jobs::queue::enqueue(
        pool,
        jobs::QUEUE,
        json!({ "kind": "autotag-undo", "runId": run_id }),
        jobs::MAX_ATTEMPTS,
    )
    .await
}

/// Startup (after the queue's `requeue_running`): re-queues every run a job still owes work to
/// (`writing`, `organizing`, `undoing`, and a `queued` one whose job is gone) that has no autotag
/// job waiting or running, so no album is left busy. The job itself applies the crash-recovery
/// rules. Returns how many were queued.
pub async fn resume_interrupted(pool: &kima_db::PgPool) -> sqlx::Result<usize> {
    let mut queued = 0;
    for (id, state) in adb::unfinished_runs(pool).await? {
        if adb::job_pending(pool, jobs::QUEUE, id).await? {
            continue;
        }
        info!(run = %id, %state, "resuming an interrupted autotag run");
        if state == RunState::Undoing.as_str() {
            enqueue_undo(pool, id).await?;
        } else {
            enqueue_apply(pool, id).await?;
        }
        queued += 1;
    }
    Ok(queued)
}

/// The `autotag-apply` job body: [`run_apply`] without hooks. A refusal (already recorded on
/// the run) fails the job for good; anything else is retried, resuming from the journal.
/// `final_attempt`: the queue will not retry this job again, so a database or I/O error
/// settles the run as a refusal would ([`settle_failed`]) instead of leaving it `writing`
/// (an interruption, e.g. a shutdown, gets its attempt back and is left as it is). Answers
/// whether the library changed.
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
    match run_apply(ctx, run_id, hooks, cancel).await {
        Ok(()) => Ok(true),
        Err(AutotagError::Refused(m)) => Err(JobError::Permanent(m)),
        Err(AutotagError::Interrupted) => {
            Err(JobError::Retry(AutotagError::Interrupted.to_string()))
        }
        Err(e) if final_attempt => {
            let msg = e.to_string();
            warn!(run = %run_id, error = %msg, "autotag apply failed on its final attempt");
            match adb::get_run(&ctx.pool, run_id).await {
                Ok(Some(run)) if run.state == RunState::Writing.as_str() => {
                    if let Err(e) = settle_failed(ctx, &run, &msg).await {
                        warn!(run = %run_id, "cannot settle the failed run: {e}");
                    }
                }
                // The tags, identity and re-read are done: only the re-filing failed. The run
                // ends `done`, the reason in its summary (see the module doc).
                Ok(Some(run)) if run.state == RunState::Organizing.as_str() => {
                    match finish_organized(ctx, &run, Some(&msg)).await {
                        Ok(()) => return Ok(true),
                        Err(e) => warn!(run = %run_id, "cannot finish the run: {e}"),
                    }
                }
                _ => {}
            }
            Err(JobError::Permanent(msg))
        }
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

/// Ends a run that will not complete: `failed` with `msg`, and the album's search state says
/// what is left.
///
/// - The run left something to undo: files it wrote (`written` journal rows) and/or a committed
///   identity transaction (`album_before` set on the re-read run: the album row, artist step and
///   overrides are in the database). Nothing more is changed: in particular nothing is re-read,
///   since the files' new tags would move the tracks or merge them into an album that collides.
///   The summary says `needsUndo` (and `identityApplied`), the album is `failed` with the message:
///   it needs attention, and no new apply is accepted until this run is undone (Task 18 undoes a
///   `failed` run: `undo_identity` and the overrides when `album_before` is set, the written rows
///   if any).
/// - Nothing to undo: while another `failed` run of the album still needs undo, the album stays
///   `failed` with that run's message; it is left alone while another run of it is working;
///   else it goes back to `proposed`, or `applied` when its newest other run that did not fail
///   is `done` (that match stands).
///
/// Answers the album state set, if any.
pub async fn settle_failed(
    ctx: &LibraryCtx,
    run: &RunRow,
    msg: &str,
) -> Result<Option<AlbumState>> {
    let rows = jdb::rows(&ctx.pool, run.id).await?;
    let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
    let written = n("written");
    let identity_applied = adb::get_run(&ctx.pool, run.id)
        .await?
        .is_some_and(|r| r.album_before.is_some());
    let needs_undo = written > 0 || identity_applied;
    let summary = if needs_undo {
        json!({"written": written, "skipped": n("skipped"), "failed": n("failed"),
               "organized": 0, "rolledBack": false, "needsUndo": true,
               "identityApplied": identity_applied})
    } else {
        json!({"written": 0, "skipped": n("skipped"), "failed": n("failed"), "organized": 0})
    };
    adb::finish_run(&ctx.pool, run.id, RunState::Failed, &summary, Some(msg)).await?;
    let (state, error): (Option<AlbumState>, Option<String>) = if needs_undo {
        (Some(AlbumState::Failed), Some(msg.to_string()))
    } else {
        let mut conn = ctx.pool.acquire().await?;
        let other = adb::needs_undo_run(&mut conn, run.album_id, run.id).await?;
        drop(conn);
        match other {
            Some((_, error)) => (Some(AlbumState::Failed), error),
            None => match adb::latest_other_run_state(&ctx.pool, run.album_id, run.id)
                .await?
                .as_deref()
            {
                Some("queued" | "writing" | "organizing" | "undoing") => (None, None),
                Some("done") => (Some(AlbumState::Applied), None),
                _ => (Some(AlbumState::Proposed), None),
            },
        }
    };
    if let Some(state) = state
        && let Err(e) = adb::set_state(&ctx.pool, run.album_id, state, error.as_deref()).await
    {
        // The album row went away (and the run with it, by cascade): nothing left to mark.
        info!(run = %run.id, "album state not set: {e}");
    }
    Ok(state)
}

/// `{written, skipped, failed, organized: 0}` from the run's journal (the summary of a `files`
/// or `copy` run; only those reach step 5).
async fn journal_summary(ctx: &LibraryCtx, run: Uuid) -> Result<Value> {
    let rows = jdb::rows(&ctx.pool, run).await?;
    let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
    Ok(
        json!({"written": n("written"), "skipped": n("skipped"), "failed": n("failed"),
              "organized": 0}),
    )
}

/// Ends an `organizing` run (step 6): the album `applied`, the run `done` with the journal's
/// counts, `organized` = the files its organizer run moved, and `organizeError` when the
/// re-filing did not complete.
async fn finish_organized(
    ctx: &LibraryCtx,
    run: &RunRow,
    organize_error: Option<&str>,
) -> Result<()> {
    let mut summary = journal_summary(ctx, run.id).await?;
    let organizer_run = adb::get_run(&ctx.pool, run.id)
        .await?
        .and_then(|r| r.organizer_run_id);
    if let Some(id) = organizer_run {
        summary["organized"] = json!(odb::done_count(&ctx.pool, id).await?);
    }
    if let Some(e) = organize_error {
        // Users see it: files relative to the library (the organizer's errors may quote the
        // absolute paths).
        summary["organizeError"] = json!(journal::scrub(&ctx.root.get(), e));
    }
    adb::mark_album_applied(&ctx.pool, run.album_id).await?;
    adb::finish_run(&ctx.pool, run.id, RunState::Done, &summary, None).await?;
    info!(run = %run.id, album = %run.album_id, %summary, "autotag apply finished");
    Ok(())
}

/// Runs (or resumes) the apply of `run_id`; see the module doc. A `queued` run moves to
/// `writing`; a run in any state but `writing`/`organizing` is left alone (a finished run is
/// never applied again).
pub async fn run_apply(
    ctx: &LibraryCtx,
    run_id: Uuid,
    hooks: &ApplyHooks,
    cancel: &CancellationToken,
) -> Result<()> {
    let Some(run) = adb::get_run(&ctx.pool, run_id).await? else {
        info!(run = %run_id, "autotag run gone; nothing to apply");
        return Ok(());
    };
    match run.state.as_str() {
        "queued" => {
            let moved =
                adb::transition_run(&ctx.pool, run.id, &[RunState::Queued], RunState::Writing)
                    .await?;
            if !moved {
                return Ok(());
            }
        }
        "writing" | "organizing" => {}
        other => {
            info!(run = %run.id, state = other, "autotag run not applying; nothing to do");
            return Ok(());
        }
    }
    let mut job = Job {
        ctx,
        run: &run,
        hooks,
        cancel,
        events: AutotagEvents::new(ctx.events.clone()),
        total: 0,
        done: 0,
    };
    match job.apply().await {
        Err(AutotagError::Refused(msg)) => {
            warn!(run = %run.id, reason = %msg, "autotag apply refused");
            job.refuse(&msg).await?;
            Err(AutotagError::Refused(msg))
        }
        r => r,
    }
}

/// What step 1 found.
struct Precheck {
    /// Track id → library path, for the run's tracks.
    paths: HashMap<Uuid, String>,
    /// `copy`: the copy run written into, and each track's copy path (relative to its target).
    copy: Option<(OrganizerRun, HashMap<Uuid, String>)>,
}

/// The §19.6 values of every applied track as tag overrides (`db` and `copy`).
fn db_overrides(applied: &Applied) -> Result<Vec<(Uuid, Value)>> {
    applied
        .tracks
        .iter()
        .map(|t| {
            let o = TagOverrides::from_applied(&applied.album, t);
            Ok((t.track_id, serde_json::to_value(o).map_err(io)?))
        })
        .collect()
}

/// How the identity transaction ended.
enum IdentityOutcome {
    Applied(ArtistStep),
    Collision(ConflictAlbum),
    AlbumGone,
}

/// What one `planned` row came to.
enum RowOutcome {
    Written(Vec<u8>),
    Skipped(&'static str),
    Failed(String),
    /// The `stop_after_rename` hook: stop as a crash would.
    Crash,
}

struct Job<'a> {
    ctx: &'a LibraryCtx,
    run: &'a RunRow,
    hooks: &'a ApplyHooks,
    cancel: &'a CancellationToken,
    events: AutotagEvents,
    total: usize,
    done: usize,
}

fn io(e: impl std::fmt::Display) -> AutotagError {
    AutotagError::Io(e.to_string())
}

fn target_of(run: &RunRow) -> Result<Target> {
    match run.target.as_str() {
        "files" => Ok(Target::Files),
        "copy" => Ok(Target::Copy),
        "db" => Ok(Target::Db),
        other => Err(AutotagError::Refused(format!(
            "unknown apply target {other}"
        ))),
    }
}

impl Job<'_> {
    fn progress(&mut self, state: &str) {
        self.events.progress(
            AutotagJob::Apply,
            Some(self.run.album_id),
            Some(self.run.id),
            self.done,
            self.total,
            Some(state),
        );
    }

    fn finish_event(&mut self, state: &str) {
        self.events.finish(
            AutotagJob::Apply,
            Some(self.run.album_id),
            Some(self.run.id),
            self.done,
            self.total,
            Some(state),
        );
    }

    async fn apply(&mut self) -> Result<()> {
        let applied: Applied = self.run.applied().map_err(|e| {
            AutotagError::Refused(format!("the run's stored values cannot be read: {e}"))
        })?;
        let target = target_of(self.run)?;
        // Reorganizing moves library files: never with the copies or the database alone (the
        // request refuses it too; this is the job's own guard).
        if self.run.reorganize && target != Target::Files {
            return Err(AutotagError::Refused(REORGANIZE_FILES_ONLY.to_string()));
        }
        if self.run.state == RunState::Organizing.as_str() {
            // Steps 1-4 are done (the files may already have moved): only step 5 is left.
            return self.organize_and_finish().await;
        }
        let keys = IdentityKeys {
            artist_key: names::key(&applied.album.artist),
            title_key: names::key(&applied.album.title),
        };
        let mut rows = jdb::rows(&self.ctx.pool, self.run.id).await?;
        let resumed = !rows.is_empty();
        // Where the files are written: the library, or the copy the journal was planned in.
        let mut root = match target {
            Target::Copy if resumed => self.resumed_copy_root().await?,
            _ => self.ctx.root.get(),
        };
        let read_only = match target {
            Target::Copy => COPY_FIRST,
            _ => FILES_READ_ONLY,
        };
        // Checked whenever a file is still to be written: a resumed run whose files are all
        // done goes on to the identity step whatever the mount says now.
        let to_write = !resumed || rows.iter().any(|r| r.state == "planned");
        if target == Target::Files && !resumed && !self.files_writable(&root).await {
            return Err(AutotagError::Refused(FILES_READ_ONLY.to_string()));
        }
        if target != Target::Db && resumed && to_write && !self.files_writable(&root).await {
            // A resumed run already wrote some files: read-only is per file. Each file left is
            // checked (reading only) by the crash rules: one already holding the new tags (a
            // crash after its rename) is `written`, so it is undone with the others; one still
            // as planned fails read-only; anything else changed outside kima-rs. The run goes on
            // with what was written (refused below when nothing was).
            for r in rows.iter().filter(|r| r.state == "planned") {
                let (root, row) = (root.clone(), r.clone());
                let check = blocking(move || Ok(journal::check_row(&root, &row, true))).await?;
                match check {
                    RowCheck::AlreadyWritten(sha) => {
                        let mut tx = self.ctx.pool.begin().await?;
                        jdb::mark_written(&mut tx, r.id, &sha).await?;
                        tx.commit().await?;
                    }
                    RowCheck::Write => {
                        jdb::set_row_state(&self.ctx.pool, r.id, "failed", Some(read_only)).await?
                    }
                    RowCheck::Changed => {
                        jdb::set_row_state(&self.ctx.pool, r.id, "failed", Some(CHANGED_OUTSIDE))
                            .await?
                    }
                }
            }
            rows = jdb::rows(&self.ctx.pool, self.run.id).await?;
        }
        let mut db_paths = Vec::new();
        if resumed {
            let (r, list) = (root.clone(), rows.clone());
            let removed = blocking(move || Ok(journal::remove_leftovers(&r, &list))).await?;
            if removed > 0 {
                info!(run = %self.run.id, removed, "deleted leftover tag copies");
            }
        } else {
            // Step 1 (also again after a crash before the journal commit: nothing was touched).
            let pre = self.precheck(&applied, target, &keys).await?;
            match target {
                Target::Files | Target::Copy => {
                    let (paths, copy_run, root_name) = match pre.copy {
                        Some((run, copies)) => (copies, Some(run), "copy"),
                        None => (pre.paths, None, "library"),
                    };
                    if let Some(run) = &copy_run {
                        root = PathBuf::from(&run.target_root);
                    }
                    let mut planned = self.plan(&root, &applied, &paths).await?;
                    for row in &mut planned {
                        row.root = root_name.to_string();
                    }
                    let mut tx = self.ctx.pool.begin().await?;
                    if let Some(run) = &copy_run {
                        adb::set_copy_run(&mut tx, self.run.id, run.id).await?;
                    }
                    jdb::insert_planned(&mut tx, self.run.id, &planned).await?;
                    tx.commit().await?;
                    if let Some(h) = &self.hooks.after_journal {
                        h();
                    }
                    rows = jdb::rows(&self.ctx.pool, self.run.id).await?;
                }
                Target::Db => db_paths = pre.paths.into_values().collect(),
            }
        }

        let summary = match target {
            Target::Files | Target::Copy => {
                self.fail_point(FailPoint::BeforeWrites)?;
                self.write_rows(&root, &rows, resumed).await?;
                let rows = jdb::rows(&self.ctx.pool, self.run.id).await?;
                let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
                if n("written") + n("skipped") == 0 {
                    return Err(AutotagError::Refused(NOTHING_WRITTEN.to_string()));
                }
                let overrides: Vec<(Uuid, Value)> = if target == Target::Copy {
                    // The library files keep their tags: the overrides show the match (as
                    // `db`) until a switch makes the tagged copies the library. Only for the
                    // tracks whose copy holds the new tags (written, or already tagged): a
                    // copy that failed was never tagged, and the switch would revert its match.
                    let tagged: BTreeSet<Uuid> = rows
                        .iter()
                        .filter(|r| r.state == "written" || r.state == "skipped")
                        .filter_map(|r| r.track_id)
                        .collect();
                    db_overrides(&applied)?
                        .into_iter()
                        .filter(|(t, _)| tagged.contains(t))
                        .collect()
                } else {
                    let year = (!applied.mbids_only)
                        .then_some(applied.album.year)
                        .flatten();
                    match year {
                        Some(y) => rows
                            .iter()
                            .filter(|r| r.state == "written" || r.state == "skipped")
                            .filter_map(|r| r.track_id)
                            .map(|t| {
                                let o = TagOverrides {
                                    year: Some(y),
                                    ..TagOverrides::default()
                                };
                                Ok((t, serde_json::to_value(o).map_err(io)?))
                            })
                            .collect::<Result<_>>()?,
                        None => Vec::new(),
                    }
                };
                self.fail_point(FailPoint::BeforeIdentity)?;
                self.identity(&keys, &overrides).await?;
                self.fail_point(FailPoint::AfterIdentity)?;
                if target == Target::Copy {
                    // The library files did not change: a plain re-read, through the overrides.
                    let mut paths = self.library_paths().await?;
                    paths.sort();
                    self.prune_and_reread(&paths, ReadCause::Scan).await?;
                } else {
                    let (mut tag_write, mut scan_read) = (Vec::new(), Vec::new());
                    for r in &rows {
                        match r.state.as_str() {
                            "written" | "skipped" => tag_write.push(r.path.clone()),
                            _ => scan_read.push(r.path.clone()),
                        }
                    }
                    self.prune_and_reread(&tag_write, ReadCause::TagWrite)
                        .await?;
                    if !scan_read.is_empty() {
                        self.reread(&scan_read, ReadCause::Scan).await?;
                    }
                }
                json!({"written": n("written"), "skipped": n("skipped"), "failed": n("failed"),
                       "organized": 0})
            }
            Target::Db => {
                self.total = applied.tracks.len();
                let overrides = db_overrides(&applied)?;
                self.fail_point(FailPoint::BeforeIdentity)?;
                self.identity(&keys, &overrides).await?;
                self.fail_point(FailPoint::AfterIdentity)?;
                db_paths.sort();
                self.prune_and_reread(&db_paths, ReadCause::Scan).await?;
                self.done = self.total;
                json!({"written": overrides.len(), "skipped": 0, "failed": 0, "organized": 0})
            }
        };

        // Step 5: re-file the album's files under their new names.
        if self.run.reorganize {
            adb::transition_run(
                &self.ctx.pool,
                self.run.id,
                &[RunState::Writing],
                RunState::Organizing,
            )
            .await?;
            return self.organize_and_finish().await;
        }
        adb::mark_album_applied(&self.ctx.pool, self.run.album_id).await?;
        adb::finish_run(&self.ctx.pool, self.run.id, RunState::Done, &summary, None).await?;
        self.finish_event(RunState::Done.as_str());
        info!(run = %self.run.id, album = %self.run.album_id, %summary, "autotag apply finished");
        Ok(())
    }

    /// Step 5 and step 6 of an `organizing` run. An interruption leaves the run `organizing`,
    /// to resume the same organizer run. A database error is returned (the job retries, the run
    /// stays `organizing`): one raised by the organizer's preview or apply has already ended
    /// that organizer run `failed` (as the organizer job does), so the retry ends the run `done`
    /// with `organizeError`; one raised around it (creating the run, reading its state) retries
    /// the same organizer run. Any other failure of the re-filing ends the run `done` with
    /// `organizeError` at once (see the module doc).
    async fn organize_and_finish(&mut self) -> Result<()> {
        if self.total == 0 {
            // A resumed run: the files were all handled before.
            self.total = jdb::rows(&self.ctx.pool, self.run.id).await?.len();
            self.done = self.total;
        }
        self.progress(RunState::Organizing.as_str());
        self.fail_point(FailPoint::BeforeOrganize)?;
        let error = match self.organize().await {
            Ok(()) => None,
            Err(OrganizeError::Interrupted) => return Err(AutotagError::Interrupted),
            Err(OrganizeError::Db(e)) => return Err(AutotagError::Db(e)),
            Err(e) => {
                let msg = e.to_string();
                warn!(run = %self.run.id, reason = %msg, "the reorganize did not complete");
                Some(msg)
            }
        };
        finish_organized(self.ctx, self.run, error.as_deref()).await?;
        self.finish_event(RunState::Done.as_str());
        Ok(())
    }

    /// The reorganize: the run's organizer run (made here, with its id stored on the run in the
    /// same transaction, unless it has one), previewed then applied.
    async fn organize(&self) -> std::result::Result<(), OrganizeError> {
        let pool = &self.ctx.pool;
        let current = adb::get_run(pool, self.run.id)
            .await?
            .and_then(|r| r.organizer_run_id);
        let organizer_run = match current {
            Some(id) => id,
            None => {
                let root = self.ctx.root.get();
                if !self.files_writable(&root).await {
                    return Err(OrganizeError::Refused(FILES_READ_ONLY.to_string()));
                }
                let root = root.display().to_string();
                let id = Uuid::now_v7();
                let mut tx = pool.begin().await?;
                odb::create_scoped_run_in(
                    &mut tx,
                    id,
                    odb::MODE_MOVE,
                    &root,
                    &root,
                    self.run.created_by,
                    &[self.run.album_id],
                    "autotag",
                )
                .await?;
                if !adb::set_organizer_run(&mut tx, self.run.id, id).await? {
                    // Another call made it first (cannot happen on the concurrency-1 queue):
                    // keep that one.
                    tx.rollback().await?;
                    return Err(OrganizeError::Invalid(
                        "the run already has a reorganize run".to_string(),
                    ));
                }
                tx.commit().await?;
                info!(run = %self.run.id, organizer_run = %id, "reorganize run created");
                id
            }
        };
        organize::preview_then_apply_with(
            self.ctx,
            organizer_run,
            &self.hooks.organize,
            self.cancel,
        )
        .await
    }

    /// A resumed `copy` run's target: the copy run recorded with its journal, still `done` and
    /// not switched to; else [`COPY_FIRST`].
    async fn resumed_copy_root(&self) -> Result<PathBuf> {
        let refused = || AutotagError::Refused(COPY_FIRST.to_string());
        let id = self.run.copy_run_id.ok_or_else(refused)?;
        let run = odb::get_run(&self.ctx.pool, id)
            .await?
            .ok_or_else(refused)?;
        if run.state != "done" || run.switched_at.is_some() {
            return Err(refused());
        }
        Ok(PathBuf::from(run.target_root))
    }

    /// The library paths of the album's present tracks.
    async fn library_paths(&self) -> Result<Vec<String>> {
        Ok(adb::local_album(&self.ctx.pool, self.run.album_id)
            .await?
            .map(|a| a.tracks.into_iter().map(|t| t.path).collect())
            .unwrap_or_default())
    }

    fn fail_point(&self, at: FailPoint) -> Result<()> {
        if self.hooks.fail_at == Some(at) {
            return Err(AutotagError::Io("forced failure (test hook)".to_string()));
        }
        Ok(())
    }

    async fn files_writable(&self, root: &Path) -> bool {
        let r = root.to_path_buf();
        tokio::task::spawn_blocking(move || writable(&r))
            .await
            .unwrap_or(false)
    }

    /// Step 1 under the row locks (tracks, then the album; released at the commit, before any
    /// file is touched): the candidate's album, no other working run, no applied match, the
    /// track set, the collision; then the files check (for `copy`: the copy run and its copies).
    async fn precheck(
        &mut self,
        applied: &Applied,
        target: Target,
        keys: &IdentityKeys,
    ) -> Result<Precheck> {
        let album = self.run.album_id;
        let mut tx = self.ctx.pool.begin().await?;
        let Some(present) = adb::lock_album_tracks(&mut tx, album).await? else {
            return Err(AutotagError::Refused(IdentityError::AlbumGone.to_string()));
        };
        if let Some(candidate) = self.run.candidate_id
            && let Some(owner) = adb::candidate_album(&mut tx, candidate).await?
            && owner != album
        {
            return Err(AutotagError::Refused(WRONG_ALBUM.to_string()));
        }
        if adb::other_active_run(&mut tx, album, self.run.id)
            .await?
            .is_some()
        {
            return Err(AutotagError::Refused(ALBUM_BUSY.to_string()));
        }
        // Another run's overrides would be replaced (and lost when this run is undone), or a
        // failed run's files and identity would become this run's `before`.
        // The album row's own applied identity counts too (a done run that left no override,
        // e.g. a `files` run without a year): unless it is this run's (`album_before` set: a
        // `db` run resumed after its identity transaction).
        if adb::foreign_override(&mut tx, album, self.run.id).await?
            || adb::needs_undo_run(&mut tx, album, self.run.id)
                .await?
                .is_some()
            || (self.run.album_before.is_none()
                && adb::album_identity_applied(&mut tx, album).await?)
        {
            return Err(AutotagError::Refused(APPLIED_MATCH_EXISTS.to_string()));
        }
        let want: BTreeSet<Uuid> = applied.tracks.iter().map(|t| t.track_id).collect();
        let have: BTreeSet<Uuid> = present.iter().map(|t| t.id).collect();
        if want != have || want.len() != applied.tracks.len() {
            return Err(AutotagError::Refused(TRACKS_CHANGED.to_string()));
        }
        if target != Target::Db && present.iter().any(|t| t.via_symlink) {
            return Err(AutotagError::Refused(SYMLINK_NOTE.to_string()));
        }
        if !applied.mbids_only
            && let Some(c) = adb::find_collision(&mut tx, album, keys, applied.album.year).await?
        {
            return Err(AutotagError::Refused(c.message()));
        }
        tx.commit().await?;

        let mut copy = None;
        if target != Target::Db {
            let local = adb::local_album(&self.ctx.pool, album)
                .await?
                .ok_or_else(|| AutotagError::Refused(TRACKS_CHANGED.to_string()))?;
            if target == Target::Copy {
                let run = usable_copy_run(self.ctx)
                    .await?
                    .ok_or_else(|| AutotagError::Refused(COPY_FIRST.to_string()))?;
                let copies = copy_refusal(self.ctx, &local, &run)
                    .await?
                    .map_err(AutotagError::Refused)?;
                copy = Some((run, copies));
            } else if let Some(reason) = album_refusal(self.ctx, &local, target).await? {
                return Err(AutotagError::Refused(reason));
            }
        }
        Ok(Precheck {
            paths: present.into_iter().map(|t| (t.id, t.path)).collect(),
            copy,
        })
    }

    /// Step 2's rows, one per track (`paths`: track id → path under `root`), in path order. A
    /// file that cannot be read refuses the apply (nothing is touched yet), naming its path
    /// relative to `root`. The rows say `root='library'`; the caller sets `copy`.
    async fn plan(
        &self,
        root: &Path,
        applied: &Applied,
        paths: &HashMap<Uuid, String>,
    ) -> Result<Vec<NewJournalRow>> {
        let mut work: Vec<(Uuid, String, TagValues)> = applied
            .tracks
            .iter()
            .filter_map(|t| {
                paths.get(&t.track_id).map(|p| {
                    (
                        t.track_id,
                        p.clone(),
                        TagValues::from_applied(&applied.album, t),
                    )
                })
            })
            .collect();
        work.sort_by(|a, b| a.1.cmp(&b.1));
        let root = root.to_path_buf();
        let scrub_root = root.clone();
        blocking(move || {
            Ok(work
                .iter()
                .map(|(id, rel, values)| {
                    journal::plan_file(&root, rel, *id, values).map_err(|e| (rel.clone(), e))
                })
                .collect::<std::result::Result<Vec<_>, _>>())
        })
        .await?
        .map_err(|(rel, e)| match e {
            TagWriteError::Symlink => AutotagError::Refused(SYMLINK_NOTE.to_string()),
            TagWriteError::Unsupported(_) => AutotagError::Refused(e.to_string()),
            e => AutotagError::Refused(journal::scrub(&scrub_root, &format!("{rel}: {e}"))),
        })
    }

    /// Step 3: every `planned` row, in journal order, checkpointing between files.
    async fn write_rows(&mut self, root: &Path, rows: &[JournalRow], resumed: bool) -> Result<()> {
        self.total = rows.len();
        self.done = rows.iter().filter(|r| r.state != "planned").count();
        self.progress(RunState::Writing.as_str());
        for (handled, row) in rows.iter().filter(|r| r.state == "planned").enumerate() {
            if self.cancel.is_cancelled() || self.hooks.stop_after_files == Some(handled) {
                return Err(AutotagError::Interrupted);
            }
            match self.write_one(root, row, resumed).await? {
                RowOutcome::Written(sha) => {
                    let mut tx = self.ctx.pool.begin().await?;
                    jdb::mark_written(&mut tx, row.id, &sha).await?;
                    tx.commit().await?;
                }
                RowOutcome::Skipped(note) => {
                    jdb::set_row_state(&self.ctx.pool, row.id, "skipped", Some(note)).await?
                }
                RowOutcome::Failed(note) => {
                    info!(run = %self.run.id, row = row.id, %note, "autotag row failed");
                    jdb::set_row_state(&self.ctx.pool, row.id, "failed", Some(&note)).await?
                }
                RowOutcome::Crash => return Err(AutotagError::Interrupted),
            }
            self.done += 1;
            self.progress(RunState::Writing.as_str());
        }
        Ok(())
    }

    /// One row: the folder check, the §19.9 state check, then the safe write.
    async fn write_one(&self, root: &Path, row: &JournalRow, resumed: bool) -> Result<RowOutcome> {
        let (root, row) = (root.to_path_buf(), row.clone());
        let ignore = self.ctx.ignore.clone();
        let hooks = self.hooks.write;
        let res =
            tokio::task::spawn_blocking(move || write_row(&root, &row, resumed, &ignore, &hooks))
                .await;
        Ok(res.unwrap_or_else(|e| {
            warn!("the tag writer panicked: {e}");
            RowOutcome::Failed("the tag writer failed unexpectedly; nothing written".to_string())
        }))
    }

    /// Step 4's transaction, retried once on a deadlock: the album identity and the overrides.
    async fn identity(&self, keys: &IdentityKeys, overrides: &[(Uuid, Value)]) -> Result<()> {
        let (pool, run) = (&self.ctx.pool, self.run);
        let outcome = kima_db::retry_once_on_conflict(|| async move {
            let mut tx = pool.begin().await?;
            let step = match adb::apply_identity(&mut tx, run, keys).await {
                Ok(step) => step,
                Err(IdentityError::Db(e)) => return Err(e),
                Err(IdentityError::Collision(c)) => return Ok(IdentityOutcome::Collision(c)),
                Err(IdentityError::AlbumGone) => return Ok(IdentityOutcome::AlbumGone),
            };
            adb::put_overrides(&mut tx, run.id, overrides).await?;
            tx.commit().await?;
            Ok(IdentityOutcome::Applied(step))
        })
        .await?;
        match outcome {
            IdentityOutcome::Applied(step) => {
                info!(run = %run.id, action = step.action, "album identity applied");
                Ok(())
            }
            IdentityOutcome::Collision(c) => Err(AutotagError::Refused(c.message())),
            IdentityOutcome::AlbumGone => {
                Err(AutotagError::Refused(IdentityError::AlbumGone.to_string()))
            }
        }
    }

    /// Prunes what a re-parent left empty, then re-reads `paths`.
    async fn prune_and_reread(&self, paths: &[String], cause: ReadCause) -> Result<()> {
        kima_db::library::prune_empty(&self.ctx.pool).await?;
        self.reread(paths, cause).await
    }

    async fn reread(&self, paths: &[String], cause: ReadCause) -> Result<()> {
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
            warn!(run = %self.run.id, failed = report.failed.len(), "files not re-read after the apply");
        }
        Ok(())
    }

    /// A refusal: see [`settle_failed`].
    async fn refuse(&mut self, msg: &str) -> Result<()> {
        settle_failed(self.ctx, self.run, msg).await?;
        self.finish_event(RunState::Failed.as_str());
        Ok(())
    }
}

/// Blocking: one `planned` row (see [`Job::write_one`]). A failure note names files relative
/// to the library.
fn write_row(
    root: &Path,
    row: &JournalRow,
    resumed: bool,
    ignore: &crate::watch::IgnoreSet,
    hooks: &WriteHooks,
) -> RowOutcome {
    match write_row_inner(root, row, resumed, ignore, hooks) {
        RowOutcome::Failed(note) => RowOutcome::Failed(journal::scrub(root, &note)),
        other => other,
    }
}

fn write_row_inner(
    root: &Path,
    row: &JournalRow,
    resumed: bool,
    ignore: &crate::watch::IgnoreSet,
    hooks: &WriteHooks,
) -> RowOutcome {
    match tagwrite::refuse_symlink(root, &row.path) {
        Ok(()) => {}
        Err(TagWriteError::Symlink) => return RowOutcome::Failed(SYMLINK_NOTE.to_string()),
        Err(e) => return RowOutcome::Failed(e.to_string()),
    }
    match journal::check_row(root, row, resumed) {
        RowCheck::Write => {}
        RowCheck::AlreadyWritten(sha) => return RowOutcome::Written(sha),
        RowCheck::Changed => return RowOutcome::Failed(CHANGED_OUTSIDE.to_string()),
    }
    let (old, new) = match (
        journal::tags_of(&row.old_tags),
        journal::tags_of(&row.new_tags),
    ) {
        (Ok(o), Ok(n)) => (o, n),
        (Err(e), _) | (_, Err(e)) => return RowOutcome::Failed(e.to_string()),
    };
    if old == new {
        return RowOutcome::Skipped(ALREADY_TAGGED);
    }
    let abs: PathBuf = match journal::join_rel(root, &row.path) {
        Ok(p) => p,
        Err(e) => return RowOutcome::Failed(e.to_string()),
    };
    let container = match tagwrite::container_of(&abs) {
        Ok(c) => c,
        Err(e) => return RowOutcome::Failed(e.to_string()),
    };
    // The version the journal recorded (NULL: no ID3v2 tag, written as v2.4).
    let id3 = row.id3_version.as_deref().and_then(Id3Version::parse);
    let expected = row.before_sha.as_deref();
    match tagwrite::safe_write_expecting(
        &abs, row.id, container, id3, &new, ignore, hooks, expected,
    ) {
        Ok(w) => RowOutcome::Written(w.after_sha.to_vec()),
        Err(_) if hooks.stop_after_rename => RowOutcome::Crash,
        Err(e) => RowOutcome::Failed(e.to_string()),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(io)?
        .map_err(io)
}
