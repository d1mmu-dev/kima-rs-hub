//! `autotag_runs` (spec §6.8, §19.9): one apply of a candidate to an album, and its undo.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Applied;

/// Where an apply writes (spec §19.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Files,
    Copy,
    Db,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Copy => "copy",
            Self::Db => "db",
        }
    }
}

/// `autotag_runs.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Writing,
    Organizing,
    Done,
    Failed,
    Undoing,
    Undone,
    UndoFailed,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Writing => "writing",
            Self::Organizing => "organizing",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Undoing => "undoing",
            Self::Undone => "undone",
            Self::UndoFailed => "undo_failed",
        }
    }
}

/// A run about to be created. `applied` must be complete (an `mbids_only` value already passed
/// through `with_local_identity`).
#[derive(Debug, Clone)]
pub struct NewRun {
    pub id: Uuid,
    pub album_id: Uuid,
    pub candidate_id: Uuid,
    pub applied: Applied,
    pub target: Target,
    pub reorganize: bool,
    pub created_by: Option<Uuid>,
}

/// Every `autotag_runs` column.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: Uuid,
    pub album_id: Uuid,
    pub candidate_id: Option<Uuid>,
    pub applied: Option<Value>,
    pub album_before: Option<Value>,
    pub artist_action: Option<String>,
    pub old_artist_id: Option<Uuid>,
    pub new_artist_id: Option<Uuid>,
    pub old_artist_name: Option<String>,
    pub target: String,
    pub reorganize: bool,
    pub state: String,
    pub organizer_run_id: Option<Uuid>,
    pub copy_run_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub summary: Option<Value>,
    pub error: Option<String>,
}

impl RunRow {
    /// The stored §19.6 values.
    pub fn applied(&self) -> Result<Applied, serde_json::Error> {
        serde_json::from_value(self.applied.clone().unwrap_or(Value::Null))
    }
}

/// Creates the run (`queued`) and marks the album `applying` with this candidate chosen, in
/// the caller's transaction.
pub async fn create_run(conn: &mut PgConnection, r: &NewRun) -> sqlx::Result<RunRow> {
    let applied = serde_json::to_value(&r.applied).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let row = sqlx::query_as!(
        RunRow,
        "INSERT INTO autotag_runs (id, album_id, candidate_id, applied, target, reorganize, state,
                                   created_by)
         VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7)
         RETURNING id, album_id, candidate_id, applied, album_before, artist_action,
                   old_artist_id, new_artist_id, old_artist_name, target, reorganize, state,
                   organizer_run_id, copy_run_id, created_by, created_at, finished_at, summary,
                   error",
        r.id,
        r.album_id,
        r.candidate_id,
        applied,
        r.target.as_str(),
        r.reorganize,
        r.created_by
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO autotag_albums (album_id, state, chosen_candidate) VALUES ($1, 'applying', $2)
         ON CONFLICT (album_id) DO UPDATE SET state = 'applying',
           chosen_candidate = EXCLUDED.chosen_candidate, error = NULL, updated_at = now()",
        r.album_id,
        r.candidate_id
    )
    .execute(&mut *conn)
    .await?;
    Ok(row)
}

