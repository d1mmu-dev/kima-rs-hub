//! What every identification stage needs, built once per process.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kima_core::cipher::Cipher;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_db::settings_kv::SettingsCache;
use kima_library::scan::LibraryCtx;
use kima_net::Net;

use crate::events::ProgressEvents;
use crate::stages::embedding::EmbeddingRuntime;

/// Cached result of initializing the CLAP audio runtime.
#[derive(Default)]
pub struct EmbeddingRuntimeState {
    pub(crate) runtime:
        Mutex<Option<Result<Arc<EmbeddingRuntime>, kima_ml::CapabilityEvaluationReport>>>,
    pub(crate) text_runtime: Mutex<
        Option<
            Result<Arc<crate::stages::embedding::TextRuntime>, kima_ml::CapabilityEvaluationReport>,
        >,
    >,
    pub(crate) workers: std::sync::OnceLock<Arc<tokio::sync::Semaphore>>,
    /// Private snapshot directory, pinned on first successful use.
    pub(crate) snapshots: Mutex<Option<Arc<kima_ml::input_snapshot::SnapshotStore>>>,
    /// Last library root checked against the snapshot directory's mount, with the reason it
    /// cannot be used (`None`: fine). Re-evaluated when `LibraryRoot::set` changes the root.
    pub(crate) root_check: Mutex<Option<(PathBuf, Option<String>)>>,
    /// First global snapshot failure, latched until restart; never affects text.
    pub(crate) audio_infrastructure_failure: Mutex<Option<String>>,
    /// The vibe engine's private assets (feel probe, mood heads), loaded once per process; kept here with the
    /// other loaded-once model state so no `IdentCtx` constructor changes.
    pub(crate) vibe_assets: Mutex<Option<Arc<crate::stages::vibe_assets::VibeAssets>>>,
}

pub type EmbeddingRuntimeCache = Arc<EmbeddingRuntimeState>;

/// Cached result of initializing the analysis (MusiCNN) runtime, mirroring
/// [`EmbeddingRuntimeState::runtime`]: `None` until first use, then the same
/// success or failure for the rest of the process's life.
#[derive(Default)]
pub struct AnalysisRuntimeState {
    pub(crate) runtime: Mutex<
        Option<
            Result<
                Arc<crate::stages::analysis::AnalysisRuntime>,
                kima_ml::CapabilityEvaluationReport,
            >,
        >,
    >,
    /// Reserved for a future per-process semaphore cache mirroring
    /// `EmbeddingRuntimeState::workers`; `run_claim_stopping` still allocates a fresh
    /// `Semaphore` per claim today (tracked as an open item, not part of this fix round).
    #[allow(dead_code)]
    pub(crate) workers: std::sync::OnceLock<Arc<tokio::sync::Semaphore>>,
}

pub type AnalysisRuntimeCache = Arc<AnalysisRuntimeState>;

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
    /// When the last enrichment cycle started (spec §10.6 `MIN_INTERVAL_MS`); shared by clones.
    pub last_cycle: Arc<Mutex<Option<Instant>>>,
    /// `enrichment:progress` on [`Self::events`]'s bus, at most once a second (spec §17); shared
    /// with the HTTP side's controls so both draw on one window.
    pub progress_events: ProgressEvents,
    /// Cached CLAP initialization result, shared by all context clones until restart.
    pub embedding_runtime: EmbeddingRuntimeCache,
    /// Cached MusiCNN/analysis initialization result, shared by all context clones.
    pub analysis_runtime: AnalysisRuntimeCache,
    /// Until when AcoustID batch requests wait after one failed (shared by clones).
    pub acoustid_hold: crate::acoustid_batch::BatchHold,
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
    /// `FANART_API_KEY`: used when no enabled key is stored (spec §15.1). A secret.
    pub fanart_env_key: Option<String>,
    /// One artist's whole enrichment, album covers included
    /// ([`crate::artist::ARTIST_TIMEOUT`] in production; tests shorten it).
    pub artist_timeout: Duration,
    /// The CLAP embedding stage (M4).
    pub embedding: EmbeddingConfig,
    /// The audio analysis stage (M4).
    pub analysis: AnalysisConfig,
    /// The vibe engine (M5).
    pub vibe: VibeConfig,
}

/// Configuration of the vibe engine (M5), parsed once in `kima_core::Config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VibeConfig {
    /// `KIMA_VIBE_ASSET_DIR`: directory with the private feel probe and mood-head files. `None` turns the
    /// `vibe_assets` and `vibe_mood_heads` capabilities off.
    pub asset_dir: Option<PathBuf>,
    /// `KIMA_VIBE_EXACT_MAX`: text search scans exactly up to this many eligible tracks, then uses the HNSW index.
    pub exact_max: u64,
}

