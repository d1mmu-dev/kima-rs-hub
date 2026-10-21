use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kima_db::PgPool;
use kima_jobs::queue::{self, NOTIFY_CHANNEL};
use kima_jobs::worker::{
    JobCtx, JobError, ListenConnect, WorkerConfig, handler, handler_ctx, spawn_worker,
};
use serde_json::json;
use sqlx::postgres::PgListener;
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

/// Terminates every backend whose current query is LISTEN, waiting (bounded) until one exists.
async fn terminate_listen_backend(pool: &PgPool) {
    let mut killed = 0;
    for _ in 0..100 {
        killed = sqlx::query_scalar::<_, i64>(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE datname = current_database() AND query ILIKE 'LISTEN%'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if killed > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(killed > 0, "never found the LISTEN backend");
}

fn counting_connect(calls: Arc<AtomicUsize>, fail_on: Option<usize>) -> ListenConnect {
    Arc::new(move |pool: PgPool| {
        let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            if Some(n) == fail_on {
                return Err(sqlx::Error::PoolTimedOut);
            }
            let mut listener = PgListener::connect_with(&pool).await?;
            listener.listen(NOTIFY_CHANNEL).await?;
            Ok(listener)
        })
    })
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reconnect_failure_uses_retry_arm(pool: PgPool) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(30),
        listen_retry: Duration::from_millis(200),
        listen_connect: counting_connect(calls.clone(), Some(2)),
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

    terminate_listen_backend(&pool).await;
    // The 2nd call (the immediate reconnect) fails; the retry arm makes the 3rd.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while calls.load(Ordering::SeqCst) < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no third listen_connect call: {}",
            calls.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    queue::enqueue(&pool, "scan", json!({"n": 7}), 1)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("job not picked up after the retry arm reconnected")
        .unwrap();
    assert_eq!(got, json!({"n": 7}));
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn jobs_enqueued_during_reconnect_run_promptly(pool: PgPool) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let config = WorkerConfig {
        poll_interval: Duration::from_secs(3600),
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

    terminate_listen_backend(&pool).await;
    queue::enqueue(&pool, "scan", json!({"n": 3}), 1)
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("job enqueued during the reconnect was not swept")
        .unwrap();
    assert_eq!(got, json!({"n": 3}));
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listen_is_retried_when_the_first_attempt_fails(pool: PgPool) {
    // A pool whose connections are all taken makes the first LISTEN time out. The pool is lazy
    // and the two connections are taken with a retry: establishing a connection counts against
    // the short acquire timeout, which a loaded machine can exceed.
    let small = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy_with((*pool.connect_options()).clone());
    let mut taken = Vec::new();
    for _ in 0..100 {
        match small.acquire().await {
            Ok(conn) => taken.push(conn),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
        if taken.len() == 2 {
            break;
        }
    }
    assert_eq!(taken.len(), 2, "could not take both pool connections");
    let held = taken;

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
    // The job can arrive through the recovery poll after a failed attempt, before the next
    // attempt's LISTEN is visible, so wait for the backend to appear.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let before = loop {
        let pids = listen_pids().await;
        if pids.len() == 1 {
            break pids;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected one LISTEN backend: {pids:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
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

async fn row(pool: &PgPool, id: uuid::Uuid) -> (String, i32, Option<String>) {
    sqlx::query_as("SELECT state, attempts, last_error FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[test]
fn job_error_from_string_is_retry_and_displays_its_message() {
    assert_eq!(
        JobError::from("boom".to_string()),
        JobError::Retry("boom".into())
    );
    assert_eq!(JobError::Permanent("nope".into()).to_string(), "nope");
    assert_eq!(JobError::Retry("boom".into()).to_string(), "boom");
}

#[test]
fn heartbeat_defaults_to_a_quarter_lease() {
    let config = WorkerConfig::new("q", 1);
    assert_eq!(config.lease, Duration::from_secs(300));
    assert_eq!(config.heartbeat, config.lease / 4);
}

/// Spec §24.3: a job running for several leases keeps its claim, so a second worker polling
/// the same queue never reclaims it.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn job_longer_than_three_leases_completes(pool: PgPool) {
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let config = |poll| WorkerConfig {
        lease: Duration::from_secs(1),
        heartbeat: Duration::from_millis(250),
        poll_interval: poll,
        ..WorkerConfig::new("long", 1)
    };
    let slow = {
        let runs = runs.clone();
        handler(move |_job| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(3500)).await;
                Ok(())
            }
        })
    };
    let first = spawn_worker(
        pool.clone(),
        config(Duration::from_millis(100)),
        slow.clone(),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "long", json!({}), 3).await.unwrap();
    wait_for_state(&pool, id, "running").await;
    let second = spawn_worker(
        pool.clone(),
        config(Duration::from_millis(100)),
        slow,
        shutdown.clone(),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let (state, attempts, _) = loop {
        let r = row(&pool, id).await;
        if (r.0 != "running" && r.0 != "queued") || tokio::time::Instant::now() > deadline {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!((state.as_str(), attempts), ("done", 1));
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the handler ran more than once"
    );
    shutdown.cancel();
    for w in [first, second] {
        tokio::time::timeout(Duration::from_secs(10), w)
            .await
            .unwrap()
            .unwrap();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lost_claim_cancels_token(pool: PgPool) {
    let heartbeat = Duration::from_millis(500);
    let (started_tx, mut started) = mpsc::unbounded_channel();
    let (cancelled_tx, mut cancelled) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let _worker = spawn_worker(
        pool.clone(),
        WorkerConfig {
            lease: Duration::from_secs(30),
            heartbeat,
            ..fast("lost", 1)
        },
        handler_ctx(move |_job, ctx: JobCtx| {
            let (started_tx, cancelled_tx) = (started_tx.clone(), cancelled_tx.clone());
            async move {
                started_tx.send(()).unwrap();
                ctx.cancel.cancelled().await;
                cancelled_tx.send(()).unwrap();
                Ok(())
            }
        }),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "lost", json!({}), 3).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.recv())
        .await
        .expect("job never started")
        .unwrap();
    // The claim is taken away (run_at pushed out so the worker cannot simply claim it again).
    sqlx::query(
        "UPDATE jobs SET state = 'queued', run_at = now() + interval '1 hour' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    tokio::time::timeout(heartbeat * 2, cancelled.recv())
        .await
        .expect("token not cancelled within two heartbeats")
        .unwrap();
    // Give the worker time to (wrongly) record the outcome, then check it did not.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(row(&pool, id).await.0, "queued");
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn permanent_error_fails_at_once(pool: PgPool) {
    let shutdown = CancellationToken::new();
    let _worker = spawn_worker(
        pool.clone(),
        fast("perm", 1),
        handler_ctx(|_job, _ctx| async { Err(JobError::Permanent("nope".into())) }),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "perm", json!({}), 5).await.unwrap();
    let (attempts, error) = wait_for_state(&pool, id, "failed").await;
    assert_eq!((attempts, error.as_deref()), (1, Some("nope")));
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn string_error_still_retries(pool: PgPool) {
    let shutdown = CancellationToken::new();
    let _worker = spawn_worker(
        pool.clone(),
        fast("retry", 1),
        handler(|_job| async { Err("flaky".to_string()) }),
        shutdown.clone(),
    );
    let id = queue::enqueue(&pool, "retry", json!({}), 5).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (state, attempts, error) = row(&pool, id).await;
        if error.is_some() {
            assert_eq!(
                (state.as_str(), attempts, error.as_deref()),
                ("queued", 1, Some("flaky"))
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "failure never recorded"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    shutdown.cancel();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn shutdown_cancels_job_token(pool: PgPool) {
    let (started_tx, mut started) = mpsc::unbounded_channel();
    let (cancelled_tx, mut cancelled) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let worker = spawn_worker(
        pool.clone(),
        fast("stop", 1),
        handler_ctx(move |_job, ctx: JobCtx| {
            let (started_tx, cancelled_tx) = (started_tx.clone(), cancelled_tx.clone());
            async move {
                started_tx.send(()).unwrap();
                ctx.cancel.cancelled().await;
                cancelled_tx.send(()).unwrap();
                Err(JobError::Retry("cancelled".into()))
            }
        }),
        shutdown.clone(),
    );
    queue::enqueue(&pool, "stop", json!({}), 3).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.recv())
        .await
        .expect("job never started")
        .unwrap();
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), cancelled.recv())
        .await
        .expect("job token not cancelled on shutdown")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("worker did not stop")
        .unwrap();
}
