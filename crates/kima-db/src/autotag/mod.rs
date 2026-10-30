//! The autotagger's data (spec §6.8, §19): per-album search state, stored candidates, the
//! local album as the matcher sees it and the values an apply writes.

pub mod albums;
pub mod applied;
pub mod identity;
pub mod journal;
pub mod runs;

pub use albums::*;
pub use applied::*;
pub use identity::*;
pub use runs::*;

use std::collections::HashMap;

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// The stored tag overrides (`track_tag_overrides.fields`, spec §6.8, §19.7) of the tracks at
/// these paths, by path. The scanner merges them over the file's tags on every read.
pub async fn overrides_for_paths(
    pool: &PgPool,
    paths: &[String],
) -> sqlx::Result<HashMap<String, serde_json::Value>> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query!(
        "SELECT t.path, o.fields FROM track_tag_overrides o
         JOIN tracks t ON t.id = o.track_id
         WHERE t.path = ANY($1)",
        paths
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.path, r.fields)).collect())
}

/// The missing track a new file with this content fingerprint would take over (the scanner's
/// re-point rule: the earliest missing, then the smallest id), skipping `claimed`, and that
/// track's stored override if it has one. Lets the scanner merge a moved or renamed file's
/// override before it resolves the file (spec §19.7).
pub async fn override_by_missing_fingerprint(
    pool: &PgPool,
    fingerprint: &[u8],
    claimed: &[uuid::Uuid],
) -> sqlx::Result<Option<(uuid::Uuid, Option<serde_json::Value>)>> {
    let row = sqlx::query!(
        r#"SELECT t.id, o.fields AS "fields?" FROM tracks t
           LEFT JOIN track_tag_overrides o ON o.track_id = t.id
           WHERE t.fingerprint = $1 AND t.missing_since IS NOT NULL AND NOT (t.id = ANY($2))
           ORDER BY t.missing_since, t.id LIMIT 1"#,
        fingerprint,
        claimed
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.id, r.fields)))
}

/// Deletes the tag overrides of every autotag run that tagged the copies of `copy_run`
/// (`autotag_runs.copy_run_id`), in the caller's transaction: run by the organizer's
/// `switch_root`, after which the tagged copies are the library and their tags the truth
/// (spec §19.9). Answers how many were deleted.
pub async fn delete_copy_overrides(conn: &mut PgConnection, copy_run: Uuid) -> sqlx::Result<u64> {
    let r = sqlx::query!(
        "DELETE FROM track_tag_overrides o USING autotag_runs r
         WHERE o.run_id = r.id AND r.copy_run_id = $1",
        copy_run
    )
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// Whether an autotag run that writes into the copies of `copy_run` is still working
/// (`writing`, `organizing`, `undoing`): the copy must not become the library under it.
pub async fn copy_run_in_use(pool: &PgPool, copy_run: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM autotag_runs
             WHERE copy_run_id = $1 AND state IN ('writing', 'organizing', 'undoing')) AS "e!""#,
        copy_run
    )
    .fetch_one(pool)
    .await
}
