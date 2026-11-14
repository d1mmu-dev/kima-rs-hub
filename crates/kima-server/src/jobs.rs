//! Wires the job queue workers into the server process.

use std::collections::BTreeMap;
use std::time::Duration;

use kima_db::PgPool;
use kima_ident::IdentCtx;
use kima_ident::autotag::search;
use kima_ident::coordinator;
use kima_jobs::worker::{Handler, WorkerConfig, spawn_worker};
use kima_library::jobs as library_jobs;
use kima_library::scan::LibraryCtx;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Maps a queue name to the handler that runs its jobs, with that queue's worker settings.
#[derive(Default)]
pub struct Registry {
    queues: BTreeMap<String, (WorkerConfig, Handler)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, queue: impl Into<String>, concurrency: usize, handler: Handler) {
        self.register_config(WorkerConfig::new(queue, concurrency), handler);
    }

    /// Registers a queue with explicit worker settings (lease, poll interval, ...).
    pub fn register_config(&mut self, config: WorkerConfig, handler: Handler) {
        self.queues.insert(config.queue.clone(), (config, handler));
    }

    /// The worker settings registered for `queue`.
    pub fn config(&self, queue: &str) -> Option<&WorkerConfig> {
        self.queues.get(queue).map(|(c, _)| c)
    }

    /// The registered queue names, sorted.
    pub fn queues(&self) -> Vec<&str> {
        self.queues.keys().map(String::as_str).collect()
    }

    pub fn len(&self) -> usize {
        self.queues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    /// Registers the `library` queue (reconciles, single-file updates, organizer runs).
    /// Concurrency 1: those jobs must never overlap. Default lease: long runs keep their claim
    /// through the worker's heartbeat.
    /// A job that changed tracks kicks the enrichment coordinator (spec §10.6).
    pub fn register_library(&mut self, ctx: LibraryCtx) {
        self.register(
            library_jobs::QUEUE,
            1,
            library_jobs::handler_with(ctx, coordinator::kick_on_change()),
        );
    }

    /// Registers the `enrich` queue: the coordinator's cycles (spec §10.6). Concurrency 1;
    /// default lease, kept by the heartbeat through long cycles.
    pub fn register_enrich(&mut self, ctx: IdentCtx) {
        self.register(coordinator::QUEUE, 1, coordinator::handler(ctx));
    }

    /// Registers the `autotag` queue: one candidate search per album (spec §19.2).
    /// Concurrency 1: MusicBrainz allows one request at a time anyway, and the store of an
    /// album's candidates assumes one search of it at a time.
    pub fn register_autotag(&mut self, ctx: IdentCtx) {
        self.register(search::QUEUE, 1, search::handler(ctx));
    }

    /// Registers feed refresh independently of the identification switch.
    pub fn register_podcast_refresh(&mut self, ctx: kima_podcast::ctx::PodcastCtx) {
        self.register("podcasts", 2, podcast_refresh_handler(ctx));
    }

    /// Registers the `podcast-cache` queue: one episode download per job (Task 12, D-204).
    /// Concurrency 2, 3 attempts with the default 5 s/10 s/... backoff.
    pub fn register_podcast_cache(&mut self, ctx: kima_podcast::ctx::PodcastCtx) {
        self.register(kima_podcast::cache::QUEUE, 2, podcast_cache_handler(ctx));
    }
}

fn podcast_refresh_handler(ctx: kima_podcast::ctx::PodcastCtx) -> Handler {
    let catalog = kima_podcast::catalog::Catalog::new(ctx.net.clone(), ctx.pool.clone());
    kima_jobs::worker::handler_ctx(move |job, job_ctx| {
        let ctx = ctx.clone();
        let catalog = catalog.clone();
        async move {
            use kima_jobs::worker::JobError;
            let id = job
                .payload
                .get("podcast_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .ok_or_else(|| {
                    JobError::Permanent("podcast job missing or invalid podcast_id".into())
                })?;
            let run = async {
                let followed: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM podcast_subscriptions WHERE podcast_id = $1)",
                )
                .bind(id)
                .fetch_one(&ctx.pool)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
                if followed {
                    match kima_podcast::refresh::refresh_job(&catalog, id, job.id).await {
                        Ok(_) => {}
                        Err(kima_podcast::refresh::RefreshError::Feed(
                            kima_podcast::refresh::FeedFailure::Gone(_),
                        )) => {
                            // Feed says it is gone: permanent, never retry.
                            return Err(JobError::Permanent("podcast feed is gone".into()));
                        }
                        Err(error) => return Err(JobError::Retry(error.to_string())),
                    }
                }
                Ok(())
            };
            tokio::select! {
                result = run => result,
                _ = job_ctx.cancel.cancelled() => Err(JobError::Retry("podcast refresh cancelled".into())),
            }
        }
    })
}

