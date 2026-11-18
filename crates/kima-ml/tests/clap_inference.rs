mod common;
use kima_ml::onnx::{ClapAudioSession, OnnxModel};

// Real-model parity for the music checkpoint lives in `music_clap_real.rs`.

#[test]
fn rejects_spoofed_clap_identity() {
    let artifact = common::verified_fixture(
        OnnxModel::ClapAudio.artifact_id(),
        include_bytes!("fixtures/synthetic_clap.onnx")
            .as_slice()
            .into(),
    );
    assert!(ClapAudioSession::from_verified_bytes(&artifact, 1, Some(1)).is_err());
}
