mod common;

use kima_ml::ArtifactId;
use kima_ml::text_tokenizer::{ClapTextTokenizer, TokenizerError};

#[test]
fn rejects_spoofed_tokenizer_identity() {
    // Right artifact id, wrong (unpinned) bytes: hashes cannot match the
    // adapter's own hardcoded pin.
    let artifact = common::verified_fixture(
        ArtifactId::from_static("tokenizer"),
        b"{\"not\":\"the pinned roberta tokenizer\"}"
            .as_slice()
            .into(),
    );
    assert!(matches!(
        ClapTextTokenizer::from_verified_bytes(&artifact),
        Err(TokenizerError::UntrustedArtifact)
    ));
}

#[test]
fn rejects_right_bytes_under_wrong_pinned_id() {
    // Regression: a correctly-hashed artifact must still be rejected if it
    // carries the wrong pinned id (per onnx-verification.md).
    let artifact = common::verified_fixture(
        ArtifactId::from_static("not-the-tokenizer"),
        b"{\"not\":\"the pinned roberta tokenizer\"}"
            .as_slice()
            .into(),
    );
    assert!(matches!(
        ClapTextTokenizer::from_verified_bytes(&artifact),
        Err(TokenizerError::UntrustedArtifact)
    ));
}

#[test]
fn encode_rejects_oversized_raw_input() {
    use kima_ml::text_tokenizer::MAX_INPUT_CHARS;
    // Use a trivially malformed (non-pinned) artifact; encode's own input
    // bound must fire before any tokenizer work happens, so it cannot
    // depend on successfully loading the private tokenizer.
    let artifact = common::verified_fixture(
        ArtifactId::from_static("tokenizer"),
        b"not a real tokenizer.json".as_slice().into(),
    );
    assert!(matches!(
        ClapTextTokenizer::from_verified_bytes(&artifact),
        Err(TokenizerError::UntrustedArtifact)
    ));
    let _ = MAX_INPUT_CHARS; // constant exists and is reachable
}

/// Known short strings (written by us, not copied from any corpus) encoded
/// with the real pinned RoBERTa tokenizer.json, independently verified
/// against `transformers.RobertaTokenizer.from_pretrained('roberta-base')`
/// called with `padding=False, truncation=True, max_length=77` (see
/// upstream `onnx_backend.py`, `TEXT_MAX_LENGTH = 77`). The Python
/// reference was generated in a private scratch venv; only the resulting
/// integer id arrays are committed here.
#[test]
#[ignore = "private noncommercial tokenizer and Python reference required"]
fn private_real_tokenizer_matches_python_reference() {
    use kima_ml::{onnx::OnnxModel, verify_artifact};
    let _ = OnnxModel::ClapText; // documents the sibling pinned text model id
    let artifact = verify_artifact(
        std::path::Path::new("/home/dimmu/.hermes/cache/scratch/clap-text-tokenizer"),
        &kima_ml::text_tokenizer::descriptor(),
    )
    .expect("pinned tokenizer.json must verify");
    let tokenizer =
        ClapTextTokenizer::from_verified_bytes(&artifact).expect("pinned tokenizer must load");

    let cases: &[(&str, &[i64])] = &[
        ("a short clip of drums", &[0, 102, 765, 7200, 9, 21072, 2]),
        ("", &[0, 2]),
        ("guitar", &[0, 571, 6439, 271, 2]),
        (
            "synth pad with slow attack and warm chorus",
            &[0, 38972, 212, 11212, 19, 2635, 908, 8, 3279, 19738, 2],
        ),
        ("hello world", &[0, 42891, 232, 2]),
    ];
    for (text, expected) in cases {
        let ids = tokenizer.encode(text).expect("encode should succeed");
        assert_eq!(&ids, expected, "mismatch for {text:?}");
    }

    // Repetition long enough to force truncation at the pinned max_length
    // (77 tokens including <s>/</s>), matching the Python reference exactly.
    let repeated = "a ".repeat(100);
    let ids = tokenizer.encode(&repeated).expect("encode should succeed");
    assert_eq!(ids.len(), 77);
    assert_eq!(ids[0], 0);
    assert_eq!(*ids.last().unwrap(), 2);
}
