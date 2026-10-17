use std::path::PathBuf;
use std::sync::Arc;

use kima_core::auth::JwtKeys;
use kima_core::events::EventBus;
use kima_db::PgPool;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::events::SseRegistry;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: Arc<JwtKeys>,
    pub events: EventBus,
    pub sse: Arc<SseRegistry>,
    /// Cancelled when the process is shutting down; long-lived responses (SSE) end on it.
    pub shutdown: CancellationToken,
    /// Bounds concurrent argon2 hash/verify work to the number of CPUs.
    pub password_work: Arc<Semaphore>,
    /// Where kima keeps its own files (artwork cache, ...). `None` disables features that need it.
    pub data_dir: Option<PathBuf>,
    /// The music library root that track paths are relative to. `None` until configured.
    pub library_root: Option<PathBuf>,
}

impl AppState {
    pub fn new(pool: PgPool, session_secret: &str) -> Self {
        Self::with_shutdown(pool, session_secret, CancellationToken::new())
    }

    pub fn with_shutdown(pool: PgPool, session_secret: &str, shutdown: CancellationToken) -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self {
            pool,
            jwt: Arc::new(JwtKeys::new(session_secret)),
            events: EventBus::new(1024),
            sse: Arc::new(SseRegistry::new()),
            shutdown,
            password_work: Arc::new(Semaphore::new(cpus.max(1))),
            data_dir: None,
            library_root: None,
        }
    }

    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    pub fn with_library_root(mut self, root: PathBuf) -> Self {
        self.library_root = Some(root);
        self
    }

    /// Waits for a slot to run one argon2 hash or verify. Move the permit into the
    /// `spawn_blocking` closure: held by the handler, it would be freed when a client hangs
    /// up while the blocking work keeps running.
    pub async fn password_permit(&self) -> anyhow::Result<OwnedSemaphorePermit> {
        Ok(self.password_work.clone().acquire_owned().await?)
    }
}
