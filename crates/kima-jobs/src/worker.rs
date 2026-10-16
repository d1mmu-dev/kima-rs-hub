use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::queue::{self, Job, NOTIFY_CHANNEL};

pub type Handler = Arc<dyn Fn(Job) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub queue: String,
    pub concurrency: usize,
    pub lease: Duration,
    pub poll_interval: Duration,
}

impl WorkerConfig {
    pub fn new(queue: impl Into<String>, concurrency: usize) -> Self {
        Self {
            queue: queue.into(),
            concurrency: concurrency.max(1),
            lease: Duration::from_secs(300),
            poll_interval: Duration::from_secs(5),
        }
    }
}

pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Job) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    Arc::new(move |job| Box::pin(f(job)))
}

/// Runs `handler` for jobs on `config.queue`, at most `config.concurrency` at a time,
/// until `shutdown` fires; then waits for jobs already running.
pub fn spawn_worker(
    pool: PgPool,
    config: WorkerConfig,
    handler: Handler,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut listener = listen(&pool).await;
        let permits = Arc::new(Semaphore::new(config.concurrency));
        let mut running = JoinSet::new();

        while !shutdown.is_cancelled() {
            while let Some(result) = running.try_join_next() {
                log_join(result);
            }
            if let Err(error) = queue::reap(&pool).await {
                warn!(%error, "reaping expired jobs failed");
            }

            let free = permits.available_permits();
            if free > 0 {
                match queue::claim(&pool, &config.queue, free as i64, config.lease).await {
                    Ok(jobs) => {
                        let claimed_all = jobs.len() == free;
                        for job in jobs {
                            let permit =
                                permits.clone().acquire_owned().await.expect("never closed");
                            let (pool, handler) = (pool.clone(), handler.clone());
                            running.spawn(async move {
                                run_one(&pool, &handler, job).await;
                                drop(permit);
                            });
                        }
                        if claimed_all {
                            continue; // the queue may hold more; claim again once a slot frees
                        }
                    }
                    Err(error) => warn!(%error, queue = %config.queue, "claiming jobs failed"),
                }
            }

            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = notified(&mut listener) => {}
                Some(result) = running.join_next(), if !running.is_empty() => log_join(result),
                _ = tokio::time::sleep(config.poll_interval) => {}
            }
        }

        while let Some(result) = running.join_next().await {
            log_join(result);
        }
    })
}

fn log_join(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        warn!(%error, "job task failed");
    }
}

async fn listen(pool: &PgPool) -> Option<PgListener> {
    let mut listener = match PgListener::connect_with(pool).await {
        Ok(listener) => listener,
        Err(error) => {
            warn!(%error, "LISTEN unavailable; falling back to polling");
            return None;
        }
    };
    match listener.listen(NOTIFY_CHANNEL).await {
        Ok(()) => Some(listener),
        Err(error) => {
            warn!(%error, "LISTEN failed; falling back to polling");
            None
        }
    }
}

async fn notified(listener: &mut Option<PgListener>) {
    match listener {
        Some(l) => {
            if let Err(error) = l.recv().await {
                warn!(%error, "job notification stream failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        None => std::future::pending().await,
    }
}

async fn run_one(pool: &PgPool, handler: &Handler, job: Job) {
    // Lazy, so a panic before the handler returns its future is caught too.
    let outcome = AssertUnwindSafe(async { handler(job.clone()).await })
        .catch_unwind()
        .await;
    let recorded = match outcome {
        Ok(Ok(())) => queue::complete(pool, &job).await,
        Ok(Err(message)) => queue::fail(pool, &job, &message).await,
        Err(_) => queue::fail(pool, &job, "handler panicked").await,
    };
    match recorded {
        Ok(true) => {}
        Ok(false) => warn!(
            job = %job.id,
            "job outcome dropped: claim lost (lease expired and the job was reclaimed or reaped)"
        ),
        Err(error) => warn!(%error, job = %job.id, "could not record job outcome"),
    }
}