fn podcast_cache_handler(ctx: kima_podcast::ctx::PodcastCtx) -> Handler {
    kima_jobs::worker::handler_ctx(move |job: kima_jobs::queue::Job, job_ctx| {
        let ctx = ctx.clone();
        async move {
            let uuid_field = |name: &str| {
                job.payload
                    .get(name)
                    .and_then(|v| v.as_str())
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .ok_or_else(|| format!("podcast-cache job missing or invalid {name:?}"))
            };
            let episode = uuid_field("episode")?;
            let user = uuid_field("user")?;
            // On shutdown or a lost claim the download is dropped (its temp file removed with
            // it) and the attempt handed back as a retry, which shutdown does not count.
            tokio::select! {
                result = kima_podcast::cache::download(&ctx, episode, user) => {
                    result.map_err(podcast_cache_job_error)
                }
                _ = job_ctx.cancel.cancelled() => {
                    Err(kima_jobs::worker::JobError::Retry(
                        "podcast download cancelled".into(),
                    ))
                }
            }
        }
    })
}

/// A failed episode download under the queue's retry policy: a size-cap breach fails at once
/// (D-217), everything else is retried.
fn podcast_cache_job_error(error: kima_podcast::cache::CacheError) -> kima_jobs::worker::JobError {
    use kima_jobs::worker::JobError;
    if error.is_permanent() {
        JobError::Permanent(error.to_string())
    } else {
        JobError::Retry(error.to_string())
    }
}

/// What the podcast startup work did (for its log line and its test).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PodcastStartup {
    /// Cached episodes deleted by the 30-day cleanup.
    pub cleaned: u64,
    /// Cached episodes whose length was measured.
    pub measured: usize,
    /// Shows given Apple's categories.
    pub categories_filled: usize,
}

/// The podcast work done once per start (upstream `index.ts:553-569`): the 30-day episode-cache
/// cleanup and then the measuring of what stays cached (once, via a marker file), and, beside
/// them, the category fill-in for shows with an Apple id and no categories (one batched Apple
/// lookup, `assignMissingCategories`). Failures are logged, never fatal.
pub async fn podcast_startup(ctx: &kima_podcast::ctx::PodcastCtx) -> PodcastStartup {
    let cache = async {
        let cleaned = match kima_podcast::cache::cleanup(ctx).await {
            Ok((deleted, freed_mb)) => {
                if deleted > 0 {
                    tracing::info!(
                        deleted,
                        freed_mb,
                        "cleaned up expired podcast episode cache"
                    );
                }
                deleted
            }
            Err(error) => {
                warn!(%error, "podcast episode cache cleanup failed");
                0
            }
        };
        let measured = kima_podcast::cache::measure_cached_once(ctx).await;
        if measured > 0 {
            tracing::info!(measured, "measured the length of cached podcast episodes");
        }
        (cleaned, measured)
    };
    let catalog = kima_podcast::catalog::Catalog::new(ctx.net.clone(), ctx.pool.clone());
    let ((cleaned, measured), categories_filled) =
        tokio::join!(cache, catalog.assign_missing_categories());
    PodcastStartup {
        cleaned,
        measured,
        categories_filled,
    }
}

/// Runs [`podcast_startup`] in the background, so the server does not wait for it to start.
pub fn spawn_podcast_startup(ctx: kima_podcast::ctx::PodcastCtx) -> JoinHandle<PodcastStartup> {
    tokio::spawn(async move { podcast_startup(&ctx).await })
}

/// Startup step for the `autotag` queue; call before [`spawn_all`]. Requeues searches left
/// `running` by a process that died and settles the albums they left `searching`.
pub async fn prepare_autotag_queue(pool: &PgPool) -> sqlx::Result<()> {
    search::prepare(pool).await
}

