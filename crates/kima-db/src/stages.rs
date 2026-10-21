//! `track_stages` writes made by the scanner (spec §8.1, §10.4).

use sqlx::PgConnection;
use uuid::Uuid;

/// A changed file (the scanner's "changed" class): every stage row back to `pending` with a
/// clean slate and `evidence = {"reason":"file changed"}`, and the stored AcoustID
/// fingerprint deleted, since the audio may have changed (upstream
/// `reopenStagesForChangedFile`).
pub async fn reopen_for_changed_file(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE track_stages
           SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
               last_error = NULL, error_code = NULL, error_class = NULL,
               evidence = '{"reason":"file changed"}'::jsonb,
               started_at = NULL, finished_at = NULL, updated_at = now()
           WHERE track_id = $1"#,
        track
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("DELETE FROM track_acoustid WHERE track_id = $1", track)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The track's file is back (restored or re-pointed): its `FILE_MISSING` rows go back to
/// `pending`. Other rows are untouched.
pub async fn reopen_file_missing(conn: &mut PgConnection, track: Uuid) -> sqlx::Result<()> {
    reopen_file_missing_many(conn, &[track]).await.map(|_| ())
}

/// [`reopen_file_missing`] for several tracks in one statement. Returns rows changed.
pub async fn reopen_file_missing_many(
    conn: &mut PgConnection,
    tracks: &[Uuid],
) -> sqlx::Result<u64> {
    if tracks.is_empty() {
        return Ok(0);
    }
    let r = sqlx::query!(
        "UPDATE track_stages
         SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
             last_error = NULL, error_code = NULL, error_class = NULL,
             started_at = NULL, finished_at = NULL, updated_at = now()
         WHERE track_id = ANY($1) AND error_class = 'FILE_MISSING'",
        tracks
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}
