use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kima_core::auth::JwtKeys;
use kima_core::cipher::Cipher;
use kima_core::events::EventBus;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::settings_kv::SettingsCache;
use kima_library::scan::LibraryRoot;
use kima_library::transcode::Transcoder;
use kima_library::watch::{IgnoreSet, SharedStatus, WatchStatus};
use kima_net::Net;
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
    /// The safety-net reconcile schedule (`KIMA_LIBRARY_SYNC_CRON`); `None` when off.
    pub sync_cron: Option<kima_jobs::schedule::Cron>,
}

impl LibraryServices {
    pub fn with_sync_cron(mut self, cron: Option<kima_jobs::schedule::Cron>) -> Self {
        self.sync_cron = cron;
        self
    }

    pub fn new(mode: kima_core::config::WatchMode) -> Self {
        Self {
            watch_status: Arc::new(std::sync::RwLock::new(WatchStatus::new(mode))),
            ignore: IgnoreSet::new(),
            sync_cron: None,
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
    /// `KIMA_ARTWORK_CACHE_MB` in bytes: the cover-art cache is trimmed to this after each write.
    pub artwork_cache_bytes: u64,
    /// The music library root that track paths are relative to. `None` until configured.
    /// Shared with the job worker and the watcher, so switching it reaches all of them.
    pub library_root: Option<LibraryRoot>,
    /// `KIMA_ORGANIZE_TARGET`: where the organizer's copy mode writes. `None` disables copy mode.
    pub organize_target: Option<PathBuf>,
    pub library: LibraryServices,
    /// `KIMA_PUBLIC_URL`, shown to clients by `GET /api/system-settings/public-config`.
    pub public_url: Option<String>,
    /// Cached mp3 transcodes for the stream endpoint. `None` serves every stream as the original.
    pub transcoder: Option<Transcoder>,
    /// Encrypts secrets stored in the database. Random per process unless `with_cipher` is used.
    pub cipher: Arc<Cipher>,
    /// The key-value settings (`settings_kv`), cached for a minute; a POST replaces the entry.
    pub settings: SettingsCache,
    /// Whether outbound lookups may run: `autoEnrichMetadata` and `KIMA_IDENTIFY` (D-111).
    pub identify: IdentifySwitch,
    /// The one outbound HTTP client (spec §7). `Net::disabled()` unless `with_net` is used.
    pub net: Arc<Net>,
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
        let cipher = Arc::new(Cipher::random());
        Self {
            settings: SettingsCache::new(pool.clone(), cipher.clone()),
            identify: IdentifySwitch::new(false, true),
            net: Arc::new(Net::disabled()),
            pool,
            jwt: Arc::new(JwtKeys::new(session_secret)),
            events: EventBus::new(1024),
            sse: Arc::new(SseRegistry::new()),
            shutdown,
            password_work: Arc::new(Semaphore::new(cpus.max(1))),
            data_dir: None,
            artwork_cache_bytes: 512 * 1024 * 1024,
            library_root: None,
            organize_target: None,
            library: LibraryServices::default(),
            public_url: None,
            transcoder: None,
            cipher,
            login_limit: Arc::new(LoginLimiter::new()),
            password_permits: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_cipher(mut self, cipher: Cipher) -> Self {
        self.cipher = Arc::new(cipher);
        self.settings = SettingsCache::new(self.pool.clone(), self.cipher.clone());
        self
    }

    pub fn with_settings(mut self, settings: SettingsCache) -> Self {
        self.settings = settings;
        self
    }

    pub fn with_identify(mut self, identify: IdentifySwitch) -> Self {
        self.identify = identify;
        self
    }

    pub fn with_net(mut self, net: Arc<Net>) -> Self {
        self.net = net;
        self
    }

    pub fn with_public_url(mut self, public_url: Option<String>) -> Self {
        self.public_url = public_url;
        self
    }

    pub fn with_artwork_cache_mb(mut self, mb: u64) -> Self {
        self.artwork_cache_bytes = mb.saturating_mul(1024 * 1024);
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
