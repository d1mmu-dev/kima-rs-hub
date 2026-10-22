use std::time::Duration;

use sqlx::PgPool;
use time::OffsetDateTime;
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

/// Adds a job, due now, and wakes the workers listening on [`NOTIFY_CHANNEL`] (on commit).
pub async fn enqueue(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
) -> sqlx::Result<Uuid> {
    insert(pool, queue, payload, max_attempts, None).await
}

/// Adds a job that is not claimed before `run_at`. Wakes the workers only when it is already
/// due; a later job is found by their poll (within `poll_interval` of `run_at`).
pub async fn enqueue_at(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    run_at: OffsetDateTime,
) -> sqlx::Result<Uuid> {
    insert(pool, queue, payload, max_attempts, Some(run_at)).await
}

/// Inserts a job due at `run_at`, or at the database's `now()` when `None`: the app's clock
/// may differ from the database's, and a "due now" job must be claimable and notified at once.
async fn insert(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    run_at: Option<OffsetDateTime>,
) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    let due = sqlx::query_scalar!(
        r#"INSERT INTO jobs (id, queue, payload, max_attempts, run_at)
           VALUES ($1, $2, $3, $4, COALESCE($5, now()))
           RETURNING run_at <= now() AS "due!""#,
        id,
        queue,
        payload,
        max_attempts,
        run_at
    )
    .fetch_one(&mut *tx)
    .await?;
    if due {
        notify(&mut tx, queue).await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// Wakes the workers of `queue` when `tx` commits.
async fn notify(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, queue: &str) -> sqlx::Result<()> {
    // `pg_notify` returns `void`, which the query macros cannot type; hence a plain query.
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_CHANNEL)
        .bind(queue)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Moves the oldest `queued` job on `queue` whose payload `kind` is `kind` to `run_at`, waking
/// the workers if it is now due. Returns `false` when no such job is waiting.
pub async fn reschedule_queued(
    pool: &PgPool,
    queue: &str,
    kind: &str,
    run_at: OffsetDateTime,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    // `state = 'queued'` is repeated outside the subquery: if a worker claims the row while we
    // wait for its lock, the re-check on the updated row skips it.
    let due = sqlx::query_scalar!(
        r#"UPDATE jobs SET run_at = $3
           WHERE state = 'queued'
             AND id = (SELECT id FROM jobs
                       WHERE queue = $1 AND state = 'queued' AND payload ->> 'kind' = $2
                       ORDER BY created_at, id
                       LIMIT 1
                       FOR UPDATE)
           RETURNING run_at <= now() AS "due!""#,
        queue,
        kind,
        run_at
    )
    .fetch_optional(&mut *tx)
    .await?;
    if due == Some(true) {
        notify(&mut tx, queue).await?;
    }
    tx.commit().await?;
    Ok(due.is_some())
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
                 AND ((state = 'queued' AND run_at <= now() AND attempts < max_attempts)
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

/// Fails the claimed attempt for good, whatever attempts remain (a non-retryable error).
/// Returns `false` if the job is no longer ours, in which case nothing changes.
pub async fn fail_permanent(pool: &PgPool, job: &Job, error: &str) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'failed', locked_until = NULL, last_error = $2, finished_at = now()
         WHERE id = $1 AND state = 'running' AND attempts = $3",
        job.id,
        error,
        job.attempts
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Extends the claimed attempt's lease to `lease` from now (the worker's heartbeat). Returns
/// `false` if the claim is lost (the job was reaped, reclaimed or reset), changing nothing.
pub async fn renew(pool: &PgPool, job: &Job, lease: Duration) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET locked_until = now() + make_interval(secs => $3)
         WHERE id = $1 AND state = 'running' AND attempts = $2",
        job.id,
        job.attempts,
        lease.as_secs_f64()
    )
    .execute(pool)
    .await?;
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

/// Puts every `running` job on `queue` back to `queued`, due now, keeping its attempt count.
/// Call at startup, before any worker starts: jobs left `running` belong to a process that
/// died, and would otherwise wait out their lease.
///
/// Assumes a single kima process owns the queue (true for M1; running several instances
/// against one database is a non-goal). With several processes this would steal live jobs.
///
/// A job already on its final attempt is failed instead of requeued: if it is what killed
/// the process, requeueing it would crash kima again on every restart (a poison pill).
pub async fn requeue_running(pool: &PgPool, queue: &str) -> sqlx::Result<Recovered> {
    let mut tx = pool.begin().await?;
    let failed = sqlx::query!(
        "UPDATE jobs SET state = 'failed', locked_until = NULL, finished_at = now(),
                         last_error = 'process died on the final attempt'
         WHERE queue = $1 AND state = 'running' AND attempts >= max_attempts",
        queue
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let requeued = sqlx::query!(
        "UPDATE jobs SET state = 'queued', locked_until = NULL, run_at = now()
         WHERE queue = $1 AND state = 'running' AND attempts < max_attempts",
        queue
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(Recovered { requeued, failed })
}

/// What [`requeue_running`] did with the jobs it found running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Recovered {
    /// Put back in the queue, due now.
    pub requeued: u64,
    /// Out of attempts; failed with "process died on the final attempt".
    pub failed: u64,
}

/// Delay before retrying after attempt `attempts`: 5 s, 10 s, 20 s, ... at most 1 h.
pub fn backoff(attempts: i32) -> Duration {
    let exponent = (attempts.max(1) - 1).min(20) as u32;
    Duration::from_secs(5u64.saturating_mul(1 << exponent).min(3600))
}

/// Like [`enqueue`], unless a job of the same `kind` (the payload's `kind` field) is already
/// queued or running on `queue`. Returns the new job's id, or `None` when one is pending.
///
/// The check and insert are one statement, serialised per queue by a transaction-scoped
/// advisory lock, so concurrent callers cannot both insert (a bare `INSERT ... WHERE NOT
/// EXISTS` can, under READ COMMITTED).
pub async fn enqueue_unless_pending(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
) -> sqlx::Result<Option<Uuid>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('kima_jobs_enqueue:' || $1))")
        .bind(queue)
        .execute(&mut *tx)
        .await?;
    let id = sqlx::query_scalar!(
        r#"INSERT INTO jobs (id, queue, payload, max_attempts)
           SELECT $1::uuid, $2::text, $3::jsonb, $4::int4
           WHERE NOT EXISTS (SELECT 1 FROM jobs
                             WHERE queue = $2 AND state IN ('queued', 'running')
                               AND payload ->> 'kind' IS NOT DISTINCT FROM $3::jsonb ->> 'kind')
           RETURNING id"#,
        Uuid::now_v7(),
        queue,
        payload,
        max_attempts
    )
    .fetch_optional(&mut *tx)
    .await?;
    if id.is_some() {
        notify(&mut tx, queue).await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// The id of a queued or running job on `queue` whose payload `kind` is `kind`, oldest first.
pub async fn pending_id(pool: &PgPool, queue: &str, kind: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM jobs
         WHERE queue = $1 AND state IN ('queued', 'running') AND payload ->> 'kind' = $2
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(queue)
    .bind(kind)
    .fetch_optional(pool)
    .await
}

/// Like [`enqueue_unless_pending`], but always returns a job id: the new job's, or the one
/// already pending. Retries if the pending job finishes between the two steps.
pub async fn enqueue_or_pending(
    pool: &PgPool,
    queue: &str,
    kind: &str,
    payload: serde_json::Value,
    max_attempts: i32,
) -> sqlx::Result<Uuid> {
    loop {
        if let Some(id) = enqueue_unless_pending(pool, queue, payload.clone(), max_attempts).await?
        {
            return Ok(id);
        }
        if let Some(id) = pending_id(pool, queue, kind).await? {
            return Ok(id);
        }
    }
}

/// Like [`enqueue`], unless a job still waiting (`queued`, not running) on `queue` has the same
/// text in its payload's `key` field. A running job may already have read stale input, so it
/// does not count. Returns the new id, or `None` for a duplicate. Serialised per queue.
pub async fn enqueue_unless_queued_with_key(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    key: &str,
) -> sqlx::Result<Option<Uuid>> {
    insert_unless_queued_with_key(pool, queue, payload, max_attempts, key, RunAt::Now).await
}

/// Like [`enqueue_unless_queued_with_key`], for a job not claimed before `run_at`. A queued
/// duplicate counts whatever its own `run_at`. Wakes the workers only when the new job is due.
pub async fn enqueue_at_unless_queued_with_key(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    key: &str,
    run_at: OffsetDateTime,
) -> sqlx::Result<Option<Uuid>> {
    insert_unless_queued_with_key(pool, queue, payload, max_attempts, key, RunAt::At(run_at)).await
}

/// Like [`enqueue_at_unless_queued_with_key`], due `delay` after the database's `now()`: the
/// database clock is the one claims compare `run_at` with.
pub async fn enqueue_in_unless_queued_with_key(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    key: &str,
    delay: Duration,
) -> sqlx::Result<Option<Uuid>> {
    insert_unless_queued_with_key(pool, queue, payload, max_attempts, key, RunAt::In(delay)).await
}

/// When a job inserted by [`insert_unless_queued_with_key`] is due.
enum RunAt {
    /// The database's `now()`.
    Now,
    At(OffsetDateTime),
    /// The database's `now()` plus this.
    In(Duration),
}

/// The body of the `*_unless_queued_with_key` calls.
async fn insert_unless_queued_with_key(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    key: &str,
    run_at: RunAt,
) -> sqlx::Result<Option<Uuid>> {
    let (at, delay) = match run_at {
        RunAt::Now => (None, Duration::ZERO),
        RunAt::At(t) => (Some(t), Duration::ZERO),
        RunAt::In(d) => (None, d),
    };
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('kima_jobs_enqueue:' || $1))")
        .bind(queue)
        .execute(&mut *tx)
        .await?;
    let inserted = sqlx::query!(
        r#"INSERT INTO jobs (id, queue, payload, max_attempts, run_at)
           SELECT $1::uuid, $2::text, $3::jsonb, $4::int4,
                  COALESCE($6::timestamptz, now() + make_interval(secs => $7::float8))
           WHERE NOT EXISTS (SELECT 1 FROM jobs
                             WHERE queue = $2 AND state = 'queued'
                               AND payload ->> $5::text IS NOT DISTINCT FROM $3::jsonb ->> $5::text)
           RETURNING id, run_at <= now() AS "due!""#,
        Uuid::now_v7(),
        queue,
        payload,
        max_attempts,
        key,
        at,
        delay.as_secs_f64()
    )
    .fetch_optional(&mut *tx)
    .await?;
    if inserted.as_ref().is_some_and(|r| r.due) {
        notify(&mut tx, queue).await?;
    }
    tx.commit().await?;
    Ok(inserted.map(|r| r.id))
}

/// Makes a job with the same payload `key` text as `payload` due now: the oldest one still
/// `queued` on `queue` is moved to the database's `now()` (never later than it was), else a new
/// one is inserted, due now. One transaction under the same per-queue lock as the
/// `*_unless_queued_with_key` inserts, so concurrent callers leave exactly one queued job, and
/// a job queued for later by someone else cannot slip in between the two steps. Returns the
/// job's id; wakes the workers.
pub async fn advance_or_enqueue_with_key(
    pool: &PgPool,
    queue: &str,
    payload: serde_json::Value,
    max_attempts: i32,
    key: &str,
) -> sqlx::Result<Uuid> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('kima_jobs_enqueue:' || $1))")
        .bind(queue)
        .execute(&mut *tx)
        .await?;
    let moved = sqlx::query_scalar!(
        r#"UPDATE jobs SET run_at = LEAST(run_at, now())
           WHERE state = 'queued'
             AND id = (SELECT id FROM jobs
                       WHERE queue = $1 AND state = 'queued'
                         AND payload ->> $2::text IS NOT DISTINCT FROM $3::jsonb ->> $2::text
                       ORDER BY created_at, id
                       LIMIT 1
                       FOR UPDATE)
           RETURNING id"#,
        queue,
        key,
        payload
    )
    .fetch_optional(&mut *tx)
    .await?;
    let id = match moved {
        Some(id) => id,
        None => {
            sqlx::query_scalar!(
                "INSERT INTO jobs (id, queue, payload, max_attempts) VALUES ($1, $2, $3, $4)
                 RETURNING id",
                Uuid::now_v7(),
                queue,
                payload,
                max_attempts
            )
            .fetch_one(&mut *tx)
            .await?
        }
    };
    notify(&mut tx, queue).await?;
    tx.commit().await?;
    Ok(id)
}

/// Gives a claimed attempt back: the job returns to `queued`, due now, with the attempt it was
/// claimed with uncounted. For a job that stopped only because the process is shutting down,
/// so a restart does not use up its attempts. Returns `false` if the job is no longer ours.
pub async fn release(pool: &PgPool, job: &Job) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE jobs SET state = 'queued', locked_until = NULL, run_at = now(),
                         attempts = GREATEST(attempts - 1, 0)
         WHERE id = $1 AND state = 'running' AND attempts = $2",
        job.id,
        job.attempts
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}
