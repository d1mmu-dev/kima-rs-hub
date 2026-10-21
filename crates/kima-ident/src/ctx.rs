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
#[derive(Clone, PartialEq, Eq)]
pub struct IdentConfig {
    /// ffmpeg was found at startup (spec §9.4): the fingerprint stage is offered.
    pub fingerprinting: bool,
    /// Decodes run at once (`KIMA_FINGERPRINT_WORKERS`, spec §9.3).
    pub fingerprint_workers: usize,
    /// `ACOUSTID_DISABLED=true`.
    pub acoustid_disabled: bool,
    /// `ACOUSTID_CLIENT_KEY`: second in the key order (spec §8.4). A secret.
    pub acoustid_env_key: Option<String>,
}

// Never prints the key.
impl std::fmt::Debug for IdentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentConfig")
            .field("fingerprinting", &self.fingerprinting)
            .field("fingerprint_workers", &self.fingerprint_workers)
            .field("acoustid_disabled", &self.acoustid_disabled)
            .field(
                "acoustid_env_key",
                &self.acoustid_env_key.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

impl IdentCtx {
    /// The AcoustID client key now: setting, env, default (spec §8.4).
    pub async fn acoustid_key(&self) -> Option<String> {
        crate::acoustid::resolve_client_key(&self.settings, self.config.acoustid_env_key.as_deref())
            .await
    }
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
            acoustid_env_key: None,
        }
    }
}
