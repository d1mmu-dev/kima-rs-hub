//! Wires the job queue workers into the server process.

use std::collections::BTreeMap;
use std::time::Duration;

use kima_db::PgPool;
use kima_jobs::worker::{Handler, WorkerConfig, spawn_worker};
use kima_library::jobs as library_jobs;
use kima_library::scan::LibraryCtx;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Lease for `library` jobs. A first reconcile of a large library (or an organizer run) can
/// take hours, and the worker does not renew leases yet (heartbeats are planned for M3). With
/// the default 5 min lease, a job on its final attempt would be reaped as "lease expired"
/// while still running; 6 h keeps a long run from being reaped.
pub const LIBRARY_LEASE: Duration = Duration::from_secs(6 * 3600);

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

    pub fn len(&self) -> usize {
        self.queues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    /// Registers the `library` queue (reconciles, single-file updates, organizer runs).
    /// Concurrency 1: those jobs must never overlap. Lease: [`LIBRARY_LEASE`].
    pub fn register_library(&mut self, ctx: LibraryCtx) {
        let mut config = WorkerConfig::new(library_jobs::QUEUE, 1);
        config.lease = LIBRARY_LEASE;
        self.register_config(config, library_jobs::handler(ctx));
    }
}

/// Startup step for the `library` queue; call before [`spawn_all`]. Requeues jobs left
/// `running` by a process that died (kima runs as a single process, so nothing can really be
/// running yet; otherwise a killed reconcile would block the startup one for its 6 h lease),
/// then queues a reconcile unless one is already pending.
pub async fn prepare_library_queue(pool: &PgPool) -> sqlx::Result<()> {
    let requeued = kima_jobs::queue::requeue_running(pool, library_jobs::QUEUE).await?;
    if requeued > 0 {
        tracing::info!(
            requeued,
            "requeued library jobs left running by a previous process"
        );
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
}