impl Default for VibeConfig {
    fn default() -> Self {
        Self {
            asset_dir: None,
            exact_max: kima_core::config::DEFAULT_VIBE_EXACT_MAX,
        }
    }
}

/// Configuration for `Stage::Analysis` (M4): the MusiCNN ONNX model directory and
/// session pool, independent of [`IdentConfig::fingerprinting`] and MusicBrainz.
///
/// A plain structural default (no environment access), mirroring [`EmbeddingConfig`]'s
/// own `Default`: the `KIMA_MUSICNN_MODEL_DIR`/`DISABLE_MUSICNN` environment variables are
/// parsed once in `kima_core::Config` and threaded into this struct explicitly at startup
/// (`kima-server/src/main.rs`), the same way CLAP's `KIMA_CLAP_MODEL_DIR`/`DISABLE_CLAP` are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisConfig {
    /// Directory holding the verified MusiCNN models. `None` disables the
    /// `MusicnnAnalysis` capability (degrading to DSP-only analysis).
    pub model_dir: Option<PathBuf>,
    /// Explicit operator override that skips MusiCNN model loading.
    pub disabled: bool,
    /// BoundedSessionPool size for the MusiCNN ONNX sessions.
    pub pool_size: usize,
    /// Maximum simultaneous blocking analysis workers.
    pub workers: usize,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            model_dir: None,
            disabled: false,
            pool_size: 1,
            workers: 1,
        }
    }
}

/// Configuration for `Stage::Embedding` (M4): the CLAP audio ONNX model directory and a
/// bounded session pool, independent of [`IdentConfig::fingerprinting`] and MusicBrainz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    /// Directory holding the verified `hf_audio.onnx` artifact. `None` disables the
    /// `ClapAudioEmbedding` capability, without claiming work.
    pub model_dir: Option<PathBuf>,
    /// Explicit operator override that skips model loading.
    pub disabled: bool,
    /// `BoundedSessionPool` size for the CLAP ONNX session (bounded, never one session per
    /// claim).
    pub pool_size: usize,
    /// Maximum simultaneous blocking embeddings shared by clones of one context, including
    /// work still running after cancellation. Fixed when first used; separate contexts have
    /// independent limits.
    pub workers: usize,
    /// Private (0700, server-owned) snapshot directory on the library's mount. `None` makes
    /// no audio embedding is claimed and the capability reports why.
    pub snapshot_dir: Option<PathBuf>,
    /// Set by the startup check when the snapshot setup is unusable: blocks AUDIO embedding
    /// only (status `unavailable` with this reason); CLAP text and `disabled` are unaffected.
    pub audio_blocked_reason: Option<String>,
    /// Input-size and concurrent-snapshot limits (safe `kima-ml` defaults).
    pub snapshot_limits: kima_ml::input_snapshot::SnapshotLimits,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_dir: None,
            disabled: false,
            pool_size: 1,
            workers: 1,
            snapshot_dir: None,
            audio_blocked_reason: None,
            snapshot_limits: kima_ml::input_snapshot::SnapshotLimits::default(),
        }
    }
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
            .field(
                "fanart_env_key",
                &self.fanart_env_key.as_ref().map(|_| ".."),
            )
            .field("artist_timeout", &self.artist_timeout)
            .field("vibe", &self.vibe)
            .finish()
    }
}

impl IdentCtx {
    /// The AcoustID client key now: setting, env, default (spec §8.4).
    pub async fn acoustid_key(&self) -> Option<String> {
        crate::acoustid::resolve_client_key(&self.settings, self.config.acoustid_env_key.as_deref())
            .await
    }

    /// The fanart.tv key now: the stored key while enabled, else env (spec §15.1).
    pub async fn fanart_key(&self) -> Option<String> {
        crate::fanart::resolve_key(&self.settings, self.config.fanart_env_key.as_deref()).await
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
            fanart_env_key: None,
            artist_timeout: crate::artist::ARTIST_TIMEOUT,
            embedding: EmbeddingConfig::default(),
            analysis: AnalysisConfig::default(),
            vibe: VibeConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_both_keys() {
        let config = IdentConfig {
            acoustid_env_key: Some("acoustid-secret-value".into()),
            fanart_env_key: Some("fanart-secret-value".into()),
            ..IdentConfig::default()
        };
        let shown = format!("{config:?} {config:#?}");
        for secret in ["acoustid-secret-value", "fanart-secret-value"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(shown.contains("fanart_env_key: Some(\"..\")"), "{shown}");
    }
}
