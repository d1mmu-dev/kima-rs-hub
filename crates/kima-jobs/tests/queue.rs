use std::time::Duration;

use kima_db::PgPool;
use kima_jobs::queue::{self, backoff};
use serde_json::json;

const LEASE: Duration = Duration::from_secs(60);

async fn state_of(pool: &PgPool, id: uuid::Uuid) -> (String, i32, Option<String>, bool) {
    let row: (String, i32, Option<String>, bool) = sqlx::query_as(
        "SELECT state, attempts, last_error, run_at > now() FROM jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    row
}

#[test]
fn backoff_doubles_from_five_seconds_and_caps_at_an_hour() {
    assert_eq!(backoff(1), Duration::from_secs(5));
    assert_eq!(backoff(2), Duration::from_secs(10));
    assert_eq!(backoff(3), Duration::from_secs(20));
    assert_eq!(backoff(20), Duration::from_secs(3600));
    assert_eq!(backoff(i32::MAX), Duration::from_secs(3600));
    assert_eq!(backoff(0), Duration::from_secs(5));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claimed_job_is_not_claimed_twice(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({"path": "/music"}), 3)
        .await
        .unwrap();
    let jobs = queue::claim(&pool, "scan", 10, LEASE).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, id);
    assert_eq!(jobs[0].payload, json!({"path": "/music"}));
    assert_eq!(jobs[0].attempts, 1);
    assert!(
        queue::claim(&pool, "scan", 10, LEASE)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn queues_are_separate(pool: PgPool) {
    queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    assert!(
        queue::claim(&pool, "other", 10, LEASE)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_claims_split_the_work(pool: PgPool) {
    for i in 0..10 {
        queue::enqueue(&pool, "scan", json!({ "i": i }), 3)
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        queue::claim(&pool, "scan", 10, LEASE),
        queue::claim(&pool, "scan", 10, LEASE)
    );
    let mut ids: Vec<_> = a
        .unwrap()
        .into_iter()
        .chain(b.unwrap())
        .map(|j| j.id)
        .collect();
    assert_eq!(ids.len(), 10);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 10, "no job handed out twice");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn complete_marks_done(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    let job = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    assert!(queue::complete(&pool, &job).await.unwrap());
    assert_eq!(state_of(&pool, id).await.0, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failure_before_last_attempt_requeues_later(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    let job = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    queue::fail(&pool, &job, "boom").await.unwrap();
    let (state, attempts, error, delayed) = state_of(&pool, id).await;
    assert_eq!(
        (state.as_str(), attempts, error.as_deref(), delayed),
        ("queued", 1, Some("boom"), true)
    );
    assert!(
        queue::claim(&pool, "scan", 1, LEASE)
            .await
            .unwrap()
            .is_empty(),
        "backoff not over"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failure_on_last_attempt_is_final(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 1).await.unwrap();
    let job = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    queue::fail(&pool, &job, "boom").await.unwrap();
    assert_eq!(state_of(&pool, id).await.0, "failed");
}

/// Review focus 5: a worker that died mid-job.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn expired_lease_is_reclaimed(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    queue::claim(&pool, "scan", 1, LEASE).await.unwrap();
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let again = queue::claim(&pool, "scan", 1, LEASE).await.unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].attempts, 2);
}

/// Review focus 5: ... but not beyond max_attempts.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn expired_lease_on_last_attempt_is_reaped_not_reclaimed(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 1).await.unwrap();
    queue::claim(&pool, "scan", 1, LEASE).await.unwrap();
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        queue::claim(&pool, "scan", 1, LEASE)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(queue::reap(&pool).await.unwrap(), 1);
    let (state, _, error, _) = state_of(&pool, id).await;
    assert_eq!(state, "failed");
    assert_eq!(error.as_deref(), Some("lease expired on the final attempt"));
}

/// Review focus 5, fix round 1: a stale worker must not touch a job another worker now owns.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_complete_after_reclaim_is_ignored(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    let stale = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let fresh = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(fresh.attempts, 2);

    assert!(!queue::complete(&pool, &stale).await.unwrap());
    assert_eq!(state_of(&pool, id).await.0, "running");

    assert!(queue::complete(&pool, &fresh).await.unwrap());
    assert_eq!(state_of(&pool, id).await.0, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_complete_after_reap_is_ignored(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 1).await.unwrap();
    let stale = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(queue::reap(&pool).await.unwrap(), 1);

    assert!(!queue::complete(&pool, &stale).await.unwrap());
    assert_eq!(state_of(&pool, id).await.0, "failed");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_fail_is_ignored(pool: PgPool) {
    let id = queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    let stale = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let fresh = queue::claim(&pool, "scan", 1, LEASE)
        .await
        .unwrap()
        .remove(0);

    assert!(!queue::fail(&pool, &stale, "stale boom").await.unwrap());
    let (state, attempts, error, _) = state_of(&pool, id).await;
    assert_eq!((state.as_str(), attempts, error), ("running", 2, None));

    assert!(queue::fail(&pool, &fresh, "fresh boom").await.unwrap());
    assert_eq!(state_of(&pool, id).await.0, "queued");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requeue_running_recovers_jobs_of_a_dead_process(pool: PgPool) {
    let stuck = queue::enqueue(&pool, "q", json!({"kind": "x"}), 3)
        .await
        .unwrap();
    queue::claim(&pool, "q", 1, LEASE).await.unwrap();
    let waiting = queue::enqueue(&pool, "q", json!({"kind": "y"}), 3)
        .await
        .unwrap();
    let other = queue::enqueue(&pool, "other", json!({}), 3).await.unwrap();
    queue::claim(&pool, "other", 1, LEASE).await.unwrap();

    assert_eq!(
        queue::requeue_running(&pool, "q").await.unwrap(),
        queue::Recovered {
            requeued: 1,
            failed: 0
        }
    );
    let (state, attempts, _, delayed) = state_of(&pool, stuck).await;
    assert_eq!((state.as_str(), attempts, delayed), ("queued", 1, false));
    let unlocked: bool = sqlx::query_scalar("SELECT locked_until IS NULL FROM jobs WHERE id = $1")
        .bind(stuck)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(unlocked);
    assert_eq!(state_of(&pool, waiting).await.0, "queued");
    // Other queues are left alone.
    assert_eq!(state_of(&pool, other).await.0, "running");
    // And it can be claimed straight away, as its next attempt.
    let again = queue::claim(&pool, "q", 10, LEASE).await.unwrap();
    let job = again.iter().find(|j| j.id == stuck).unwrap();
    assert_eq!(job.attempts, 2);
}

/// A job that kills the process on every run must not be retried on every restart.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requeue_running_fails_exhausted_jobs(pool: PgPool) {
    let poison = queue::enqueue(&pool, "q", json!({"kind": "poison"}), 1)
        .await
        .unwrap();
    let retryable = queue::enqueue(&pool, "q", json!({"kind": "ok"}), 2)
        .await
        .unwrap();
    // Both claimed once: `poison` is at attempts == max_attempts, `retryable` is below it.
    assert_eq!(queue::claim(&pool, "q", 10, LEASE).await.unwrap().len(), 2);

    assert_eq!(
        queue::requeue_running(&pool, "q").await.unwrap(),
        queue::Recovered {
            requeued: 1,
            failed: 1
        }
    );
    let (state, attempts, error, _) = state_of(&pool, poison).await;
    assert_eq!(
        (state.as_str(), attempts, error.as_deref()),
        ("failed", 1, Some("process died on the final attempt"))
    );
    let finished: bool = sqlx::query_scalar(
        "SELECT finished_at IS NOT NULL AND locked_until IS NULL FROM jobs WHERE id = $1",
    )
    .bind(poison)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(finished);
    assert_eq!(state_of(&pool, retryable).await.0, "queued");
    let again = queue::claim(&pool, "q", 10, LEASE).await.unwrap();
    assert_eq!(
        again.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![retryable]
    );
}

/// Backstop: a queued job that is somehow already out of attempts is never claimed.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_skips_queued_jobs_out_of_attempts(pool: PgPool) {
    let id = queue::enqueue(&pool, "q", json!({}), 2).await.unwrap();
    sqlx::query("UPDATE jobs SET attempts = 2 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        queue::claim(&pool, "q", 10, LEASE)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn pending(
    pool: &PgPool,
    queue_name: &str,
    payload: serde_json::Value,
) -> Option<uuid::Uuid> {
    queue::enqueue_unless_pending(pool, queue_name, payload, 3)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enqueue_unless_pending_skips_a_queued_or_running_duplicate(pool: PgPool) {
    let first = pending(&pool, "q", json!({"kind": "x"})).await.unwrap();
    // Same kind, even with other fields: a duplicate.
    assert_eq!(
        pending(&pool, "q", json!({"kind": "x", "n": 2})).await,
        None
    );
    // Another kind or queue is not.
    assert!(pending(&pool, "q", json!({"kind": "y"})).await.is_some());
    assert!(pending(&pool, "r", json!({"kind": "x"})).await.is_some());

    // Running still counts as pending.
    let jobs = queue::claim(&pool, "q", 10, LEASE).await.unwrap();
    assert_eq!(pending(&pool, "q", json!({"kind": "x"})).await, None);
    // Once finished, a new one may be queued.
    for job in jobs.iter().filter(|j| j.id == first) {
        assert!(queue::complete(&pool, job).await.unwrap());
    }
    assert!(pending(&pool, "q", json!({"kind": "x"})).await.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enqueue_unless_pending_is_race_safe(pool: PgPool) {
    let calls = (0..16).map(|_| pending(&pool, "q", json!({"kind": "x"})));
    let results = futures::future::join_all(calls).await;
    assert_eq!(results.iter().filter(|r| r.is_some()).count(), 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'q'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enqueue_unless_queued_with_key_skips_only_queued_duplicates(pool: PgPool) {
    let add = |path: &'static str| {
        let pool = pool.clone();
        async move {
            queue::enqueue_unless_queued_with_key(
                &pool,
                "q",
                json!({"kind": "path", "path": path}),
                3,
                "path",
            )
            .await
            .unwrap()
        }
    };
    assert!(add("a.flac").await.is_some());
    assert_eq!(add("a.flac").await, None);
    assert!(add("b.flac").await.is_some());
    // A running job may have read stale input, so a new one is queued behind it.
    queue::claim(&pool, "q", 10, LEASE).await.unwrap();
    assert!(add("a.flac").await.is_some());
    assert_eq!(add("a.flac").await, None);
}
