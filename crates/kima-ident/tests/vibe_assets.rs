//! Loading of the private vibe assets (feel probe, mood heads): hash-pinned, fail-closed, per-capability.
//! Synthetic files only; the real assets are outside Git (owner decision Q3).
use std::os::unix::fs::symlink;
use std::path::Path;

use kima_ident::stages::vibe_assets::{AssetPin, VibePins, load, load_with_hook};
use kima_ml::{Capability, CapabilityStatus, FailureCategory};
use serde_json::json;
use sha2::{Digest, Sha256};

const MOODS: [&str; 22] = [
    "happy",
    "fun_quirky",
    "uplifting",
    "sad",
    "nostalgic",
    "dark",
    "trippy",
    "calm",
    "dreamy",
    "energetic",
    "aggressive",
    "epic",
    "party",
    "groovy",
    "romantic",
    "sexy",
    "summer",
    "night",
    "driving",
    "workout",
    "focus",
    "sleep",
];

fn probe_bytes() -> Vec<u8> {
    let axis = |s: usize| json!({"alpha": 80.0, "bias": 0.5, "weights": (0..512).map(|i| ((i + s) % 7) as f64 / 7.0 - 0.4).collect::<Vec<_>>()});
    serde_json::to_vec(&json!({"model": "synthetic-model", "modelSha256": "00", "valence": axis(1), "arousal": axis(2)})).unwrap()
}

fn heads_bytes() -> Vec<u8> {
    let moods: serde_json::Map<String, serde_json::Value> =
        MOODS.iter().enumerate().map(|(n, m)| (m.to_string(), json!({"w": (0..512).map(|i| ((i + n) % 5) as f64 / 5.0 - 0.3).collect::<Vec<_>>(), "b": 0.1}))).collect();
    serde_json::to_vec(&json!({"version": "heads-v1", "model": "synthetic-model", "dims": 512, "order": MOODS, "moods": moods})).unwrap()
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn pins(probe: &[u8], heads: &[u8]) -> VibePins {
    VibePins {
        feel_probe: AssetPin {
            id: "feel-probe",
            basename: "feel_probe.json",
            sha256: Some(sha(probe)),
            max_bytes: 1 << 20,
        },
        mood_heads: AssetPin {
            id: "mood-heads",
            basename: "mood_heads.json",
            sha256: Some(sha(heads)),
            max_bytes: 4 << 20,
        },
    }
}

fn dir_with(probe: Option<&[u8]>, heads: Option<&[u8]>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    if let Some(p) = probe {
        std::fs::write(dir.path().join("feel_probe.json"), p).unwrap();
    }
    if let Some(h) = heads {
        std::fs::write(dir.path().join("mood_heads.json"), h).unwrap();
    }
    dir
}

fn failure(
    a: &kima_ident::stages::vibe_assets::VibeAssets,
    cap: Capability,
) -> (FailureCategory, String) {
    match a.report.status(cap) {
        CapabilityStatus::Unavailable { errors } => (errors[0].category, errors[0].message.clone()),
        other => panic!("{cap:?} should be unavailable, was {other:?}"),
    }
}

#[test]
fn loads_both_assets_when_pinned_and_present() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let dir = dir_with(Some(&p), Some(&h));
    let a = load(Some(dir.path()), &pins(&p, &h));
    assert!(
        a.report.is_available(Capability::VibeAssets)
            && a.report.is_available(Capability::VibeMoodHeads)
    );
    assert_eq!(a.probe.as_ref().unwrap().model, "synthetic-model");
    assert_eq!(a.heads.as_ref().unwrap().version, "heads-v1");
    assert_eq!(a.probe.as_ref().unwrap().probe_id.len(), 16);
}

#[test]
fn assets_missing_disables_only_vibe_assets() {
    let a = load(None, &VibePins::compiled());
    for cap in [Capability::VibeAssets, Capability::VibeMoodHeads] {
        let (category, message) = failure(&a, cap);
        assert_eq!(category, FailureCategory::Missing);
        assert!(
            message.contains("KIMA_VIBE_ASSET_DIR is not configured"),
            "{message}"
        );
    }
    assert!(a.probe.is_none() && a.heads.is_none());
    // Nothing else is in the report: the CLAP capabilities are not touched by this loader.
    assert_eq!(
        a.report.capabilities.keys().copied().collect::<Vec<_>>(),
        [Capability::VibeAssets, Capability::VibeMoodHeads]
    );
}

#[test]
fn missing_heads_disable_only_moods() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let dir = dir_with(Some(&p), None);
    let a = load(Some(dir.path()), &pins(&p, &h));
    assert!(
        a.report.is_available(Capability::VibeAssets),
        "similarity does not need the heads"
    );
    assert_eq!(
        failure(&a, Capability::VibeMoodHeads).0,
        FailureCategory::Missing
    );
    assert!(a.probe.is_some() && a.heads.is_none());
}

