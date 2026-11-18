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
//! are unchecked, so neither is in Git. The feel probe is pinned (provisioned privately, D-194); the mood heads stay
//! unpinned (`None`) and therefore unavailable until their licences are cleared.

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
    /// The pin table compiled into the server; tests inject their own table.
    ///
    /// `feel_probe`: SHA-256 of upstream kima-hub c7527577 `backend/src/services/matching/feelProbe.json`
    /// (25 710 bytes, fitted on `laion-clap-music-onnx-v1`, `modelSha256` = the `music_audio.onnx` pin), equal to the
    /// git blob content and to the privately provisioned copy. `mood_heads`: no pin, blocked until the owner clears
    /// the training-data licences (Q3); an unpinned asset is never read.
    pub fn compiled() -> Self {
        Self {
            feel_probe: AssetPin {
                id: "vibe-feel-probe",
                basename: "feel_probe.json",
                sha256: Some([
                    0x13, 0x70, 0x7b, 0x51, 0x07, 0xa5, 0x91, 0xd2, 0xc7, 0xd5, 0xca, 0x7c, 0x14,
                    0x30, 0x05, 0x47, 0x7d, 0xb4, 0x1d, 0x5c, 0xec, 0x18, 0xec, 0xed, 0x78, 0xa9,
                    0x50, 0x39, 0x02, 0x04, 0x4e, 0x2d,
                ]),
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

/// Whether the compiled native audio encoder produces the matching vector space
/// (`MATCHING_SOUND`): the music checkpoint `music_audio` with pipeline `clap-music-audio-v1`.
/// Any other compiled identity (such as M4's general `hf_audio`) never makes music layouts or the
/// vibe feature possible.
pub fn native_audio_is_matching_sound() -> bool {
    use kima_ml::onnx::OnnxModel;
    let sound = kima_db::vibe::MATCHING_SOUND;
    OnnxModel::ClapAudio.artifact_id().as_str() == sound.model_id
        && OnnxModel::ClapAudio.expected_sha256() == sound.model_sha256
        && super::embedding::PIPELINE_VERSION == sound.pipeline_version
}

/// The native audio capability is available and embeds into the matching vector space.
pub fn matching_audio_available(ctx: &IdentCtx) -> bool {
    matching_audio_given(
        native_audio_is_matching_sound(),
        super::embedding::cached_status(ctx).as_ref(),
    )
}

/// [`matching_audio_available`] with the identity check and the cached capability report
/// supplied, so both halves of the predicate are unit-testable.
fn matching_audio_given(
    identity_matches: bool,
    report: Option<&CapabilityEvaluationReport>,
) -> bool {
    identity_matches
        && report.is_some_and(|s| {
            s.capabilities.get(&Capability::ClapAudioEmbedding)
                == Some(&CapabilityStatus::Available)
        })
}

/// Music-space vectors exist or can be produced: matching audio is available, or at least
/// one eligible vector is stored. Independent of the probe, which only the formula needs.
pub async fn music_vectors_possible(ctx: &IdentCtx) -> sqlx::Result<bool> {
    if matching_audio_available(ctx) {
        return Ok(true);
    }
    sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM {})",
        *kima_db::vibe::ELIGIBLE
    ))
    .fetch_one(&ctx.pool)
    .await
}

/// M5 spec §5.8: `vibeEmbeddings` = CLAP not disabled, a verified feel probe, and matching
/// audio or stored eligible vectors. Missing assets are capability failures, never startup
/// failures; text and mood heads are not prerequisites of the similarity capability.
pub async fn vibe_embeddings(ctx: &IdentCtx) -> sqlx::Result<bool> {
    if ctx.config.embedding.disabled {
        return Ok(false);
    }
    if !initialize(ctx).await.is_ok_and(|a| a.probe.is_some()) {
        return Ok(false);
    }
    music_vectors_possible(ctx).await
}

