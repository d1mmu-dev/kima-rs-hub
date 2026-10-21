//! What every identification stage needs, built once per process.

use std::sync::Arc;

use kima_core::cipher::Cipher;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_db::settings_kv::SettingsCache;
use kima_library::scan::LibraryCtx;
use kima_net::Net;

/// Shared handles for the stages. Cheap to clone.
#[derive(Clone)]
pub struct IdentCtx {
    pub pool: PgPool,
    pub net: Arc<Net>,
    pub cipher: Arc<Cipher>,
    pub events: EventBus,
    pub library: LibraryCtx,
    pub settings: SettingsCache,
    pub config: IdentConfig,
}

/// Process configuration for identification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentConfig {
    /// ffmpeg was found at startup (spec §9.4): the fingerprint stage is offered.
    pub fingerprinting: bool,
    /// Decodes run at once (`KIMA_FINGERPRINT_WORKERS`, spec §9.3).
    pub fingerprint_workers: usize,
    /// `ACOUSTID_DISABLED=true`.
    pub acoustid_disabled: bool,
}

impl IdentConfig {
    /// `max(1, available_parallelism / 2)` (spec §9.3).
    pub fn default_fingerprint_workers() -> usize {
        std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .max(1)
    }
}

impl Default for IdentConfig {
    fn default() -> Self {
        Self {
            fingerprinting: true,
            fingerprint_workers: Self::default_fingerprint_workers(),
            acoustid_disabled: false,
        }
    }
}
