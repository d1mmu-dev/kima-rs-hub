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
