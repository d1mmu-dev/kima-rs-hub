//! `autotag_journal` (spec §6.8, §19.9): one row per file an apply writes, committed before any
//! file is touched, so an interrupted apply resumes (and an undo restores) from it.
//!
//! A row holds library-relative paths, hashes and tag values only, never a secret.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// A row about to be journalled (`planned`).
#[derive(Debug, Clone, PartialEq)]
pub struct NewJournalRow {
    pub track_id: Option<Uuid>,
    /// `library` or `copy`.
    pub root: String,
    /// Relative to that root.
    pub path: String,
    /// SHA-256 of the whole file before the write.
    pub before_sha: Option<Vec<u8>>,
    pub audio_md5: Option<Vec<u8>>,
    /// Native key → its current values (`[]` = absent), for every touched key.
    pub old_tags: Value,
    pub new_tags: Value,
    /// `v3` or `v4` for an MP3 with an ID3v2 tag; `None` otherwise.
    pub id3_version: Option<String>,
}

/// Every `autotag_journal` column.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalRow {
    pub id: i64,
    pub run_id: Uuid,
    pub track_id: Option<Uuid>,
    pub root: String,
    pub path: String,
    pub before_sha: Option<Vec<u8>>,
    pub after_sha: Option<Vec<u8>>,
    pub audio_md5: Option<Vec<u8>>,
    pub old_tags: Value,
    pub new_tags: Value,
    pub id3_version: Option<String>,
    pub state: String,
    pub note: Option<String>,
    pub applied_seq: Option<i64>,
}

/// Inserts `rows` as `planned`, in order; answers their ids.
pub async fn insert_planned(
    conn: &mut PgConnection,
    run: Uuid,
    rows: &[NewJournalRow],
) -> sqlx::Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(rows.len());
    for r in rows {
        let id = sqlx::query_scalar!(
            "INSERT INTO autotag_journal (run_id, track_id, root, path, before_sha, audio_md5,
                                          old_tags, new_tags, id3_version, state)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'planned')
             RETURNING id",
            run,
            r.track_id,
            r.root,
            r.path,
            r.before_sha.as_deref(),
            r.audio_md5.as_deref(),
            r.old_tags,
            r.new_tags,
            r.id3_version
        )
        .fetch_one(&mut *conn)
        .await?;
        ids.push(id);
    }
    Ok(ids)
}

/// A `planned` row becomes `written`, with the written file's hash and the next `applied_seq`
/// (undo goes in reverse of it). A row in any other state is left alone, so a repeated call
/// never takes a second number.
pub async fn mark_written(
    conn: &mut PgConnection,
    row_id: i64,
    after_sha: &[u8],
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE autotag_journal
         SET state = 'written', after_sha = $2, note = NULL,
             applied_seq = nextval('autotag_applied_seq')
         WHERE id = $1 AND state = 'planned'",
        row_id,
        after_sha
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Sets a row's state and note (`skipped`, `failed`, `undone`, `undo_failed`).
pub async fn set_row_state(
    pool: &PgPool,
    row_id: i64,
    state: &str,
    note: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE autotag_journal SET state = $2, note = $3 WHERE id = $1",
        row_id,
        state,
        note
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The run's rows, in journal order.
pub async fn rows(pool: &PgPool, run_id: Uuid) -> sqlx::Result<Vec<JournalRow>> {
    sqlx::query_as!(
        JournalRow,
        "SELECT id, run_id, track_id, root, path, before_sha, after_sha, audio_md5, old_tags,
                new_tags, id3_version, state, note, applied_seq
         FROM autotag_journal WHERE run_id = $1 ORDER BY id",
        run_id
    )
    .fetch_all(pool)
    .await
}

/// The run's `written` rows, newest write first: the order undo restores them in.
pub async fn written_desc(pool: &PgPool, run_id: Uuid) -> sqlx::Result<Vec<JournalRow>> {
    sqlx::query_as!(
        JournalRow,
        "SELECT id, run_id, track_id, root, path, before_sha, after_sha, audio_md5, old_tags,
                new_tags, id3_version, state, note, applied_seq
         FROM autotag_journal WHERE run_id = $1 AND state = 'written'
         ORDER BY applied_seq DESC, id DESC",
        run_id
    )
    .fetch_all(pool)
    .await
}

/// The rows an undo restores (`written`, and `undo_failed` ones retried), newest write first.
pub async fn undo_rows_desc(pool: &PgPool, run_id: Uuid) -> sqlx::Result<Vec<JournalRow>> {
    sqlx::query_as!(
        JournalRow,
        "SELECT id, run_id, track_id, root, path, before_sha, after_sha, audio_md5, old_tags,
                new_tags, id3_version, state, note, applied_seq
         FROM autotag_journal WHERE run_id = $1 AND state IN ('written', 'undo_failed')
         ORDER BY applied_seq DESC, id DESC",
        run_id
    )
    .fetch_all(pool)
    .await
}
