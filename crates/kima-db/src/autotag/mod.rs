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

use sqlx::PgPool;

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
