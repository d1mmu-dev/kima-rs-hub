//! Organizer runs and their journal (spec §5.2). The file work lives in
//! `kima_library::organize::apply`; this module only reads and writes rows.

use serde_json::Value;
use sqlx::{FromRow, PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MODE_COPY: &str = "copy";
pub const MODE_MOVE: &str = "move";

#[derive(Debug, Clone, FromRow)]
pub struct Run {
    pub id: Uuid,
    pub mode: String,
    pub state: String,
    pub source_root: String,
    pub target_root: String,
    pub created_by: Option<Uuid>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub switched_at: Option<OffsetDateTime>,
    pub summary: Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct JournalRow {
    pub id: i64,
    pub run_id: Uuid,
    pub track_id: Option<Uuid>,
    pub from_path: String,
    pub to_path: String,
    pub sha256: Option<Vec<u8>>,
    pub state: String,
    pub note: Option<String>,
    pub collision: bool,
    pub applied_seq: Option<i64>,
}

/// A row to insert when a run is planned.
#[derive(Debug, Clone)]
pub struct NewRow {
    pub track_id: Option<Uuid>,
    pub from_path: String,
    pub to_path: String,
    pub collision: bool,
    /// Already at its target: stored as `unchanged`, never applied.
    pub unchanged: bool,
}

/// A journal row with what the preview shows about its track (all `None` for a sidecar or a
/// track that is gone).
#[derive(Debug, Clone, FromRow)]
pub struct RowView {
    #[sqlx(flatten)]
    pub row: JournalRow,
    pub album_id: Option<Uuid>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub album_year: Option<i32>,
    pub track_no: Option<i32>,
    pub guessed: Option<sqlx::types::Json<crate::library::Guessed>>,
}

/// What a non-missing track contributes to path planning.
#[derive(Debug, Clone, FromRow)]
pub struct PlanTrack {
    pub id: Uuid,
    pub path: String,
    pub album_artist: String,
    pub track_artist: String,
    pub album: String,
    pub album_year: Option<i32>,
    pub disc: i32,
    pub track_no: i32,
    pub title: String,
    pub is_compilation: bool,
}

const RUN_COLUMNS: &str = "id, mode, state, source_root, target_root, created_by, created_at, \
                           finished_at, switched_at, summary, error";
const ROW_COLUMNS: &str =
    "id, run_id, track_id, from_path, to_path, sha256, state, note, collision, applied_seq";

pub async fn create_run(
    pool: &PgPool,
    id: Uuid,
    mode: &str,
    source_root: &str,
    target_root: &str,
    created_by: Option<Uuid>,
) -> sqlx::Result<Run> {
    sqlx::query_as(&format!(
        "INSERT INTO organizer_runs (id, mode, source_root, target_root, created_by)
         VALUES ($1, $2, $3, $4, $5) RETURNING {RUN_COLUMNS}"
    ))
    .bind(id)
    .bind(mode)
    .bind(source_root)
    .bind(target_root)
    .bind(created_by)
    .fetch_one(pool)
    .await
}

pub async fn get_run(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<Run>> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM organizer_runs WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// The newest runs first.
pub async fn list_runs(pool: &PgPool, limit: i64) -> sqlx::Result<Vec<Run>> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM organizer_runs ORDER BY created_at DESC, id DESC LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Moves a run from one of `from` to `to`. Returns false when it was in none of them, so two
/// requests cannot both start the same transition.
pub async fn transition(pool: &PgPool, id: Uuid, from: &[&str], to: &str) -> sqlx::Result<bool> {
    let from: Vec<String> = from.iter().map(|s| s.to_string()).collect();
    let done = matches!(to, "previewed" | "done" | "failed" | "undone");
    // An undo remembers the state it started from, so a refused undo can return to it.
    let res = sqlx::query(
        "UPDATE organizer_runs
         SET state = $3, error = NULL,
             finished_at = CASE WHEN $4 THEN now() ELSE NULL END,
             summary = CASE WHEN $3 = 'undoing'
                            THEN summary || jsonb_build_object('undoFrom', state)
                            ELSE summary END
         WHERE id = $1 AND state = ANY($2)",
    )
    .bind(id)
    .bind(&from)
    .bind(to)
    .bind(done)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// A refused undo: the run goes back to the state it was undone from (`done` or `failed`),
/// with the reason in `error`.
pub async fn revert_undo(pool: &PgPool, id: Uuid, reason: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE organizer_runs
         SET state = CASE WHEN summary ->> 'undoFrom' IN ('done', 'failed')
                          THEN summary ->> 'undoFrom' ELSE 'failed' END,
             error = $2, finished_at = now()
         WHERE id = $1 AND state = 'undoing'",
    )
    .bind(id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Ends a run as `failed` with a reason.
pub async fn fail_run(pool: &PgPool, id: Uuid, error: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE organizer_runs SET state = 'failed', error = $2, finished_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_summary(pool: &PgPool, id: Uuid, summary: &Value) -> sqlx::Result<()> {
    sqlx::query("UPDATE organizer_runs SET summary = summary || $2 WHERE id = $1")
        .bind(id)
        .bind(summary)
        .execute(pool)
        .await?;
    Ok(())
}

/// Runs left mid-way (`planning`, `applying`, `undoing`).
pub async fn interrupted_runs(pool: &PgPool) -> sqlx::Result<Vec<Run>> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM organizer_runs
         WHERE state IN ('planning', 'applying', 'undoing') ORDER BY created_at"
    ))
    .fetch_all(pool)
    .await
}

/// Replaces the run's journal with `rows` (a re-planned preview starts over).
pub async fn replace_rows(pool: &PgPool, run_id: Uuid, rows: &[NewRow]) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM organizer_journal WHERE run_id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    for chunk in rows.chunks(1000) {
        let tracks: Vec<Option<Uuid>> = chunk.iter().map(|r| r.track_id).collect();
        let froms: Vec<&str> = chunk.iter().map(|r| r.from_path.as_str()).collect();
        let tos: Vec<&str> = chunk.iter().map(|r| r.to_path.as_str()).collect();
        let collisions: Vec<bool> = chunk.iter().map(|r| r.collision).collect();
        let states: Vec<&str> = chunk
            .iter()
            .map(|r| if r.unchanged { "unchanged" } else { "planned" })
            .collect();
        sqlx::query(
            "INSERT INTO organizer_journal (run_id, track_id, from_path, to_path, collision, state)
             SELECT $1, t, f, p, c, s
             FROM UNNEST($2::uuid[], $3::text[], $4::text[], $5::bool[], $6::text[])
                  WITH ORDINALITY AS u(t, f, p, c, s, n)
             ORDER BY n",
        )
        .bind(run_id)
        .bind(&tracks)
        .bind(&froms)
        .bind(&tos)
        .bind(&collisions)
        .bind(&states)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// One page of the journal, in plan order, with track details, plus the total row count.
pub async fn rows(
    pool: &PgPool,
    run_id: Uuid,
    offset: i64,
    limit: i64,
) -> sqlx::Result<(Vec<RowView>, i64)> {
    let rows = sqlx::query_as(
        "SELECT j.id, j.run_id, j.track_id, j.from_path, j.to_path, j.sha256, j.state, j.note,
                j.collision, j.applied_seq,
                al.id AS album_id, al.title AS album, ar.name AS artist, al.year AS album_year,
                t.track_no, t.guessed
         FROM organizer_journal j
         LEFT JOIN tracks t ON t.id = j.track_id
         LEFT JOIN albums al ON al.id = t.album_id
         LEFT JOIN artists ar ON ar.id = al.artist_id
         WHERE j.run_id = $1
         ORDER BY j.id OFFSET $2 LIMIT $3",
    )
    .bind(run_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar("SELECT count(*) FROM organizer_journal WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}

/// Every row of a run in plan order.
pub async fn all_rows(pool: &PgPool, run_id: Uuid) -> sqlx::Result<Vec<JournalRow>> {
    sqlx::query_as(&format!(
        "SELECT {ROW_COLUMNS} FROM organizer_journal WHERE run_id = $1 ORDER BY id"
    ))
    .bind(run_id)
    .fetch_all(pool)
    .await
}

/// Row counts per state.
pub async fn state_counts(pool: &PgPool, run_id: Uuid) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT state, count(*) FROM organizer_journal WHERE run_id = $1 GROUP BY state ORDER BY state",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
}

/// How many present tracks (not flagged missing) have no `done` row in the run: tracks that
/// were not copied, or arrived after the preview. Switching to the copy would lose them.
pub async fn tracks_not_done(pool: &PgPool, run_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM tracks t
         WHERE t.missing_since IS NULL
           AND NOT EXISTS (SELECT 1 FROM organizer_journal j
                           WHERE j.run_id = $1 AND j.track_id = t.id AND j.state = 'done')",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
}

/// Records the source checksum, before the file is touched.
pub async fn set_sha(pool: &PgPool, row_id: i64, sha: &[u8]) -> sqlx::Result<()> {
    sqlx::query("UPDATE organizer_journal SET sha256 = $2 WHERE id = $1")
        .bind(row_id)
        .bind(sha)
        .execute(pool)
        .await?;
    Ok(())
}

/// Sets a row's state and note. `done` also takes the next `applied_seq`.
pub async fn set_row_state(
    conn: &mut PgConnection,
    row_id: i64,
    state: &str,
    note: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE organizer_journal
         SET state = $2, note = $3,
             applied_seq = CASE WHEN $2 = 'done' THEN nextval('organizer_applied_seq')
                                ELSE applied_seq END
         WHERE id = $1",
    )
    .bind(row_id)
    .bind(state)
    .bind(note)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Points a track at a new path (relative to the library root).
pub async fn set_track_path(
    conn: &mut PgConnection,
    track_id: Uuid,
    path: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE tracks SET path = $2, updated_at = now() WHERE id = $1")
        .bind(track_id)
        .bind(path)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The track (if any) whose path is `path`.
pub async fn track_at(pool: &PgPool, path: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT id FROM tracks WHERE path = $1")
        .bind(path)
        .fetch_optional(pool)
        .await
}

/// A track's current path.
pub async fn track_path(pool: &PgPool, track_id: Uuid) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT path FROM tracks WHERE id = $1")
        .bind(track_id)
        .fetch_optional(pool)
        .await
}

/// Every present track with the fields path planning needs.
pub async fn plan_tracks(pool: &PgPool) -> sqlx::Result<Vec<PlanTrack>> {
    sqlx::query_as(
        "SELECT t.id, t.path, ar.name AS album_artist, t.artist_name AS track_artist,
                al.title AS album, al.year AS album_year, t.disc, t.track_no, t.title,
                al.is_compilation
         FROM tracks t
         JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = al.artist_id
         WHERE t.missing_since IS NULL
         ORDER BY t.path",
    )
    .fetch_all(pool)
    .await
}

/// Switches the library to a copy run's target: every done track row's track takes its
/// `to_path`, the run is stamped `switched_at`, and `system_settings.library_root` becomes the
/// target root. One transaction. Returns how many tracks were re-pointed.
pub async fn switch_root(pool: &PgPool, run_id: Uuid, target_root: &str) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    // Two steps through a unique placeholder, so a track taking a path another track is
    // about to leave cannot trip the unique index half-way.
    sqlx::query(
        "UPDATE tracks t SET path = 'kima-organizer-switch/' || j.id || '/' || j.to_path
         FROM organizer_journal j
         WHERE j.run_id = $1 AND j.state = 'done' AND j.track_id = t.id",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    let moved = sqlx::query(
        "UPDATE tracks t SET path = j.to_path, updated_at = now()
         FROM organizer_journal j
         WHERE j.run_id = $1 AND j.state = 'done' AND j.track_id = t.id",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("UPDATE organizer_runs SET switched_at = now() WHERE id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    crate::settings::set_library_root(&mut *tx, Some(target_root)).await?;
    tx.commit().await?;
    Ok(moved)
}

/// Track paths that are staging names of rows left parked mid-run (`staged`, `undo_staged`).
/// The files are hidden, so a scan would not see them; it must not flag those tracks missing.
pub async fn parked_paths(pool: &PgPool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT t.path FROM organizer_journal j JOIN tracks t ON t.id = j.track_id
         WHERE j.state IN ('staged', 'undo_staged')",
    )
    .fetch_all(pool)
    .await
}
