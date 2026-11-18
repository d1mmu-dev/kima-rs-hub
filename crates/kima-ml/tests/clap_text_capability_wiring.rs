use kima_ml::onnx::OnnxModel;
use kima_ml::{Capability, CapabilityConfig, CapabilityStatus, evaluate_capabilities};
use std::collections::BTreeMap;

/// The private music CLAP model directory (`music_text.onnx` + `tokenizer.json`); panics when unset.
fn private_model_dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(
        std::env::var("KIMA_CLAP_MODEL_DIR").expect("KIMA_CLAP_MODEL_DIR must be set"),
    );
    assert!(dir.is_absolute(), "KIMA_CLAP_MODEL_DIR must be absolute");
    dir
}

/// Exercises the real production wiring end to end: both the pinned
/// `tokenizer.json` and `music_text.onnx` must independently verify before
/// `Capability::ClapTextEmbedding` reports `Available`. Uses only the
/// private, hash-pinned local model cache (never committed, never
/// downloaded by this test).
#[test]
#[ignore = "private noncommercial models required"]
fn clap_text_capability_requires_both_tokenizer_and_text_model() {
    let full_reqs = BTreeMap::from([(
        Capability::ClapTextEmbedding,
        OnnxModel::requirements_by_capability()
            .remove(&Capability::ClapTextEmbedding)
            .unwrap(),
    )]);
    let report = evaluate_capabilities(
        &private_model_dir(),
        &CapabilityConfig::default(),
        &full_reqs,
    );
    assert_eq!(
        report.status(Capability::ClapTextEmbedding),
        &CapabilityStatus::Available,
        "expected Available with both pinned artifacts present: {:?}",
        report.status(Capability::ClapTextEmbedding)
    );

    // Removing just the tokenizer descriptor (simulating the previous,
    // tokenizer-less state) must make the capability Unavailable again,
    // proving readiness genuinely depends on both artifacts and not just
    // the music_text.onnx alone.
    let text_model_only = BTreeMap::from([(
        Capability::ClapTextEmbedding,
        vec![OnnxModel::ClapText.descriptor()],
    )]);
    let report_without_tokenizer_dependency_check = evaluate_capabilities(
        &private_model_dir(),
        &CapabilityConfig::default(),
        &text_model_only,
    );
    // This alone is Available (it never asked for the tokenizer), which is
    // exactly why production wiring must register both descriptors
    // together via `OnnxModel::requirements_by_capability`.
    assert_eq!(
        report_without_tokenizer_dependency_check.status(Capability::ClapTextEmbedding),
        &CapabilityStatus::Available
    );

    // A missing tokenizer descriptor pointed at a nonexistent file must
    // flip the real production requirement set to Unavailable.
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        private_model_dir().join(OnnxModel::CLAP_TEXT_BASENAME),
        dir.path().join(OnnxModel::CLAP_TEXT_BASENAME),
    )
    .unwrap();
    // tokenizer.json intentionally absent from `dir`.
    let report_missing_tokenizer = evaluate_capabilities(
        dir.path(),
        &CapabilityConfig::default(),
        &BTreeMap::from([(
            Capability::ClapTextEmbedding,
            OnnxModel::requirements_by_capability()
                .remove(&Capability::ClapTextEmbedding)
                .unwrap(),
        )]),
    );
    assert!(matches!(
        report_missing_tokenizer.status(Capability::ClapTextEmbedding),
        CapabilityStatus::Unavailable { errors } if errors.len() == 1
    ));
}
