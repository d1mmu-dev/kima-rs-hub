use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::queue::{self, Job, NOTIFY_CHANNEL};

/// Why a job attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// Retried after [`queue::backoff`], until `max_attempts` is reached.
    Retry(String),
    /// Never retried: the job fails at once, whatever attempts remain.
    Permanent(String),
}

impl From<String> for JobError {
    fn from(message: String) -> Self {
        Self::Retry(message)
    }
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retry(message) | Self::Permanent(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for JobError {}

/// What a running job gets besides its row.
#[derive(Debug, Clone)]
pub struct JobCtx {
    /// Fires on shutdown and when the job's claim is lost (its lease could not be renewed
    /// because the job was reaped or reclaimed). Long handlers check it between units of work
    /// and return early; the outcome of a lost claim is not recorded. A handler that returns
    /// [`JobError::Retry`] once shutdown has begun gets its attempt back (the job is queued
    /// again, due now); `Ok` completes the job and `Permanent` fails it as usual.
    pub cancel: CancellationToken,
}

pub type Handler =
    Arc<dyn Fn(Job, JobCtx) -> BoxFuture<'static, Result<(), JobError>> + Send + Sync>;

/// Opens a LISTEN connection on [`NOTIFY_CHANNEL`]; replaceable so tests can count or fail it.
pub type ListenConnect =
    Arc<dyn Fn(PgPool) -> BoxFuture<'static, sqlx::Result<PgListener>> + Send + Sync>;

fn default_listen_connect() -> ListenConnect {
    Arc::new(|pool| {
        Box::pin(async move {
            let mut listener = PgListener::connect_with(&pool).await?;
            listener.listen(NOTIFY_CHANNEL).await?;
            Ok(listener)
        })
    })
}

#[derive(Clone)]
pub struct WorkerConfig {
    pub queue: String,
    pub concurrency: usize,
    pub lease: Duration,
    /// How often a running job's lease is renewed. Well under `lease`, so a slow renewal or
    /// two does not let the claim lapse.
    pub heartbeat: Duration,
    pub poll_interval: Duration,
    /// Minimum time between attempts to re-establish a lost LISTEN connection.
    pub listen_retry: Duration,
    /// How the LISTEN connection is (re)established.
    pub listen_connect: ListenConnect,
}

impl std::fmt::Debug for WorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConfig")
            .field("queue", &self.queue)
            .field("concurrency", &self.concurrency)
            .field("lease", &self.lease)
            .field("heartbeat", &self.heartbeat)
            .field("poll_interval", &self.poll_interval)
            .field("listen_retry", &self.listen_retry)
            .finish_non_exhaustive()
    }
}

impl WorkerConfig {
    pub fn new(queue: impl Into<String>, concurrency: usize) -> Self {
        Self {
            queue: queue.into(),
            concurrency: concurrency.max(1),
            lease: Duration::from_secs(300),
            heartbeat: Duration::from_secs(300) / 4,
            poll_interval: Duration::from_secs(5),
            listen_retry: Duration::from_secs(30),
            listen_connect: default_listen_connect(),
        }
    }
}

/// A handler that needs neither the cancel token nor permanent errors: an `Err` is retried.
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Job) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    Arc::new(move |job, _ctx| f(job).map(|r| r.map_err(JobError::Retry)).boxed())
}

/// A handler given the job's [`JobCtx`].
pub fn handler_ctx<F, Fut>(f: F) -> Handler
where
    F: Fn(Job, JobCtx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), JobError>> + Send + 'static,
{
    Arc::new(move |job, ctx| Box::pin(f(job, ctx)))
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
        let mut listener = listen(&pool, &config).await;
        let mut last_listen_attempt = tokio::time::Instant::now();
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
                            let (config, shutdown) = (config.clone(), shutdown.clone());
                            running.spawn(async move {
                                run_one(&pool, &handler, &config, &shutdown, job).await;
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
                woke = notified(&mut listener) => {
                    if !woke {
                        // The connection dropped or failed: the old listener is dropped and replaced at once. On failure
                        // the retry arm tries again; either way the loop sweeps next, so
                        // jobs enqueued while unsubscribed are found.
                        last_listen_attempt = tokio::time::Instant::now();
                        listener = listen(&pool, &config).await;
                    }
                }
                _ = tokio::time::sleep_until(last_listen_attempt + config.listen_retry),
                    if listener.is_none() =>
                {
                    last_listen_attempt = tokio::time::Instant::now();
                    listener = listen(&pool, &config).await;
                    // The loop re-claims next, so jobs enqueued while unsubscribed are found.
                }
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

async fn listen(pool: &PgPool, config: &WorkerConfig) -> Option<PgListener> {
    match (config.listen_connect)(pool.clone()).await {
        Ok(listener) => Some(listener),
        Err(error) => {
            warn!(%error, "LISTEN unavailable; falling back to polling");
            None
        }
    }
}

/// Resolves when a notification arrives (`true`) or the connection is lost or fails (`false`).
async fn notified(listener: &mut Option<PgListener>) -> bool {
    match listener {
        Some(l) => match l.try_recv().await {
            Ok(Some(_)) => true,
            Ok(None) => {
                warn!("job notification connection dropped");
                false
            }
            Err(error) => {
                warn!(%error, "job notification stream failed");
                false
            }
        },
        None => std::future::pending().await,
    }
}

async fn run_one(
    pool: &PgPool,
    handler: &Handler,
    config: &WorkerConfig,
    shutdown: &CancellationToken,
    job: Job,
) {
    let cancel = shutdown.child_token();
    let ctx = JobCtx {
        cancel: cancel.clone(),
    };
    // Lazy, so a panic before the handler returns its future is caught too.
    let work = AssertUnwindSafe(async { handler(job.clone(), ctx).await }).catch_unwind();
    // Polled in this task next to the handler, so it stops as soon as the handler returns.
    let heartbeat = keep_claim(pool, &job, config.lease, config.heartbeat, &cancel);
    let outcome = tokio::select! {
        outcome = work => outcome,
        never = heartbeat => match never {},
    };
    let recorded = match outcome {
        Ok(Ok(())) => queue::complete(pool, &job).await,
        // Gave up because the process is shutting down: the attempt is given back, so a
        // restart (or several) never fails a job that was only interrupted.
        Ok(Err(JobError::Retry(_))) if shutdown.is_cancelled() => queue::release(pool, &job).await,
        Ok(Err(JobError::Retry(message))) => queue::fail(pool, &job, &message).await,
        Ok(Err(JobError::Permanent(message))) => queue::fail_permanent(pool, &job, &message).await,
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

/// Renews `job`'s lease every `heartbeat`. When a renewal finds the claim gone, cancels
/// `cancel`, logs once and stops renewing. A database error is logged and retried on the next
/// beat: the lease is still ours until it runs out. Never returns.
async fn keep_claim(
    pool: &PgPool,
    job: &Job,
    lease: Duration,
    heartbeat: Duration,
    cancel: &CancellationToken,
) -> std::convert::Infallible {
    let heartbeat = heartbeat.max(Duration::from_millis(1));
    let mut beats = tokio::time::interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
    beats.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        beats.tick().await;
        match queue::renew(pool, job, lease).await {
            Ok(true) => {}
            Ok(false) => {
                warn!(job = %job.id, queue = %job.queue, "job claim lost; cancelling the job");
                cancel.cancel();
                return std::future::pending().await;
            }
            Err(error) => warn!(%error, job = %job.id, "renewing the job lease failed"),
        }
    }
}
