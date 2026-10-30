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
    /// The albums a scoped run plans; `None` is the whole library.
    pub album_ids: Option<Vec<Uuid>>,
    /// `user` or `autotag`.
    pub origin: String,
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
    pub disc_total: Option<i32>,
}

/// One album's rows in a run's preview: the group a page never splits.
#[derive(Debug, Clone)]
pub struct AlbumRows {
    /// `None` for the trailing group of rows that belong to no album.
    pub album_id: Option<Uuid>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub rows: Vec<RowView>,
}

const RUN_COLUMNS: &str = "id, mode, state, source_root, target_root, created_by, created_at, \
                           finished_at, switched_at, summary, error, album_ids, origin";
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
    let mut conn = pool.acquire().await?;
    insert_run(
        &mut conn,
        id,
        mode,
        source_root,
        target_root,
        created_by,
        None,
        "user",
    )
    .await
}

/// A run that plans only the tracks of `album_ids`.
#[allow(clippy::too_many_arguments)]
pub async fn create_scoped_run(
    pool: &PgPool,
    id: Uuid,
    mode: &str,
    source_root: &str,
    target_root: &str,
    created_by: Option<Uuid>,
    album_ids: &[Uuid],
    origin: &str,
) -> sqlx::Result<Run> {
    let mut conn = pool.acquire().await?;
    insert_run(
        &mut conn,
        id,
        mode,
        source_root,
        target_root,
        created_by,
        Some(album_ids),
        origin,
    )
    .await
}

/// [`create_scoped_run`] in the caller's transaction (the autotag reorganize stores the run's id
/// on its own run in the same transaction, so a crash never leaves two).
#[allow(clippy::too_many_arguments)]
pub async fn create_scoped_run_in(
    conn: &mut PgConnection,
    id: Uuid,
    mode: &str,
    source_root: &str,
    target_root: &str,
    created_by: Option<Uuid>,
    album_ids: &[Uuid],
    origin: &str,
) -> sqlx::Result<Run> {
    insert_run(
        conn,
        id,
        mode,
        source_root,
        target_root,
        created_by,
        Some(album_ids),
        origin,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert_run(
    conn: &mut PgConnection,
    id: Uuid,
    mode: &str,
    source_root: &str,
    target_root: &str,
    created_by: Option<Uuid>,
    album_ids: Option<&[Uuid]>,
    origin: &str,
) -> sqlx::Result<Run> {
    sqlx::query_as(&format!(
        "INSERT INTO organizer_runs (id, mode, source_root, target_root, created_by, album_ids, origin)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {RUN_COLUMNS}"
    ))
    .bind(id)
    .bind(mode)
    .bind(source_root)
    .bind(target_root)
    .bind(created_by)
    .bind(album_ids)
    .bind(origin)
    .fetch_one(&mut *conn)
    .await
}

/// How many of `album_ids` exist as albums, and how many present (not missing) tracks they hold.
pub async fn scope_counts(pool: &PgPool, album_ids: &[Uuid]) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM albums WHERE id = ANY($1)),
                (SELECT count(*) FROM tracks WHERE album_id = ANY($1) AND missing_since IS NULL)",
    )
    .bind(album_ids)
    .fetch_one(pool)
    .await
}