#[test]
fn unpinned_asset_is_unavailable_with_a_reason() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let dir = dir_with(Some(&p), Some(&h));
    let a = load(Some(dir.path()), &VibePins::compiled());
    let (_, message) = failure(&a, Capability::VibeAssets);
    assert!(message.contains("no SHA-256 pin"), "{message}");
    assert!(
        a.probe.is_none(),
        "an unpinned file is never parsed, even when it looks right"
    );
}

#[test]
fn sha_mismatch_is_unavailable_not_panic() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let mut other = p.clone();
    other.push(b' ');
    let dir = dir_with(Some(&other), Some(&h));
    let a = load(Some(dir.path()), &pins(&p, &h));
    assert_eq!(
        failure(&a, Capability::VibeAssets).0,
        FailureCategory::HashMismatch
    );
    assert!(a.probe.is_none());
    assert!(
        a.report.is_available(Capability::VibeMoodHeads),
        "the other asset is judged on its own"
    );
}

#[test]
fn fifo_or_symlink_final_component_rejected() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let dir = dir_with(None, None);
    let real = tempfile::tempdir().unwrap();
    std::fs::write(real.path().join("target.json"), &p).unwrap();
    symlink(
        real.path().join("target.json"),
        dir.path().join("feel_probe.json"),
    )
    .unwrap();
    let fifo = dir.path().join("mood_heads.json");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let a = load(Some(dir.path()), &pins(&p, &h));
    assert_eq!(
        failure(&a, Capability::VibeAssets).0,
        FailureCategory::SymlinkRefused
    );
    assert_eq!(
        failure(&a, Capability::VibeMoodHeads).0,
        FailureCategory::NotRegular
    );
}

#[test]
fn verified_bytes_are_the_parsed_bytes() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let dir = dir_with(Some(&p), Some(&h));
    // After the hash check, replace the file with different (still valid) content: the parser must see the old bytes.
    let mut replaced = false;
    let a = load_with_hook(Some(dir.path()), &pins(&p, &h), &mut |path: &Path| {
        if path.ends_with("feel_probe.json") && !replaced {
            let mut other = serde_json::from_slice::<serde_json::Value>(&p).unwrap();
            other["model"] = json!("swapped-after-verification");
            std::fs::write(path, serde_json::to_vec(&other).unwrap()).unwrap();
            replaced = true;
        }
    });
    assert!(replaced, "the hook ran");
    assert_eq!(a.probe.as_ref().unwrap().model, "synthetic-model");
}

#[test]
fn oversized_file_rejected() {
    let (p, h) = (probe_bytes(), heads_bytes());
    let dir = dir_with(Some(&p), Some(&h));
    let mut small = pins(&p, &h);
    small.feel_probe.max_bytes = 100;
    let a = load(Some(dir.path()), &small);
    assert_eq!(
        failure(&a, Capability::VibeAssets).0,
        FailureCategory::TooLarge
    );
}

#[test]
fn verified_but_malformed_content_is_unavailable_with_the_parser_message() {
    let h = heads_bytes();
    let junk = br#"{"model":"x"}"#.to_vec();
    let dir = dir_with(Some(&junk), Some(&h));
    let a = load(Some(dir.path()), &pins(&junk, &h));
    let (category, message) = failure(&a, Capability::VibeAssets);
    assert_eq!(category, FailureCategory::ConsumerLoadFailed);
    assert!(message.contains("Feel probe:"), "{message}");
}

mod common;

#[tokio::test]
async fn capabilities_reach_the_ml_report_and_clap_is_unaffected() {
    use kima_net::Bases;
    let dir = tempfile::tempdir().unwrap();
    let pool = kima_db::PgPool::connect_lazy("postgres://unused:***@127.0.0.1/unused").unwrap();
    let ctx = common::ident_ctx(&pool, dir.path(), Bases::production("http://127.0.0.1:9"));
    assert!(kima_ident::stages::embedding::cached_status(&ctx).is_none());

    let clap_only = kima_ident::stages::embedding::initialize(&ctx)
        .await
        .unwrap();
    assert!(
        !clap_only.capabilities.contains_key(&Capability::VibeAssets),
        "nothing is reported before the assets are loaded"
    );

    let vibe = kima_ident::stages::vibe_assets::initialize(&ctx)
        .await
        .unwrap();
    assert!(!vibe.report.is_available(Capability::VibeAssets));
    let both = kima_ident::stages::embedding::cached_status(&ctx).unwrap();
    for cap in [Capability::VibeAssets, Capability::VibeMoodHeads] {
        assert!(
            matches!(both.status(cap), CapabilityStatus::Unavailable { .. }),
            "{cap:?}"
        );
    }
    for cap in [
        Capability::ClapAudioEmbedding,
        Capability::ClapTextEmbedding,
    ] {
        assert_eq!(
            both.status(cap),
            clap_only.status(cap),
            "{cap:?} is unaffected by missing vibe assets"
        );
    }
    let json = serde_json::to_value(&both).unwrap();
    assert_eq!(json["capabilities"]["vibe_assets"]["state"], "unavailable");
    // Loaded once: a second call returns the same Arc.
    let again = kima_ident::stages::vibe_assets::initialize(&ctx)
        .await
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&vibe, &again));
}
