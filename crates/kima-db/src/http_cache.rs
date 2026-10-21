//! Outbound HTTP state (spec §6.2): `source_pauses`, a source that rate-limited us, kept across
//! restarts. The response cache (`http_cache`) arrives with the cache clauses of D-109.

use sqlx::PgPool;
use time::OffsetDateTime;

/// Pauses `source` until `until`, counting one more consecutive rate-limited outcome. Returns
/// the new count (1 for a source with no row).
pub async fn record_pause(
    pool: &PgPool,
    source: &str,
    until: OffsetDateTime,
    reason: &str,
) -> sqlx::Result<i32> {
    sqlx::query_scalar!(
        r#"INSERT INTO source_pauses (source, paused_until, reason) VALUES ($1, $2, $3)
           ON CONFLICT (source) DO UPDATE
           SET paused_until = EXCLUDED.paused_until,
               reason = EXCLUDED.reason,
               consecutive = source_pauses.consecutive + 1
           RETURNING consecutive"#,
        source,
        until,
        reason
    )
    .fetch_one(pool)
    .await
}

/// Forgets a source's pause and its consecutive count (it answered).
pub async fn clear_pause(pool: &PgPool, source: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM source_pauses WHERE source = $1", source)
        .execute(pool)
        .await?;
    Ok(())
}

/// Every pause row, expired or not: `(source, paused_until, consecutive)`.
pub async fn pauses(pool: &PgPool) -> sqlx::Result<Vec<(String, OffsetDateTime, i32)>> {
    let rows =
        sqlx::query!("SELECT source, paused_until, consecutive FROM source_pauses ORDER BY source")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.source, r.paused_until, r.consecutive))
        .collect())
}

/// Moves an existing pause's end without counting another outcome.
pub async fn set_pause_until(
    pool: &PgPool,
    source: &str,
    until: OffsetDateTime,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE source_pauses SET paused_until = $2 WHERE source = $1",
        source,
        until
    )
    .execute(pool)
    .await?;
    Ok(())
}