pub async fn get_run(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<RunRow>> {
    sqlx::query_as!(
        RunRow,
        "SELECT id, album_id, candidate_id, applied, album_before, artist_action, old_artist_id,
                new_artist_id, old_artist_name, target, reorganize, state, organizer_run_id,
                copy_run_id, created_by, created_at, finished_at, summary, error
         FROM autotag_runs WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
}

/// Moves a run from one of `from` to `to`. `false` when it was in none of them, so two
/// requests cannot both start the same transition.
pub async fn transition_run(
    pool: &PgPool,
    id: Uuid,
    from: &[RunState],
    to: RunState,
) -> sqlx::Result<bool> {
    let from: Vec<String> = from.iter().map(|s| s.as_str().to_string()).collect();
    let r = sqlx::query!(
        "UPDATE autotag_runs SET state = $3 WHERE id = $1 AND state = ANY($2)",
        id,
        &from,
        to.as_str()
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Ends a run: its final state, summary (no secret ever goes in it) and failure text, with
/// `finished_at` now.
pub async fn finish_run(
    pool: &PgPool,
    id: Uuid,
    state: RunState,
    summary: &Value,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE autotag_runs SET state = $2, summary = $3, error = $4, finished_at = now()
         WHERE id = $1",
        id,
        state.as_str(),
        summary,
        error
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// One present track of an album, as the apply job's pre-check sees it under the row locks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentTrack {
    pub id: Uuid,
    pub path: String,
    /// `guessed.viaSymlink`: the row stands for a file reached through a symbolic link.
    pub via_symlink: bool,
}

/// The apply job's pre-check locks (spec §19.9 step 1), in the identity lock order: the album's
/// tracks, missing ones included, in id order (`FOR NO KEY UPDATE`), then the album row. Answers
/// the present tracks (by path), `None` when the album is gone.
pub async fn lock_album_tracks(
    conn: &mut PgConnection,
    album: Uuid,
) -> sqlx::Result<Option<Vec<PresentTrack>>> {
    let rows = sqlx::query!(
        r#"SELECT id, path, missing_since IS NOT NULL AS "missing!",
                  COALESCE((guessed ->> 'viaSymlink')::boolean, false) AS "via_symlink!"
           FROM tracks WHERE album_id = $1 ORDER BY id FOR NO KEY UPDATE"#,
        album
    )
    .fetch_all(&mut *conn)
    .await?;
    let found = sqlx::query_scalar!("SELECT id FROM albums WHERE id = $1 FOR UPDATE", album)
        .fetch_optional(&mut *conn)
        .await?;
    if found.is_none() {
        return Ok(None);
    }
    let mut present: Vec<PresentTrack> = rows
        .into_iter()
        .filter(|r| !r.missing)
        .map(|r| PresentTrack {
            id: r.id,
            path: r.path,
            via_symlink: r.via_symlink,
        })
        .collect();
    present.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Some(present))
}

/// Whether any of these tracks has `guessed.viaSymlink` set.
pub async fn any_via_symlink(pool: &PgPool, tracks: &[Uuid]) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM tracks WHERE id = ANY($1)
               AND COALESCE((guessed ->> 'viaSymlink')::boolean, false)) AS "e!""#,
        tracks
    )
    .fetch_one(pool)
    .await
}

/// The album a candidate belongs to; `None` when the candidate is gone.
pub async fn candidate_album(
    conn: &mut PgConnection,
    candidate: Uuid,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!(
        "SELECT album_id FROM autotag_candidates WHERE id = $1",
        candidate
    )
    .fetch_optional(&mut *conn)
    .await
}

/// Another run of the album that is still working (`queued`, `writing`, `organizing` or
/// `undoing`), other than `run`.
pub async fn other_active_run(
    conn: &mut PgConnection,
    album: Uuid,
    run: Uuid,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar!(
        "SELECT id FROM autotag_runs
         WHERE album_id = $1 AND id <> $2 AND state IN ('queued', 'writing', 'organizing', 'undoing')
         ORDER BY created_at, id LIMIT 1",
        album,
        run
    )
    .fetch_optional(&mut *conn)
    .await
}

/// Whether any track of the album (missing ones included) has a tag override stored by a run
/// other than `run`: an applied match that must be undone before another apply.
pub async fn foreign_override(
    conn: &mut PgConnection,
    album: Uuid,
    run: Uuid,
) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM track_tag_overrides o JOIN tracks t ON t.id = o.track_id
             WHERE t.album_id = $1 AND o.run_id <> $2) AS "e!""#,
        album,
        run
    )
    .fetch_one(&mut *conn)
    .await
}

/// Whether the album row holds an applied match (`identity_source = 'autotag'`): a re-match must
/// undo it first. `false` when the album is gone.
pub async fn album_identity_applied(conn: &mut PgConnection, album: Uuid) -> sqlx::Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT identity_source = 'autotag' AS "a!" FROM albums WHERE id = $1"#,
        album
    )
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(false))
}

