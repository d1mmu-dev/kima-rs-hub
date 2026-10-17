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

/// Maps a queue name to the handler that runs its jobs, with that queue's concurrency.
#[derive(Default)]
pub struct Registry {
    queues: BTreeMap<String, (usize, Handler)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, queue: impl Into<String>, concurrency: usize, handler: Handler) {
        self.queues.insert(queue.into(), (concurrency, handler));
    }

    pub fn len(&self) -> usize {
        self.queues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }

    /// Registers the `library` queue (reconciles, single-file updates, organizer runs).
    /// Concurrency 1: those jobs must never overlap.
    pub fn register_library(&mut self, ctx: LibraryCtx) {
        self.register(library_jobs::QUEUE, 1, library_jobs::handler(ctx));
    }
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
        .map(|(queue, (concurrency, handler))| {
            spawn_worker(
                pool.clone(),
                WorkerConfig::new(queue, concurrency),
                handler,
                shutdown.clone(),
            )
        })
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
