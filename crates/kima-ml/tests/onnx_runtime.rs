mod common;

use kima_ml::onnx::{MINIMAL_IDENTITY_ONNX, OnnxModelConsumer};
use kima_ml::{ArtifactId, ModelConsumer};

#[test]
fn cargo_ort_features_never_download_runtime() {
    let manifest = include_str!("../../../Cargo.toml");
    let ort = manifest
        .lines()
        .find(|line| line.starts_with("ort ="))
        .unwrap();
    assert!(
        ort.contains("default-features = false"),
        "ORT defaults download binaries: {ort}"
    );
    assert!(ort.contains("load-dynamic"));
    assert!(!ort.contains("download-binaries") && !ort.contains("copy-dylibs"));
}

#[test]
fn missing_runtime_is_a_load_error_not_a_panic() {
    if std::env::var_os("KIMA_TEST_RUNTIME_CHILD").is_some() {
        let artifact = common::verified_fixture(
            ArtifactId::from_static("synthetic-runtime"),
            MINIMAL_IDENTITY_ONNX.into(),
        );
        let error = OnnxModelConsumer::new()
            .load(artifact)
            .expect_err("unprovisioned runtime must fail closed");
        assert!(error.to_string().contains("ORT_DYLIB_PATH"), "{error}");
        return;
    }
    let invalid_runtime = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(invalid_runtime.path(), b"not a shared library").unwrap();
    for path in [
        None,
        Some("/nonexistent/kima-ort/libonnxruntime.so"),
        Some("relative/libonnxruntime.so"),
        Some(invalid_runtime.path().to_str().unwrap()),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "missing_runtime_is_a_load_error_not_a_panic",
                "--nocapture",
            ])
            .env("KIMA_TEST_RUNTIME_CHILD", "1")
            .env_remove("ORT_DYLIB_PATH");
        if let Some(path) = path {
            command.env("ORT_DYLIB_PATH", path);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
