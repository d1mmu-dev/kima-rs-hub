use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use kima_jobs::schedule::{Cron, EnqueueFn, Scheduler};
use sqlx::PgPool;
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

#[tokio::test(start_paused = true)]
async fn scheduler_calls_enqueue_and_stops_on_shutdown() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let handle = Scheduler::new()
        .add(
            "test",
            Cron::parse("* * * * *").unwrap(),
            counting(counter.clone(), false),
        )
        .spawn(idle_pool(), shutdown.clone());
    settle().await;
    assert_eq!(counter.load(Ordering::SeqCst), 0);

    // The next minute boundary is at most 60 s away, and the one after is more than 60 s away.
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    shutdown.cancel();
    tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .expect("scheduler stops on shutdown")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn enqueue_error_does_not_stop_loop() {
    let counter = Arc::new(AtomicUsize::new(0));
    let shutdown = CancellationToken::new();
    let handle = Scheduler::new()
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
