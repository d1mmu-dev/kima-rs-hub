//! Private real-model parity of the 24 CLAP zero-shot reading prompts (ignored: the noncommercial weights and the
//! private Python reference are never in Git or CI). Checks that the prompts kima-rs embeds are upstream's, text
//! for text and in group order (`services/audio-analyzer-clap/analyzer.py` `PROMPTS` at c7527577, parsed by the
//! reference generator), and that the production text path (`NativeTextEmbedder`: pinned tokenizer, pinned
//! `music_text.onnx`, L2) reproduces upstream Python's `MusicClapText` vector for every one of them.
//!
//! Environment (every gap panics, so a skip can never read as a pass): `KIMA_CLAP_MODEL_DIR`,
//! `KIMA_MUSIC_CLAP_PYTHON_REFERENCE` (v2 reference with `reading_groups`/`reading_prompts`/`versions`),
//! `ORT_DYLIB_PATH`. No database is used (the context's pool is lazy and never connects).

mod common;

use kima_core::identify::IdentifySwitch;
use kima_ident::stages::embedding::{NativeTextEmbedder, TextEmbedder};
use kima_vibe::zero_shot::PROMPTS;
use serde_json::Value;

fn env_path(key: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set")),
    );
    assert!(path.is_absolute(), "{key} must be absolute");
    path
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "private noncommercial music CLAP models and a private Python reference required"]
async fn every_reading_prompt_is_upstreams_and_embeds_like_upstream_python() {
    let reference: Value = serde_json::from_slice(
        &std::fs::read(env_path("KIMA_MUSIC_CLAP_PYTHON_REFERENCE")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        reference["upstream"],
        "c7527577d925b20f5adf410c7fd4e0c8ac250f69"
    );
    // The reference parsed the prompts from exactly upstream's analyzer.py at that commit (sha256 of
    // `git show c7527577:services/audio-analyzer-clap/analyzer.py`), not from a local or later copy.
    assert_eq!(
        reference["analyzer_sha256"],
        "3f8d494b7321ca9ea245cdc3208f68d6ac753c0d49f0cc79d69730a047c03b26",
        "reference generated from another analyzer.py"
    );
    println!("python reference versions: {}", reference["versions"]);

    // Text parity: the same 8 groups in the same order, the same 3 prompts each, byte for byte.
    let groups: Vec<&str> = reference["reading_groups"]
        .as_array()
        .expect("v2 reference with reading_groups")
        .iter()
        .map(|g| g.as_str().unwrap())
        .collect();
    assert_eq!(
        groups,
        PROMPTS.iter().map(|(g, _)| *g).collect::<Vec<_>>(),
        "group order"
    );
    let upstream: Vec<&str> = reference["reading_prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    let native: Vec<&str> = PROMPTS
        .iter()
        .flat_map(|(_, p)| p.iter().copied())
        .collect();
    assert_eq!(native.len(), 24);
    assert_eq!(native, upstream, "prompt texts");

    // Vector parity through the production text path.
    let dir = tempfile::tempdir().unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .unwrap();
    let ctx = common::ident_ctx_with_embedding(
        &pool,
        dir.path(),
        common::dead_bases(),
        IdentifySwitch::new(false, false),
        kima_ident::EmbeddingConfig {
            model_dir: Some(env_path("KIMA_CLAP_MODEL_DIR")),
            ..Default::default()
        },
    );
    let cases = reference["text"].as_array().unwrap();
    let mut worst = (1.0_f64, 0.0_f64);
    for prompt in &native {
        let case = cases
            .iter()
            .find(|c| c["prompt"] == *prompt)
            .unwrap_or_else(|| panic!("no Python vector for {prompt:?}"));
        let expected: Vec<f64> = case["embedding"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        assert_eq!(expected.len(), 512);
        let actual = NativeTextEmbedder.embed(&ctx, prompt).await.unwrap();
        let (mut dot, mut a, mut b, mut max) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        for (&x, &y) in actual.iter().zip(&expected) {
            let x = f64::from(x);
            let y = f64::from(y as f32);
            dot += x * y;
            a += x * x;
            b += y * y;
            max = max.max((x - y).abs());
        }
        let cos = dot / (a.sqrt() * b.sqrt());
        println!("reading prompt {prompt:?}: cosine={cos:.9} max_abs={max:.3e}");
        worst = (worst.0.min(cos), worst.1.max(max));
    }
    println!(
        "reading prompts worst: cosine={:.9} max_abs={:.3e}",
        worst.0, worst.1
    );
    assert!(worst.0 >= 0.99999 && worst.1 < 1e-4, "{worst:?}");
}
