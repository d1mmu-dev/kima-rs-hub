use std::path::PathBuf;
use std::sync::Arc;

use kima_core::auth::JwtKeys;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_library::transcode::Transcoder;
use kima_library::watch::{IgnoreSet, SharedStatus, WatchStatus};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::events::SseRegistry;

/// Library machinery the HTTP side shares with background tasks. Grows with later tasks (a
/// transcoder in Task 13, the organizer's use of the ignore set in Task 17).
#[derive(Clone, Default)]
pub struct LibraryServices {
    /// Written by the watcher, read by `GET /api/library/watch-status`.
    pub watch_status: SharedStatus,
    /// Paths the watcher must not react to (the organizer's own moves).
    pub ignore: IgnoreSet,
}

impl LibraryServices {
    pub fn new(mode: kima_core::config::WatchMode) -> Self {
        Self {
            watch_status: Arc::new(std::sync::RwLock::new(WatchStatus::new(mode))),
            ignore: IgnoreSet::new(),
        }
    }
}

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
    pub library: LibraryServices,
    /// Cached mp3 transcodes for the stream endpoint. `None` serves every stream as the original.
    pub transcoder: Option<Transcoder>,
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
            library: LibraryServices::default(),
            transcoder: None,
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

    pub fn with_transcoder(mut self, transcoder: Transcoder) -> Self {
        self.transcoder = Some(transcoder);
        self
    }

    pub fn with_library_services(mut self, services: LibraryServices) -> Self {
        self.library = services;
        self
    }

    /// Waits for a slot to run one argon2 hash or verify. Move the permit into the
    /// `spawn_blocking` closure: held by the handler, it would be freed when a client hangs
    /// up while the blocking work keeps running.
    pub async fn password_permit(&self) -> anyhow::Result<OwnedSemaphorePermit> {
        Ok(self.password_work.clone().acquire_owned().await?)
    }
}
