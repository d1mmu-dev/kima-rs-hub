//! The autotagger's data (spec §6.8, §19): per-album search state, stored candidates, the
//! local album as the matcher sees it and the values an apply writes.

pub mod albums;
pub mod applied;

pub use albums::*;
pub use applied::*;

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
