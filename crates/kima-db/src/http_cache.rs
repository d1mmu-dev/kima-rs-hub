//! Outbound HTTP state (spec §6.2, §7.4): the response cache (`http_cache`) and
//! `source_pauses`, a source that rate-limited us. Both survive restarts.

use std::time::Duration;

use sqlx::PgPool;
use time::OffsetDateTime;

/// What a cached row says: a real answer or the source's "no such thing".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    Answered,
    Absent,
}

impl CacheOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheOutcome::Answered => "answered",
            CacheOutcome::Absent => "absent",
        }
    }
}

/// The unexpired row under `key`: its outcome and body.
pub async fn get(
    pool: &PgPool,
    key: &str,
) -> sqlx::Result<Option<(CacheOutcome, serde_json::Value)>> {
    let row = sqlx::query!(
        "SELECT outcome, body FROM http_cache WHERE key = $1 AND expires_at > now()",
        key
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| {
        let outcome = match r.outcome.as_str() {
            "answered" => CacheOutcome::Answered,
            "absent" => CacheOutcome::Absent,
            _ => return None,
        };
        Some((outcome, r.body))
    }))
}

/// Stores an answer or an absence under `key` for `ttl` from now, replacing any row.
/// `request` is the canonical request with secrets stripped (for debugging).
pub async fn put(
    pool: &PgPool,
    key: &str,
    source: &str,
    request: &str,
    outcome: CacheOutcome,
    body: &serde_json::Value,
    ttl: Duration,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"INSERT INTO http_cache (key, source, request, outcome, body, fetched_at, expires_at)
           VALUES ($1, $2, $3, $4, $5, now(), now() + make_interval(secs => $6))
           ON CONFLICT (key) DO UPDATE
           SET source = EXCLUDED.source,
               request = EXCLUDED.request,
               outcome = EXCLUDED.outcome,
               body = EXCLUDED.body,
               fetched_at = EXCLUDED.fetched_at,
               expires_at = EXCLUDED.expires_at"#,
        key,
        source,
        request,
        outcome.as_str(),
        body,
        ttl.as_secs_f64()
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes the expired rows; returns how many.
pub async fn prune_expired(pool: &PgPool) -> sqlx::Result<u64> {
    let done = sqlx::query!("DELETE FROM http_cache WHERE expires_at < now()")
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

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