/// The cached result, without loading anything.
pub fn cached(ctx: &IdentCtx) -> Option<Arc<VibeAssets>> {
    ctx.embedding_runtime
        .vibe_assets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Replaces the cached assets (a test, or a caller that already holds them) and returns what is now cached. The mood
/// and similarity steps were built from the previous assets and start over (a running layout is stopped).
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
    if let Some(old) = ctx
        .embedding_runtime
        .vibe_similarity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        old.map.cancel();
    }
    assets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(audio: CapabilityStatus) -> CapabilityEvaluationReport {
        CapabilityEvaluationReport {
            capabilities: BTreeMap::from([(Capability::ClapAudioEmbedding, audio)]),
        }
    }

    /// The map's 202 layout-pending check (`routes/vibe.rs`) relies on this: an available
    /// encoder counts only when it embeds into the matching music space.
    #[test]
    fn matching_audio_needs_the_matching_identity_and_an_available_encoder() {
        let available = report(CapabilityStatus::Available);
        assert!(matching_audio_given(true, Some(&available)));
        // The compiled encoder is the music checkpoint, so an available encoder counts.
        assert!(matching_audio_given(
            native_audio_is_matching_sound(),
            Some(&available)
        ));
        // Any other identity never does, available or not.
        assert!(!matching_audio_given(false, Some(&available)));
        assert!(!matching_audio_given(true, None));
        let unavailable = CapabilityEvaluationReport {
            capabilities: BTreeMap::new(),
        };
        assert!(!matching_audio_given(true, Some(&unavailable)));
    }

    /// One code contract, not two guessed names: the vectors the embedding stage publishes
    /// (artifact id, pinned bytes, pipeline) are exactly the identity every vibe read accepts.
    #[test]
    fn the_native_encoder_publishes_the_matching_sound_identity() {
        use kima_ml::onnx::OnnxModel;
        let sound = kima_db::vibe::MATCHING_SOUND;
        assert_eq!(OnnxModel::ClapAudio.artifact_id().as_str(), sound.model_id);
        assert_eq!(OnnxModel::ClapAudio.expected_sha256(), sound.model_sha256);
        assert_eq!(
            super::super::embedding::PIPELINE_VERSION,
            sound.pipeline_version
        );
        assert!(native_audio_is_matching_sound());
        // The general M4 identity it replaces stays outside the engine.
        assert_ne!(sound.model_id, "hf_audio");
        assert_ne!(sound.pipeline_version, "clap-audio-v1");
    }

    /// The feel probe is pinned to the exact upstream c7527577 `feelProbe.json` bytes (private,
    /// CC BY-NC, never in Git); the mood heads stay unpinned until their licences are cleared.
    #[test]
    fn compiled_pins_pin_the_feel_probe_and_keep_mood_heads_blocked() {
        let pins = VibePins::compiled();
        let hex: String = pins
            .feel_probe
            .sha256
            .expect("feel probe pinned")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            hex,
            "13707b5107a591d2c7d5ca7c143005477db41d5cec18eced78a9503902044e2d"
        );
        assert!(pins.feel_probe.max_bytes >= 25_710);
        assert_eq!(pins.mood_heads.sha256, None, "mood heads remain blocked");
    }

    /// Whatever file sits in the asset directory, unpinned mood heads are never read: the
    /// capability reports provisioning pending while a pinned probe still loads.
    #[test]
    fn unpinned_mood_heads_stay_unavailable_next_to_a_verified_probe() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let probe = synthetic_probe();
        std::fs::write(dir.path().join("feel_probe.json"), &probe).unwrap();
        std::fs::write(dir.path().join("mood_heads.json"), b"{}").unwrap();
        let mut pins = VibePins::compiled();
        pins.feel_probe.sha256 = Some(Sha256::digest(&probe).into());
        let assets = load(Some(dir.path()), &pins);
        assert!(assets.probe.is_some());
        assert!(assets.heads.is_none());
        assert!(assets.report.is_available(Capability::VibeAssets));
        match assets.report.status(Capability::VibeMoodHeads) {
            CapabilityStatus::Unavailable { errors } => {
                assert!(
                    errors[0].message.contains("provisioning pending"),
                    "{errors:?}"
                )
            }
            other => panic!("mood heads must be unavailable: {other:?}"),
        }
    }

    fn synthetic_probe() -> Vec<u8> {
        let axis =
            |bias: f64| serde_json::json!({"alpha": 1.0, "bias": bias, "weights": vec![0.01; 512]});
        serde_json::to_vec(&serde_json::json!({
            "model": "laion-clap-music-onnx-v1",
            "valence": axis(0.1),
            "arousal": axis(-0.1),
        }))
        .unwrap()
    }
}
