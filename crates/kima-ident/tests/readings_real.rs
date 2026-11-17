//! Private real-model acceptance, not evidence from synthetic vectors. No database or library writes.
mod common;
use kima_ident::stages::embedding::{NativeTextEmbedder, TextEmbedder, vibe_embed_text};
use kima_vibe::zero_shot::{PROMPTS, Readings};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, io::Read, os::unix::fs::OpenOptionsExt};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    upstream: String,
    analyzer_sha256: String,
    models: std::collections::HashMap<String, String>,
    tracks: Vec<Track>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    path: String,
    sha256: String,
    license: String,
    audio_vector: Vec<f32>,
    expected: [f64; 4],
}
fn safe_bytes(path: &str, limit: u64) -> Vec<u8> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .expect("private regular file");
    let meta = file.metadata().unwrap();
    assert!(meta.is_file() && meta.len() <= limit);
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() as u64 <= limit);
    bytes
}
fn validate(r: &Reference) {
    assert_eq!(r.upstream, "c7527577d925b20f5adf410c7fd4e0c8ac250f69");
    assert_eq!(
        r.analyzer_sha256,
        "3f8d494b7321ca9ea245cdc3208f68d6ac753c0d49f0cc79d69730a047c03b26"
    );
    assert_eq!(
        r.models.get("music_audio.onnx").map(String::as_str),
        Some("ab1bf5eba067b246198c630f153f774e8f1c3169b3924d4d6a4dc247bd73de97")
    );
    assert_eq!(
        r.models.get("music_text.onnx").map(String::as_str),
        Some("6dbba074b27f8973d84c5c7ae9acd0b9139a6cd9dd82bea6e87954dc3b835956")
    );
    assert_eq!(r.tracks.len(), 20, "exactly 20 predeclared licensed tracks");
    assert_eq!(
        r.tracks
            .iter()
            .map(|t| &t.sha256)
            .collect::<HashSet<_>>()
            .len(),
        20
    );
    for t in &r.tracks {
        assert!(!t.license.trim().is_empty());
        assert_eq!(t.audio_vector.len(), 512);
        assert!(
            t.expected
                .iter()
                .all(|v| v.is_finite() && (0. ..=1.).contains(v))
        );
        assert!(t.audio_vector.iter().all(|v| v.is_finite()));
        let norm = t
            .audio_vector
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.).abs() < 1e-3);
    }
}
#[test]
fn real_reference_refuses_empty_or_synthetic_substitution() {
    let r = Reference {
        upstream: "synthetic".into(),
        analyzer_sha256: String::new(),
        models: Default::default(),
        tracks: vec![],
    };
    assert!(std::panic::catch_unwind(|| validate(&r)).is_err());
}

#[tokio::test]
#[ignore = "requires licensed private music checkpoint, 20 real audio files and pinned Python reference; NOT RUN"]
async fn real_music_readings_match_upstream_python_on_twenty_tracks() {
    let model = std::env::var("KIMA_CLAP_MODEL_DIR").expect("KIMA_CLAP_MODEL_DIR required");
    let fixture = std::env::var("KIMA_ZERO_SHOT_REFERENCE")
        .expect("private upstream Python reference required");
    assert_eq!(
        std::env::var("KIMA_ZERO_SHOT_LICENSED_PRIVATE_USE").as_deref(),
        Ok("1")
    );
    assert!(std::path::Path::new(&model).is_absolute());
    let r: Reference = serde_json::from_slice(&safe_bytes(&fixture, 2_000_000)).unwrap();
    validate(&r);
    assert!(
        NativeTextEmbedder.readings_identity().is_some(),
        "native encoder is not the pinned music checkpoint; hf_audio is forbidden"
    );
    // Text inference needs a context but never opens this deliberately unreachable pool.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = common::ident_ctx(&pool, dir.path(), common::dead_bases());
    ctx.config.embedding.model_dir = Some(model.into());
    ctx.config.embedding.disabled = false;
    let mut prompts = Vec::new();
    for (_, group) in PROMPTS {
        for text in group {
            prompts.push(
                vibe_embed_text(&ctx, text, &NativeTextEmbedder)
                    .await
                    .expect("actual verified native text inference"),
            );
        }
    }
    for t in &r.tracks {
        assert_eq!(
            hex::encode(Sha256::digest(safe_bytes(&t.path, 2_000_000_000))),
            t.sha256,
            "reference audio identity"
        );
        let audio: [f32; 512] = t.audio_vector.clone().try_into().unwrap();
        let got = Readings::compute(&audio, &prompts).unwrap();
        for (actual, expected) in [
            got.instrumentalness,
            got.acousticness,
            got.valence_raw,
            got.arousal_raw,
        ]
        .into_iter()
        .zip(t.expected)
        {
            assert!(
                (actual - expected).abs() <= 1e-3 + 1e-12,
                "{}: {actual} != {expected}",
                t.sha256
            );
        }
    }
}
