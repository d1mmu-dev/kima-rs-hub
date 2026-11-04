mod common;
use kima_ml::{ArtifactId, onnx::ClapAudioSession};

#[test]
fn synthetic_model_runs_batched_variable_time() {
    let artifact = common::verified_fixture(
        ArtifactId::from_static("synthetic-clap"),
        include_bytes!("fixtures/synthetic_clap.onnx")
            .as_slice()
            .into(),
    );
    let model = ClapAudioSession::from_verified_bytes(&artifact, 1, Some(1)).unwrap();
    let input = vec![2.0; 2 * 3 * 64];
    let out = model.infer(&input, 2, 3).unwrap();
    assert_eq!(out, vec![[2.0; 512]; 2]);
}

#[test]
fn rejects_invalid_inputs_and_outputs() {
    for bytes in [
        include_bytes!("fixtures/synthetic_clap_bad_shape.onnx").as_slice(),
        include_bytes!("fixtures/synthetic_clap_nonfinite.onnx").as_slice(),
    ] {
        let artifact =
            common::verified_fixture(ArtifactId::from_static("synthetic-clap-bad"), bytes.into());
        let model = ClapAudioSession::from_verified_bytes(&artifact, 1, Some(1)).unwrap();
        assert!(model.infer(&vec![1.0; 64], 1, 1).is_err());
    }
    let artifact = common::verified_fixture(
        ArtifactId::from_static("synthetic-clap"),
        include_bytes!("fixtures/synthetic_clap.onnx")
            .as_slice()
            .into(),
    );
    let model = ClapAudioSession::from_verified_bytes(&artifact, 1, Some(1)).unwrap();
    assert!(model.infer(&[], 0, 1).is_err());
    assert!(model.infer(&vec![1.0; 64], 1, 2).is_err());
    assert!(model.infer(&vec![1.0; 64], 6, 1).is_err());
    assert!(model.infer(&vec![1.0; 64], 1, 1002).is_err());
    let mut input = vec![0.0; 64];
    input[0] = f32::NAN;
    assert!(model.infer(&input, 1, 1).is_err());
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
