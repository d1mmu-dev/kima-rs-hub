use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

pub const NOTIFY_CHANNEL: &str = "kima_jobs";

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: Uuid,
    pub queue: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// Adds a job and wakes the workers listening on [`NOTIFY_CHANNEL`] (on commit).
pub async fn enqueue(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "INSERT INTO jobs (id, queue, payload, max_attempts) VALUES ($1, $2, $3, $4)",
        id,
        queue,
        payload,
        max_attempts
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_CHANNEL)
        .bind(queue)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// Takes up to `limit` due jobs, including ones whose worker's lease ran out.
pub async fn claim(
    pool: &PgPool,
    queue: &str,
    limit: i64,
    lease: Duration,
) -> sqlx::Result<Vec<Job>> {
    sqlx::query_as!(
        Job,
        r#"UPDATE jobs
           SET state = 'running',
               attempts = attempts + 1,
               locked_until = now() + make_interval(secs => $3)
           WHERE id IN (
               SELECT id FROM jobs
               WHERE queue = $1
                 AND ((state = 'queued' AND run_at <= now())
                      OR (state = 'running' AND locked_until < now() AND attempts < max_attempts))
               ORDER BY run_at, id
               LIMIT $2
               FOR UPDATE SKIP LOCKED)
           RETURNING id, queue, payload, attempts, max_attempts"#,
        queue,
        limit,
        lease.as_secs_f64()
    )
    .fetch_all(pool)
    .await
}

/// Marks the claimed attempt done. Returns `false` if the job is no longer ours (its lease
/// expired and it was reclaimed, reaped, or finished elsewhere), in which case nothing changes.
pub async fn complete(pool: &PgPool, job: &Job) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'done', locked_until = NULL, last_error = NULL, finished_at = now()
         WHERE id = $1 AND state = 'running' AND attempts = $2",
        job.id,
        job.attempts
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Records a failed attempt: retried after [`backoff`], or failed for good on the last attempt.
/// Returns `false` if the job is no longer ours, in which case nothing changes.
pub async fn fail(pool: &PgPool, job: &Job, error: &str) -> sqlx::Result<bool> {
    let result = if job.attempts >= job.max_attempts {
        sqlx::query!(
            "UPDATE jobs SET state = 'failed', locked_until = NULL, last_error = $2, finished_at = now()
             WHERE id = $1 AND state = 'running' AND attempts = $3",
            job.id,
            error,
            job.attempts
        )
        .execute(pool)
        .await?
    } else {
        sqlx::query!(
            "UPDATE jobs SET state = 'queued', locked_until = NULL, last_error = $2,
                             run_at = now() + make_interval(secs => $3)
             WHERE id = $1 AND state = 'running' AND attempts = $4",
            job.id,
            error,
            backoff(job.attempts).as_secs_f64(),
            job.attempts
        )
        .execute(pool)
        .await?
    };
    Ok(result.rows_affected() == 1)
}

/// Fails jobs whose lease ran out on their final attempt (their worker died).
pub async fn reap(pool: &PgPool) -> sqlx::Result<u64> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'failed', locked_until = NULL, finished_at = now(),
                         last_error = 'lease expired on the final attempt'
         WHERE state = 'running' AND locked_until < now() AND attempts >= max_attempts"
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delay before retrying after attempt `attempts`: 5 s, 10 s, 20 s, ... at most 1 h.
pub fn backoff(attempts: i32) -> Duration {
    let exponent = (attempts.max(1) - 1).min(20) as u32;
    Duration::from_secs(5u64.saturating_mul(1 << exponent).min(3600))
}
