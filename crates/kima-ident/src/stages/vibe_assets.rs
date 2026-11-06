//! The vibe engine's private assets (M5): the feel probe and the 22 mood heads.
//!
//! Both are loaded from `KIMA_VIBE_ASSET_DIR` with the M4 artifact pattern (`kima_ml::verify_artifact`: open with
//! `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, `fstat` must say regular file, bounded read, SHA-256 against a compiled
//! pin), and the bytes that were verified are the bytes that are parsed. Each asset is its own capability, so a
//! missing mood-head file turns moods off without touching similarity (spec 4.4):
//! `vibe_assets` = the feel probe, `vibe_mood_heads` = the heads. A missing directory, a missing pin, a missing
//! file, a hash mismatch or a malformed file makes the capability `unavailable` with the reason; nothing panics and
//! the server boots.
//!
//! Rights (owner decision Q3, 2026-11-05): `feelProbe.json` is CC BY-NC and the mood heads' training-data licences
//! are unchecked, so neither is in Git; the pins below stay `None` until the owner provisions the files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kima_ml::{
    ArtifactDescriptor, ArtifactFailure, ArtifactId, Capability, CapabilityEvaluationReport,
    CapabilityStatus, FailureCategory, VerifiedBytes, verify_artifact,
};
use kima_vibe::feel_probe::{FeelProbe, parse_feel_probe};
use kima_vibe::mood_heads::{MoodHeads, parse_mood_heads};

use crate::IdentCtx;

/// One expected asset file. `sha256: None` means "not provisioned": the asset is reported unavailable and is never
/// read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPin {
    pub id: &'static str,
    pub basename: &'static str,
    pub sha256: Option<[u8; 32]>,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VibePins {
    pub feel_probe: AssetPin,
    pub mood_heads: AssetPin,
}

impl VibePins {
    /// The pin table compiled into the server. The hashes are filled in when the owner provisions the files
    /// (Q3); tests inject their own table.
    pub fn compiled() -> Self {
        Self {
            feel_probe: AssetPin {
                id: "vibe-feel-probe",
                basename: "feel_probe.json",
                sha256: None,
                max_bytes: 1 << 20,
            },
            mood_heads: AssetPin {
                id: "vibe-mood-heads",
                basename: "mood_heads.json",
                sha256: None,
                max_bytes: 8 << 20,
            },
        }
    }
}

/// What loading found: the parsed assets that are usable and the per-capability report.
#[derive(Debug, Clone)]
pub struct VibeAssets {
    pub probe: Option<Arc<FeelProbe>>,
    pub heads: Option<Arc<MoodHeads>>,
    pub report: CapabilityEvaluationReport,
}

fn failure(
    id: &'static str,
    category: FailureCategory,
    message: impl Into<String>,
) -> ArtifactFailure {
    ArtifactFailure {
        artifact_id: ArtifactId::from_static(id),
        category,
        message: message.into(),
    }
}

fn load_one<T>(
    dir: Option<&Path>,
    pin: &AssetPin,
    hook: &mut dyn FnMut(&Path),
    parse: impl FnOnce(&VerifiedBytes) -> Result<T, String>,
) -> Result<T, ArtifactFailure> {
    let Some(dir) = dir else {
        return Err(failure(
            pin.id,
            FailureCategory::Missing,
            "KIMA_VIBE_ASSET_DIR is not configured",
        ));
    };
    let Some(expected_sha256) = pin.sha256 else {
        return Err(failure(
            pin.id,
            FailureCategory::Missing,
            "no SHA-256 pin is compiled in for this asset (provisioning pending)",
        ));
    };
    let descriptor = ArtifactDescriptor {
        id: ArtifactId::from_static(pin.id),
        basename: pin.basename,
        expected_sha256,
        max_bytes: pin.max_bytes,
    };
    let verified = verify_artifact(dir, &descriptor).map_err(ArtifactFailure::from)?;
    hook(&dir.join(pin.basename));
    parse(&verified)
        .map_err(|message| failure(pin.id, FailureCategory::ConsumerLoadFailed, message))
}

/// [`load`] with a hook that runs between hash verification and parsing (tests replace the file there to show that
/// the parser sees the verified bytes).
pub fn load_with_hook(
    dir: Option<&Path>,
    pins: &VibePins,
    hook: &mut dyn FnMut(&Path),
) -> VibeAssets {
    let probe = load_one(dir, &pins.feel_probe, hook, |b| {
        parse_feel_probe(b.as_bytes()).map_err(|e| e.to_string())
    });
    let heads = load_one(dir, &pins.mood_heads, hook, |b| {
        parse_mood_heads(b.as_bytes()).map_err(|e| e.to_string())
    });
    fn status<T>(r: &Result<T, ArtifactFailure>) -> CapabilityStatus {
        match r {
            Ok(_) => CapabilityStatus::Available,
            Err(f) => CapabilityStatus::Unavailable {
                errors: vec![f.clone()],
            },
        }
    }
    let mut capabilities = BTreeMap::new();
    capabilities.insert(Capability::VibeAssets, status(&probe));
    capabilities.insert(Capability::VibeMoodHeads, status(&heads));
    VibeAssets {
        probe: probe.ok().map(Arc::new),
        heads: heads.ok().map(Arc::new),
        report: CapabilityEvaluationReport { capabilities },
    }
}

/// Reads and verifies both assets from `dir` (`KIMA_VIBE_ASSET_DIR`). Blocking file I/O.
pub fn load(dir: Option<&Path>, pins: &VibePins) -> VibeAssets {
    load_with_hook(dir, pins, &mut |_| {})
}

/// Loads the assets once per process (cached in the shared runtime state, like the CLAP runtimes) on a blocking
/// thread, and returns them. Later calls return the cached result.
pub async fn initialize(ctx: &IdentCtx) -> Result<Arc<VibeAssets>, tokio::task::JoinError> {
    let ctx = ctx.clone();
    tokio::task::spawn_blocking(move || assets_now(&ctx)).await
}

fn assets_now(ctx: &IdentCtx) -> Arc<VibeAssets> {
    let mut slot = ctx
        .embedding_runtime
        .vibe_assets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(loaded) = slot.as_ref() {
        return loaded.clone();
    }
    let dir: Option<PathBuf> = ctx.config.vibe.asset_dir.clone();
    let loaded = Arc::new(load(dir.as_deref(), &VibePins::compiled()));
    *slot = Some(loaded.clone());
    loaded
}

/// The cached result, without loading anything.
pub fn cached(ctx: &IdentCtx) -> Option<Arc<VibeAssets>> {
    ctx.embedding_runtime
        .vibe_assets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Replaces the cached assets (a test, or a caller that already holds them) and returns what is now cached.
pub fn install(ctx: &IdentCtx, assets: VibeAssets) -> Arc<VibeAssets> {
    let assets = Arc::new(assets);
    *ctx.embedding_runtime
        .vibe_assets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(assets.clone());
    // The mood step was built from the previous heads.
    *ctx.embedding_runtime
        .vibe_moods
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    assets
}
