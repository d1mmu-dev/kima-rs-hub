//! HTTP read side of the map: the same eligible, current map rows feed map and summary.
use super::ELIGIBLE;
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, FromRow)]
pub struct MapRow {
    pub id: Uuid,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub colour: f64,
    pub title: String,
    pub artist: String,
    pub artist_id: Uuid,
    pub album_id: Uuid,
    pub has_cover: bool,
    pub valence: Option<f64>,
}

pub struct MapRead {
    pub rows: Vec<MapRow>,
    pub computed_at: Option<OffsetDateTime>,
}

/// Edited titles for queueable results (shared by the later search/similar/alchemy/path routes).
/// Existing unedited rows are present with `None`; unknown ids are absent.
pub async fn edited_titles(
    pool: &PgPool,
    ids: &[Uuid],
) -> sqlx::Result<std::collections::HashMap<Uuid, Option<String>>> {
    let rows: Vec<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, display_title FROM tracks WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// Whether a first layout can still be produced. Called only when the current map is empty.
pub async fn layout_pending(pool: &PgPool, sound_available: bool) -> sqlx::Result<bool> {
    let present: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tracks WHERE missing_since IS NULL)")
            .fetch_one(pool)
            .await?;
    if !present {
        return Ok(false);
    }
    if sound_available {
        return Ok(true);
    }
    sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM {})", *ELIGIBLE))
        .fetch_one(pool)
        .await
}

pub async fn read_map(pool: &PgPool) -> sqlx::Result<MapRead> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query_as(&format!(
        "WITH eligible AS (SELECT te.track_id, te.created_at FROM {})
         SELECT t.id, m.x, m.y, m.z, m.colour, t.title, ar.name AS artist,
                ar.id AS artist_id, a.id AS album_id, a.image_path IS NOT NULL AS has_cover,
                r.valence_raw::float8 AS valence
         FROM eligible e JOIN matching_map m ON m.track_id=e.track_id AND m.embedded_at=e.created_at
         JOIN tracks t ON t.id=e.track_id JOIN albums a ON a.id=t.album_id
         JOIN artists ar ON ar.id=a.artist_id
         LEFT JOIN track_clap_readings r ON r.track_id=e.track_id AND r.embedded_at=e.created_at
         ORDER BY t.id",
        *ELIGIBLE
    ))
    .fetch_all(&mut *tx)
    .await?;
    let computed_at = sqlx::query_scalar::<_, Option<OffsetDateTime>>(
        "SELECT map_updated_at FROM matching_state WHERE id='default'",
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    tx.commit().await?;
    Ok(MapRead { rows, computed_at })
}
