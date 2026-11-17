//! Text-to-audio nearest neighbours. Query vectors are already normalized by the text adapter.
use super::ELIGIBLE;
use crate::PgPool;
use sqlx::FromRow;
use uuid::Uuid;

pub fn ef_for_limit(limit: usize) -> usize {
    limit.saturating_mul(2).clamp(100, 1000)
}

#[derive(Debug, FromRow)]
pub struct SearchTrack {
    pub id: Uuid,
    pub title: String,
    pub duration: i32,
    pub track_no: i32,
    pub distance: f64,
    pub album_id: Uuid,
    pub album_title: String,
    pub album_has_cover: bool,
    pub artist_id: Uuid,
    pub artist_name: String,
}

fn searchable() -> String {
    // Keep the one eligibility predicate; join metadata before the LIMIT in either path.
    format!(
        "SELECT t.id,t.title,t.duration_secs AS duration,t.track_no,
        t.embedding <=> $1::real[]::vector AS distance,
        al.id AS album_id,al.title AS album_title,(al.image_path IS NOT NULL) AS album_has_cover,
        ar.id AS artist_id,ar.name AS artist_name
        FROM (SELECT t.*,te.embedding FROM {}) t
        JOIN albums al ON al.id=t.album_id JOIN artists ar ON ar.id=al.artist_id",
        *ELIGIBLE
    )
}

pub async fn search_tracks_by_embedding(
    pool: &PgPool,
    vector: &[f32],
    limit: usize,
    exact_max: u64,
) -> sqlx::Result<Vec<SearchTrack>> {
    if vector.len() != 512
        || vector.iter().any(|v| !v.is_finite())
        || !vector.iter().any(|v| *v != 0.0)
    {
        return Err(sqlx::Error::Protocol("invalid text embedding".into()));
    }
    let limit =
        i64::try_from(limit).map_err(|_| sqlx::Error::Protocol("search limit overflow".into()))?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut tx = pool.begin().await?;
    // Stable eligibility/count while deciding and executing the policy, no pool-global GUCs.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}", *ELIGIBLE))
        .fetch_one(&mut *tx)
        .await?;
    let base = searchable();
    let exact = format!("{base} ORDER BY (t.embedding <=> $1::real[]::vector) + 0,t.id LIMIT $2");
    let indexed = count as u64 > exact_max;
    let mut rows = if indexed {
        sqlx::query("SELECT set_config('hnsw.ef_search',$1,true),set_config('hnsw.iterative_scan','relaxed_order',true),set_config('hnsw.max_scan_tuples','40000',true)")
            .bind(ef_for_limit(limit as usize).to_string()).execute(&mut *tx).await?;
        sqlx::query_as::<_,SearchTrack>(&format!("WITH nearest AS MATERIALIZED ({base} ORDER BY t.embedding <=> $1::real[]::vector LIMIT $2) SELECT * FROM nearest ORDER BY distance + 0,id"))
            .bind(vector).bind(limit).fetch_all(&mut *tx).await?
    } else {
        Vec::new()
    };
    if !indexed || rows.len() < limit as usize {
        rows = sqlx::query_as::<_, SearchTrack>(&exact)
            .bind(vector)
            .bind(limit)
            .fetch_all(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(rows)
}