/// Every `previewed` run becomes `superseded`: its plan no longer matches the library, so it
/// can no longer be applied. Returns how many runs changed.
pub async fn supersede_previewed(conn: &mut PgConnection) -> sqlx::Result<u64> {
    let res =
        sqlx::query("UPDATE organizer_runs SET state = 'superseded' WHERE state = 'previewed'")
            .execute(&mut *conn)
            .await?;
    Ok(res.rows_affected())
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

/// Starts applying a `previewed` or `failed` run in one guarded statement: it refuses a run an
/// undo has touched (any `undone`, `undo_staged` or `undo_failed` row), so a concurrent undo
/// cannot slip in between a check and the update. False when nothing was updated.
pub async fn start_apply(pool: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let res = sqlx::query(
        "UPDATE organizer_runs
         SET state = 'applying', error = NULL, finished_at = NULL
         WHERE id = $1 AND state IN ('previewed', 'failed')
           AND NOT EXISTS (SELECT 1 FROM organizer_journal
                           WHERE run_id = $1
                             AND state IN ('undone', 'undo_staged', 'undo_failed'))",
    )
    .bind(id)
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

/// Runs left mid-way (`planning`, `applying`, `undoing`), except the reorganize run of an
/// autotag apply still `organizing`: that apply's own job resumes it (spec §19.9 step 5), and
/// two jobs must never drive one run.
pub async fn interrupted_runs(pool: &PgPool) -> sqlx::Result<Vec<Run>> {
    sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM organizer_runs o
         WHERE state IN ('planning', 'applying', 'undoing')
           AND NOT EXISTS (SELECT 1 FROM autotag_runs a
                           WHERE a.organizer_run_id = o.id AND a.state = 'organizing')
         ORDER BY created_at"
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

/// The journal's rows with the album each belongs to: a track's own album; a sidecar (or a row
/// whose track is gone) takes the album of a track that shares its target folder. `$1` is the
/// run id.
const EFFECTIVE_ALBUM: &str = "WITH jr AS (
        SELECT j.id, t.album_id AS own_album, regexp_replace(j.to_path, '[^/]*$', '') AS dir
        FROM organizer_journal j LEFT JOIN tracks t ON t.id = j.track_id
        WHERE j.run_id = $1
    ), fo AS (
        SELECT dir, (array_agg(own_album ORDER BY own_album))[1] AS album_id
        FROM jr WHERE own_album IS NOT NULL GROUP BY dir
    ), eff AS (
        SELECT jr.id, COALESCE(jr.own_album, fo.album_id) AS album_id
        FROM jr LEFT JOIN fo ON fo.dir = jr.dir
    )";

/// One page of a run's journal by album: `limit` groups from `offset`, each with every one of
/// its rows (plan order), plus the number of groups. Albums come ordered by artist, album, id;
/// rows that belong to no album form a trailing group.
pub async fn rows_by_album(
    pool: &PgPool,
    run_id: Uuid,
    offset: i64,
    limit: i64,
) -> sqlx::Result<(Vec<AlbumRows>, i64)> {
    let groups: Vec<(Option<Uuid>, Option<String>, Option<String>)> = sqlx::query_as(&format!(
        "{EFFECTIVE_ALBUM}
         SELECT e.album_id, ar.name, al.title
         FROM eff e
         LEFT JOIN albums al ON al.id = e.album_id
         LEFT JOIN artists ar ON ar.id = al.artist_id
         GROUP BY e.album_id, ar.name, al.title
         ORDER BY (e.album_id IS NULL), ar.name, al.title, e.album_id
         OFFSET $2 LIMIT $3"
    ))
    .bind(run_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "{EFFECTIVE_ALBUM} SELECT count(*) FROM (SELECT album_id FROM eff GROUP BY album_id) g"
    ))
    .bind(run_id)
    .fetch_one(pool)
    .await?;
    let ids: Vec<Uuid> = groups.iter().filter_map(|g| g.0).collect();
    let with_none = groups.iter().any(|g| g.0.is_none());
    let views: Vec<(Option<Uuid>, RowView)> = if groups.is_empty() {
        Vec::new()
    } else {
        let rows: Vec<RowWithGroup> = sqlx::query_as(&format!(
            "{EFFECTIVE_ALBUM}
             SELECT e.album_id AS group_id,
                    j.id, j.run_id, j.track_id, j.from_path, j.to_path, j.sha256, j.state, j.note,
                    j.collision, j.applied_seq,
                    al.id AS album_id, al.title AS album, ar.name AS artist,
                    al.year AS album_year, t.track_no, t.guessed
             FROM organizer_journal j
             JOIN eff e ON e.id = j.id
             LEFT JOIN tracks t ON t.id = j.track_id
             LEFT JOIN albums al ON al.id = t.album_id
             LEFT JOIN artists ar ON ar.id = al.artist_id
             WHERE e.album_id = ANY($2) OR ($3 AND e.album_id IS NULL)
             ORDER BY j.id"
        ))
        .bind(run_id)
        .bind(&ids)
        .bind(with_none)
        .fetch_all(pool)
        .await?;
        rows.into_iter().map(|r| (r.group_id, r.view)).collect()
    };
    let mut out: Vec<AlbumRows> = groups
        .into_iter()
        .map(|(album_id, artist, album)| AlbumRows {
            album_id,
            artist,
            album,
            rows: Vec::new(),
        })
        .collect();
    for (group, view) in views {
        if let Some(g) = out.iter_mut().find(|g| g.album_id == group) {
            g.rows.push(view);
        }
    }
    Ok((out, total))
}

#[derive(FromRow)]
struct RowWithGroup {
    group_id: Option<Uuid>,
    #[sqlx(flatten)]
    view: RowView,
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

/// Whether an undo has touched the run: any row `undone`, `undo_staged` or `undo_failed`.
pub async fn has_undo_rows(pool: &PgPool, run_id: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM organizer_journal
         WHERE run_id = $1 AND state IN ('undone', 'undo_staged', 'undo_failed'))",
    )
    .bind(run_id)
    .fetch_one(pool)
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
                           WHERE j.run_id = $1 AND j.track_id = t.id
                             AND j.state IN ('done', 'unchanged'))",
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

