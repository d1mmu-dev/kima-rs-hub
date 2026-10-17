use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kima_core::auth::JwtKeys;
use kima_core::cipher::Cipher;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_library::scan::LibraryRoot;
use kima_library::transcode::Transcoder;
use kima_library::watch::{IgnoreSet, SharedStatus, WatchStatus};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::events::SseRegistry;
use crate::login_limit::LoginLimiter;

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
    /// Shared with the job worker and the watcher, so switching it reaches all of them.
    pub library_root: Option<LibraryRoot>,
    /// `KIMA_ORGANIZE_TARGET`: where the organizer's copy mode writes. `None` disables copy mode.
    pub organize_target: Option<PathBuf>,
    pub library: LibraryServices,
    /// Cached mp3 transcodes for the stream endpoint. `None` serves every stream as the original.
    pub transcoder: Option<Transcoder>,
    /// Encrypts secrets stored in the database. Random per process unless `with_cipher` is used.
    pub cipher: Arc<Cipher>,
    /// Failed password sign-ins, per (client IP, username).
    pub login_limit: Arc<LoginLimiter>,
    /// Counts `password_permit()` calls, so tests can prove no argon2 work was started.
    password_permits: Arc<AtomicU64>,
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
            organize_target: None,
            library: LibraryServices::default(),
            transcoder: None,
            cipher: Arc::new(Cipher::random()),
            login_limit: Arc::new(LoginLimiter::new()),
            password_permits: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_cipher(mut self, cipher: Cipher) -> Self {
        self.cipher = Arc::new(cipher);
        self
    }

    pub fn with_data_dir(mut self, data_dir: PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    pub fn with_library_root(mut self, root: PathBuf) -> Self {
        self.library_root = Some(LibraryRoot::new(root));
        self
    }

    /// Uses `root` itself, so a switch made elsewhere (the organizer job) shows here.
    pub fn with_shared_library_root(mut self, root: LibraryRoot) -> Self {
        self.library_root = Some(root);
        self
    }

    /// The current library root.
    pub fn library_root(&self) -> Option<PathBuf> {
        self.library_root.as_ref().map(LibraryRoot::get)
    }

    pub fn with_organize_target(mut self, target: Option<PathBuf>) -> Self {
        self.organize_target = target;
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
        self.password_permits.fetch_add(1, Ordering::Relaxed);
        Ok(self.password_work.clone().acquire_owned().await?)
    }

    /// How many password permits have been requested so far.
    pub fn password_permits_issued(&self) -> u64 {
        self.password_permits.load(Ordering::Relaxed)
    }
}