/// Startup step for the `library` queue; call before [`spawn_all`]. Requeues jobs left
/// `running` by a process that died (kima runs as a single process, so nothing can really be
/// running yet; otherwise a killed reconcile would block the startup one until its lease ran out),
/// then queues a reconcile unless one is already pending.
pub async fn prepare_library_queue(pool: &PgPool) -> sqlx::Result<()> {
    let recovered = kima_jobs::queue::requeue_running(pool, library_jobs::QUEUE).await?;
    if recovered.requeued > 0 {
        tracing::info!(
            requeued = recovered.requeued,
            "requeued library jobs left running by a previous process"
        );
    }
    if recovered.failed > 0 {
        warn!(
            failed = recovered.failed,
            "failed library jobs that were on their final attempt when the previous process died"
        );
    }
    // Organizer runs whose job is gone (it died on its final attempt) resume from the journal.
    let resumed = kima_library::organize::apply::resume_interrupted(pool).await?;
    if resumed > 0 {
        tracing::info!(resumed, "queued interrupted organizer runs");
    }
    // Autotag runs left writing/organizing/undoing (or queued) with no job: the job resumes
    // from the journal (spec §19.9), so no album stays busy.
    let resumed = kima_library::autotag::apply::resume_interrupted(pool).await?;
    if resumed > 0 {
        tracing::info!(resumed, "queued interrupted autotag runs");
    }
    library_jobs::enqueue_reconcile(pool).await?;
    Ok(())
}

/// Starts one worker per registered queue. All stop when `shutdown` fires.
pub fn spawn_all(
    pool: &PgPool,
    registry: Registry,
    shutdown: CancellationToken,
) -> Vec<JoinHandle<()>> {
    registry
        .queues
        .into_iter()
        .map(|(_, (config, handler))| spawn_worker(pool.clone(), config, handler, shutdown.clone()))
        .collect()
}

/// Waits up to `timeout` for the workers to finish; logs a warning per worker still running
/// at the deadline, then returns. Call after cancelling the shutdown token.
pub async fn drain(handles: Vec<JoinHandle<()>>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    for (index, mut handle) in handles.into_iter().enumerate() {
        match tokio::time::timeout_at(deadline, &mut handle).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(%error, worker = index, "job worker ended abnormally"),
            Err(_) => warn!(
                worker = index,
                ?timeout,
                "job worker still running at shutdown deadline"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_returns_at_the_deadline_for_a_stuck_worker() {
        let stuck = tokio::spawn(std::future::pending::<()>());
        let quick = tokio::spawn(async {});
        let started = std::time::Instant::now();
        drain(vec![quick, stuck], Duration::from_millis(100)).await;
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn empty_registry_spawns_nothing() {
        let registry = Registry::new();
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn register_podcast_cache_adds_its_queue() {
        let mut registry = Registry::new();
        registry.register_podcast_cache(kima_podcast::ctx::PodcastCtx {
            pool: unreachable_pool(),
            net: std::sync::Arc::new(kima_net::Net::disabled()),
            data_dir: std::path::PathBuf::new(),
        });
        assert_eq!(registry.queues(), vec![kima_podcast::cache::QUEUE]);
        assert_eq!(
            registry
                .config(kima_podcast::cache::QUEUE)
                .unwrap()
                .concurrency,
            2
        );
    }

    #[test]
    fn podcast_cache_failures_map_to_the_retry_policy() {
        use kima_jobs::worker::JobError;
        use kima_podcast::cache::CacheError;
        // Over the size cap: the host will send just as much again, so never retried.
        let too_large = podcast_cache_job_error(CacheError::TooLarge {
            would_write: 10,
            limit: 5,
        });
        assert!(matches!(too_large, JobError::Permanent(_)), "{too_large:?}");
        // Past the deadline or cut short: retried under the queue's usual backoff.
        let timed_out = podcast_cache_job_error(CacheError::TimedOut(Duration::from_secs(1)));
        assert!(matches!(timed_out, JobError::Retry(_)), "{timed_out:?}");
        let short = podcast_cache_job_error(CacheError::Incomplete {
            got: 1,
            expected: 2,
        });
        assert!(matches!(short, JobError::Retry(_)), "{short:?}");
    }

    /// A pool that is never actually used by this test (no query runs against it).
    fn unreachable_pool() -> PgPool {
        PgPool::connect_lazy("postgres://127.0.0.1:1/nope").expect("lazy pool never connects")
    }
}