/// Every present track with the fields path planning needs; with `album_ids`, only those
/// albums' tracks.
pub async fn plan_tracks(
    pool: &PgPool,
    album_ids: Option<&[Uuid]>,
) -> sqlx::Result<Vec<PlanTrack>> {
    sqlx::query_as(
        "SELECT t.id, t.path, ar.name AS album_artist, t.artist_name AS track_artist,
                al.title AS album, al.year AS album_year, t.disc, t.track_no, t.title,
                al.is_compilation, t.disc_total
         FROM tracks t
         JOIN albums al ON al.id = t.album_id
         JOIN artists ar ON ar.id = al.artist_id
         WHERE t.missing_since IS NULL AND ($1::uuid[] IS NULL OR t.album_id = ANY($1))
         ORDER BY t.path",
    )
    .bind(album_ids)
    .fetch_all(pool)
    .await
}

/// Paths of the present tracks outside these albums: a scoped run must leave their folders'
/// sidecars where they are.
pub async fn paths_outside(pool: &PgPool, album_ids: &[Uuid]) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT path FROM tracks WHERE missing_since IS NULL AND NOT (album_id = ANY($1))",
        album_ids
    )
    .fetch_all(pool)
    .await
}

/// Switches the library to a copy run's target: every done track row's track takes its
/// `to_path`, the run is stamped `switched_at`, and `system_settings.library_root` becomes the
/// target root, and the tag overrides of every autotag run that tagged this run's copies are
/// deleted. One transaction. Returns how many tracks were re-pointed.
pub async fn switch_root(pool: &PgPool, run_id: Uuid, target_root: &str) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    // Two steps through a unique placeholder, so a track taking a path another track is
    // about to leave cannot trip the unique index half-way.
    sqlx::query(
        "UPDATE tracks t SET path = 'kima-organizer-switch/' || j.id || '/' || j.to_path
         FROM organizer_journal j
         WHERE j.run_id = $1 AND j.state IN ('done', 'unchanged') AND j.track_id = t.id",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    let moved = sqlx::query(
        "UPDATE tracks t SET path = j.to_path, updated_at = now()
         FROM organizer_journal j
         WHERE j.run_id = $1 AND j.state IN ('done', 'unchanged') AND j.track_id = t.id",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("UPDATE organizer_runs SET switched_at = now() WHERE id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    // The tagged copies become the library: their tags are the truth now, so the overrides
    // that showed a copy-target match on the old library go (spec §19.9).
    crate::autotag::delete_copy_overrides(&mut tx, run_id).await?;
    crate::settings::set_library_root(&mut *tx, Some(target_root)).await?;
    tx.commit().await?;
    Ok(moved)
}

/// The whole-library copy an autotag `copy` apply writes into (spec §19.9): the newest
/// copy-mode run that is `done`, not switched to and not scoped.
pub async fn latest_copy_run(pool: &PgPool) -> sqlx::Result<Option<Run>> {
    sqlx::query_as!(
        Run,
        "SELECT id, mode, state, source_root, target_root, created_by, created_at, finished_at,
                switched_at, summary, error, album_ids, origin
         FROM organizer_runs
         WHERE mode = 'copy' AND state = 'done' AND switched_at IS NULL AND album_ids IS NULL
         ORDER BY created_at DESC, id DESC LIMIT 1"
    )
    .fetch_optional(pool)
    .await
}

/// The copy of each of `tracks` that `run` made, by track: the `to_path` (relative to the run's
/// target) of its `done` row. A track without one is not in the copy. An `unchanged` row (a copy
/// an earlier run made) does not count: tagging it would change the file the switch re-hashes
/// against the row's checksum, and the copy could then never become the library.
pub async fn copy_rows_for_tracks(
    pool: &PgPool,
    run_id: Uuid,
    tracks: &[Uuid],
) -> sqlx::Result<std::collections::HashMap<Uuid, String>> {
    let rows = sqlx::query!(
        r#"SELECT track_id AS "track_id!", to_path FROM organizer_journal
           WHERE run_id = $1 AND track_id = ANY($2)
             AND state = 'done'
           ORDER BY id"#,
        run_id,
        tracks
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.track_id, r.to_path)).collect())
}

/// How many rows of a run are `done` (the files it moved or copied).
pub async fn done_count(pool: &PgPool, run_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM organizer_journal WHERE run_id = $1 AND state = 'done'"#,
        run_id
    )
    .fetch_one(pool)
    .await
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