/// Records the copy-mode organizer run a `copy` apply writes into, in the caller's transaction
/// (with the journal, so a resumed run writes into the same copy).
pub async fn set_copy_run(conn: &mut PgConnection, run: Uuid, copy_run: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE autotag_runs SET copy_run_id = $2 WHERE id = $1",
        run,
        copy_run
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Records the reorganize's organizer run, in the caller's transaction (the one that creates
/// it). `false` when the run already has one: never two organizer runs for one apply.
pub async fn set_organizer_run(
    conn: &mut PgConnection,
    run: Uuid,
    organizer_run: Uuid,
) -> sqlx::Result<bool> {
    let r = sqlx::query!(
        "UPDATE autotag_runs SET organizer_run_id = $2
         WHERE id = $1 AND organizer_run_id IS NULL",
        run,
        organizer_run
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// The state of the album's newest run other than `run` that did not fail (a refused run
/// changed nothing), if it has one.
pub async fn latest_other_run_state(
    pool: &PgPool,
    album: Uuid,
    run: Uuid,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT state FROM autotag_runs WHERE album_id = $1 AND id <> $2 AND state <> 'failed'
         ORDER BY created_at DESC, id DESC LIMIT 1",
        album,
        run
    )
    .fetch_optional(pool)
    .await
}

/// Whether an `autotag-apply` or `autotag-undo` job of the run waits or runs on `queue`.
pub async fn job_pending(pool: &PgPool, queue: &str, run: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM jobs WHERE queue = $1 AND state IN ('queued', 'running')
               AND payload ->> 'kind' IN ('autotag-apply', 'autotag-undo')
               AND payload ->> 'runId' = $2::uuid::text) AS "e!""#,
        queue,
        run
    )
    .fetch_one(pool)
    .await
}

/// The album's newest `failed` run other than `except` that left something to undo: files it
/// wrote (`written` journal rows) or a committed identity (`album_before` set). Its id and error.
pub async fn needs_undo_run(
    conn: &mut PgConnection,
    album: Uuid,
    except: Uuid,
) -> sqlx::Result<Option<(Uuid, Option<String>)>> {
    Ok(sqlx::query!(
        "SELECT r.id, r.error FROM autotag_runs r
         WHERE r.album_id = $1 AND r.id <> $2 AND r.state = 'failed'
           AND (r.album_before IS NOT NULL
                OR EXISTS (SELECT 1 FROM autotag_journal j
                           WHERE j.run_id = r.id AND j.state = 'written'))
         ORDER BY r.created_at DESC, r.id DESC LIMIT 1",
        album,
        except
    )
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| (r.id, r.error)))
}

/// Runs a job still owes work to (`queued`, `writing`, `organizing`, `undoing`): `(id, state)`.
pub async fn unfinished_runs(pool: &PgPool) -> sqlx::Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        "SELECT id, state FROM autotag_runs
         WHERE state IN ('queued', 'writing', 'organizing', 'undoing')
         ORDER BY created_at, id"
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.id, r.state))
    .collect())
}

/// The album's apply finished: `autotag_albums.state = 'applied'`, `applied_at` now.
pub async fn mark_album_applied(pool: &PgPool, album: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO autotag_albums (album_id, state, applied_at) VALUES ($1, 'applied', now())
         ON CONFLICT (album_id) DO UPDATE SET state = 'applied', applied_at = now(), error = NULL,
           updated_at = now()",
        album
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The album's run that is still working (`queued`, `writing`, `organizing` or `undoing`), the
/// newest if there were several.
pub async fn active_run(pool: &PgPool, album: Uuid) -> sqlx::Result<Option<RunRow>> {
    sqlx::query_as!(
        RunRow,
        "SELECT id, album_id, candidate_id, applied, album_before, artist_action, old_artist_id,
                new_artist_id, old_artist_name, target, reorganize, state, organizer_run_id,
                copy_run_id, created_by, created_at, finished_at, summary, error
         FROM autotag_runs
         WHERE album_id = $1 AND state IN ('queued', 'writing', 'organizing', 'undoing')
         ORDER BY created_at DESC, id DESC LIMIT 1",
        album
    )
    .fetch_optional(pool)
    .await
}
