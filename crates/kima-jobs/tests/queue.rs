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
    queue::claim(&pool, "scan", 1, LEASE).await.unwrap();
    queue::complete(&pool, id).await.unwrap();
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
