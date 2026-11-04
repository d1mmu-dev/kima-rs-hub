mod common;
use kima_ml::{ArtifactId, onnx::ClapAudioSession};

#[test]
fn rejects_spoofed_clap_identity() {
    let artifact = common::verified_fixture(
        ArtifactId::from_static("hf_audio"),
        include_bytes!("fixtures/synthetic_clap.onnx")
            .as_slice()
            .into(),
    );
    assert!(ClapAudioSession::from_verified_bytes(&artifact, 1, Some(1)).is_err());
}

#[test]
#[ignore = "private noncommercial model and Python reference required"]
fn private_real_model_python_comparison() {
    use kima_ml::{onnx::OnnxModel, verify_artifact};
    let artifact = verify_artifact(
        std::path::Path::new("/home/dimmu/.hermes/cache/scratch/clap-private-export"),
        &OnnxModel::ClapAudio.descriptor(),
    )
    .unwrap();
    let model = ClapAudioSession::from_verified_bytes(&artifact, 1, Some(1)).unwrap();
    let input: Vec<f32> = (0..1001 * 64)
        .map(|i| ((i % 101) as f32 - 50.0) / 50.0)
        .collect();
    let actual = model.infer(&input, 1, 1001).unwrap();
    let bytes =
        std::fs::read("/home/dimmu/.hermes/cache/scratch/m4-clap-smoke/python.f32").unwrap();
    assert_eq!(bytes.len(), 512 * 4);
    let reference: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let (mut dot, mut a, mut b, mut max): (f64, f64, f64, f64) = (0.0, 0.0, 0.0, 0.0);
    for (&x, &y) in actual[0].iter().zip(&reference) {
        let (x, y) = (x as f64, y as f64);
        dot += x * y;
        a += x * x;
        b += y * y;
        max = max.max((x - y).abs());
    }
    let cosine = dot / (a.sqrt() * b.sqrt());
    println!("cosine={cosine:.12} maximum_error={max:.12}");
    assert!(cosine > 0.99999 && max < 1e-4);
}
