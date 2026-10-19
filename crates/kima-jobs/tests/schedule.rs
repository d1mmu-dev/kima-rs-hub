use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use kima_jobs::schedule::{Clock, Cron, EnqueueFn, Scheduler};
use sqlx::PgPool;
use time::OffsetDateTime;
use time::macros::datetime;
use tokio_util::sync::CancellationToken;

/// The scheduled functions here never touch the pool.
fn idle_pool() -> PgPool {
    PgPool::connect_lazy("postgres://unused@127.0.0.1:1/unused").unwrap()
}

fn counting(counter: Arc<AtomicUsize>, fail: bool) -> EnqueueFn {
    Arc::new(move |_pool| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            if fail {
                Err("boom".to_string())
            } else {
                Ok(())
            }
        }
        .boxed()
    })
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

/// A wall clock that starts at `start`, follows tokio's paused time, and can be stepped.
fn fake_clock(start: OffsetDateTime) -> (Clock, Arc<AtomicI64>) {
    let base = tokio::time::Instant::now();
    let jump = Arc::new(AtomicI64::new(0));
    let j = jump.clone();
    let clock: Clock = Arc::new(move || {
        start + base.elapsed() + time::Duration::seconds(j.load(Ordering::SeqCst))
    });
    (clock, jump)
}

async fn advance_secs(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
    settle().await;
}

#[tokio::test(start_paused = true)]
async fn scheduler_calls_enqueue_and_stops_on_shutdown() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    // 30 s before the 12:35 boundary.
    let (clock, _) = fake_clock(datetime!(2026-01-01 12:34:30 UTC));
    let handle = Scheduler::new()
        .with_clock(clock)
        .add(
            "test",
            Cron::parse("* * * * *").unwrap(),
            counting(counter.clone(), false),
        )
        .spawn(idle_pool(), shutdown.clone());
    settle().await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    advance_secs(29).await;
    assert_eq!(counter.load(Ordering::SeqCst), 0, "not before the boundary");
    advance_secs(2).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1, "once at the boundary");
    // Not again until the next boundary, 60 s after the first.
    advance_secs(50).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    advance_secs(15).await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .expect("scheduler stops on shutdown")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn backward_clock_step_does_not_refire_a_boundary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let (clock, jump) = fake_clock(datetime!(2026-01-01 12:34:30 UTC));
    let handle = Scheduler::new()
        .with_clock(clock)
        .add(
            "test",
            Cron::parse("* * * * *").unwrap(),
            counting(counter.clone(), false),
        )
        .spawn(idle_pool(), shutdown.clone());
    settle().await;
    advance_secs(31).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // The clock steps back two minutes, then passes the 12:35 boundary again.
    jump.store(-120, Ordering::SeqCst);
    advance_secs(150).await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "12:35 does not fire twice"
    );

    shutdown.cancel();
    handle.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn clock_step_fires_within_one_chunk_without_catch_up() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let (clock, jump) = fake_clock(datetime!(2026-01-01 03:00:00 UTC));
    let handle = Scheduler::new()
        .with_clock(clock)
        .add(
            "daily",
            Cron::parse("0 4 * * *").unwrap(),
            counting(counter.clone(), false),
        )
        .spawn(idle_pool(), shutdown.clone());
    settle().await;
    advance_secs(60).await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    // The wall clock leaps past 04:00 (resume from suspend, NTP step) while it sleeps.
    jump.store(2 * 3600, Ordering::SeqCst);
    advance_secs(61).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1, "fires within one chunk");
    advance_secs(300).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1, "no double fire");

    // A step over several boundaries fires once, not once per missed boundary.
    jump.store(2 * 3600 + 3 * 86400, Ordering::SeqCst);
    advance_secs(61).await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    advance_secs(300).await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);

    shutdown.cancel();
    handle.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn enqueue_error_does_not_stop_loop() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let (clock, _) = fake_clock(datetime!(2026-01-01 12:34:30 UTC));
    let handle = Scheduler::new()
        .with_clock(clock)
        .add(
            "failing",
            Cron::parse("* * * * *").unwrap(),
            counting(counter.clone(), true),
        )
        .spawn(idle_pool(), shutdown.clone());
    settle().await;
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
    }
    assert!(
        counter.load(Ordering::SeqCst) >= 3,
        "kept firing after errors"
    );
    shutdown.cancel();
    tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .unwrap()
        .unwrap();
}
