use std::time::Duration;

use kima_db::PgPool;
use kima_jobs::queue;
use kima_jobs::worker::{WorkerConfig, handler, spawn_worker};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn fast(queue: &str, concurrency: usize) -> WorkerConfig {
    WorkerConfig {
        poll_interval: Duration::from_millis(200),
        ..WorkerConfig::new(queue, concurrency)
    }
}

async fn wait_for_state(pool: &PgPool, id: uuid::Uuid, want: &str) -> (i32, Option<String>) {
    for _ in 0..50 {
        let (state, attempts, error): (String, i32, Option<String>) =
            sqlx::query_as("SELECT state, attempts, last_error FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        if state == want {
            return (attempts, error);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} never reached state {want}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn runs_a_job_and_marks_it_done(pool: PgPool) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let worker = spawn_worker(
        pool.clone(),
        fast("scan", 2),
        handler(move |job| {
            let tx = tx.clone();
            async move {
                tx.send(job.payload).unwrap();
                Ok(())
            }
        }),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "scan", json!({"n": 1}), 3)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, json!({"n": 1}));
    wait_for_state(&pool, id, "done").await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn handler_error_is_recorded_and_retried_later(pool: PgPool) {
    let shutdown = CancellationToken::new();
    let _worker = spawn_worker(
        pool.clone(),
        fast("scan", 1),
        handler(|_job| async { Err("disk on fire".to_string()) }),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "scan", json!({}), 3).await.unwrap();
    // "queued" is also the initial state, so wait for the recorded error instead.
    for _ in 0..50 {
        let (state, attempts, error): (String, i32, Option<String>) =
            sqlx::query_as("SELECT state, attempts, last_error FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        if error.is_some() {
            assert_eq!(
                (state.as_str(), attempts, error.as_deref()),
                ("queued", 1, Some("disk on fire"))
            );
            shutdown.cancel();
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("failure never recorded");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn panicking_handler_does_not_kill_the_worker(pool: PgPool) {
    let shutdown = CancellationToken::new();
    let _worker = spawn_worker(
        pool.clone(),
        fast("scan", 1),
        handler(|job| async move {
            if job.payload["panic"] == true {
                panic!("handler bug");
            }
            Ok(())
        }),
        shutdown.clone(),
    );
    let bad = queue::enqueue(&pool, "scan", json!({"panic": true}), 1)
        .await
        .unwrap();
    let (_, error) = wait_for_state(&pool, bad, "failed").await;
    assert_eq!(error.as_deref(), Some("handler panicked"));
    let good = queue::enqueue(&pool, "scan", json!({"panic": false}), 1)
        .await
        .unwrap();
    wait_for_state(&pool, good, "done").await;
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn shutdown_waits_for_running_jobs(pool: PgPool) {
    let shutdown = CancellationToken::new();
    let worker = spawn_worker(
        pool.clone(),
        fast("scan", 1),
        handler(|_job| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        }),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "scan", json!({}), 1).await.unwrap();
    wait_for_state(&pool, id, "running").await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    wait_for_state(&pool, id, "done").await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn panic_before_the_future_is_recorded(pool: PgPool) {
    let shutdown = CancellationToken::new();
    let _worker = spawn_worker(
        pool.clone(),
        fast("scan", 1),
        handler(|job| {
            if job.payload["panic"] == true {
                panic!("handler bug before the future");
            }
            async { Ok(()) }
        }),
        shutdown.clone(),
    );
    let bad = queue::enqueue(&pool, "scan", json!({"panic": true}), 1)
        .await
        .unwrap();
    let (_, error) = wait_for_state(&pool, bad, "failed").await;
    assert_eq!(error.as_deref(), Some("handler panicked"));
    let good = queue::enqueue(&pool, "scan", json!({"panic": false}), 1)
        .await
        .unwrap();
    wait_for_state(&pool, good, "done").await;
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listen_is_retried_after_failure(pool: PgPool) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    // The poll interval is long, so only NOTIFY can wake the worker in time.
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(30),
        listen_retry: Duration::from_millis(200),
        ..WorkerConfig::new("scan", 1)
    };
    let _worker = spawn_worker(
        pool.clone(),
        config,
        handler(move |job| {
            let tx = tx.clone();
            async move {
                tx.send(job.payload).unwrap();
                Ok(())
            }
        }),
        shutdown.clone(),
    );

    // Kill the LISTEN backend, as a database restart or a network blip would.
    let mut killed = 0;
    for _ in 0..50 {
        killed = sqlx::query_scalar::<_, i64>(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE datname = current_database() AND query ILIKE 'LISTEN%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if killed > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(killed > 0, "never found the LISTEN backend");

    // Give the worker time to notice and re-listen, then enqueue.
    tokio::time::sleep(Duration::from_millis(600)).await;
    queue::enqueue(&pool, "scan", json!({"n": 7}), 1)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("job not picked up without a poll")
        .unwrap();
    assert_eq!(got, json!({"n": 7}));
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listen_is_retried_when_the_first_attempt_fails(pool: PgPool) {
    // A pool whose connections are all taken makes the first LISTEN time out.
    let small = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_millis(300))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let held = (
        small.acquire().await.unwrap(),
        small.acquire().await.unwrap(),
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(30),
        listen_retry: Duration::from_millis(200),
        ..WorkerConfig::new("scan", 1)
    };
    let _worker = spawn_worker(
        small.clone(),
        config,
        handler(move |job| {
            let tx = tx.clone();
            async move {
                tx.send(job.payload).unwrap();
                Ok(())
            }
        }),
        shutdown.clone(),
    );
    tokio::time::sleep(Duration::from_millis(800)).await; // first LISTEN has failed
    drop(held);
    queue::enqueue(&pool, "scan", json!({"n": 9}), 1)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("worker never recovered without waiting a poll interval")
        .unwrap();
    assert_eq!(got, json!({"n": 9}));

    // The recovered listener must be kept: one LISTEN backend, and the same one
    // after several retry periods (a discarded listener would be replaced every 200 ms).
    let listen_pids = || async {
        sqlx::query_scalar::<_, i32>(
            "SELECT pid FROM pg_stat_activity \
             WHERE datname = current_database() AND query ILIKE 'LISTEN%' ORDER BY pid",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let before = listen_pids().await;
    assert_eq!(before.len(), 1, "expected one LISTEN backend: {before:?}");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        listen_pids().await,
        before,
        "listener was replaced, not kept"
    );

    // With a listener in place no retry or poll can explain a prompt pickup; only NOTIFY can.
    queue::enqueue(&pool, "scan", json!({"n": 10}), 1)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("second job needed more than NOTIFY latency")
        .unwrap();
    assert_eq!(got, json!({"n": 10}));
    shutdown.cancel();
}
