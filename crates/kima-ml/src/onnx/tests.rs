use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn panic_wakes_an_observed_condvar_waiter() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("synthetic.onnx"), MINIMAL_IDENTITY_ONNX).unwrap();
    let artifact = crate::verify_artifact(
        dir.path(),
        &ArtifactDescriptor {
            id: ArtifactId::from_static("synthetic-waiter"),
            basename: "synthetic.onnx",
            expected_sha256: Sha256::digest(MINIMAL_IDENTITY_ONNX).into(),
            max_bytes: MINIMAL_IDENTITY_ONNX.len() as u64,
        },
    )
    .unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let mut pool = BoundedSessionPool::from_verified_bytes(&artifact, 1, Some(1)).unwrap();
    pool.wait_observer = Some(waiting_tx);
    let pool = Arc::new(pool);
    let waiter = pool.clone();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            pool.with_session::<_, ()>(|_| {
                std::thread::spawn(move || {
                    done_tx.send(waiter.with_session(|_| Ok(()))).unwrap();
                });
                // Notification occurs while holding the queue mutex immediately
                // before Condvar::wait atomically releases it. Return cannot race
                // ahead of wait registration. No sleeps or scheduler assumptions.
                waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                panic!("injected checkout panic");
            })
        }))
        .is_err()
    );
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(pool.available(), 1);
}

fn clap_fixture(id: ArtifactId, bytes: Box<[u8]>) -> VerifiedBytes {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let descriptor = ArtifactDescriptor {
        id,
        basename: "synthetic.onnx",
        expected_sha256: Sha256::digest(&bytes).into(),
        max_bytes: bytes.len() as u64,
    };
    std::fs::write(dir.path().join(descriptor.basename), &bytes).unwrap();
    crate::verify_artifact(dir.path(), &descriptor).unwrap()
}

#[test]
fn synthetic_model_runs_batched_variable_time() {
    let artifact = clap_fixture(
        ArtifactId::from_static("synthetic-clap"),
        include_bytes!("../../tests/fixtures/synthetic_clap.onnx")
            .as_slice()
            .into(),
    );
    let model = ClapAudioSession::from_trusted_bytes(&artifact, 1, Some(1)).unwrap();
    let input = vec![2.0; 2 * 3 * 64];
    let out = model.infer(&input, 2, 3).unwrap();
    assert_eq!(out, vec![[2.0; 512]; 2]);
}

#[test]
fn rejects_invalid_inputs_and_outputs() {
    for bytes in [
        include_bytes!("../../tests/fixtures/synthetic_clap_bad_shape.onnx").as_slice(),
        include_bytes!("../../tests/fixtures/synthetic_clap_nonfinite.onnx").as_slice(),
    ] {
        let artifact = clap_fixture(ArtifactId::from_static("synthetic-clap-bad"), bytes.into());
        let model = ClapAudioSession::from_trusted_bytes(&artifact, 1, Some(1)).unwrap();
        assert!(model.infer(&vec![1.0; 64], 1, 1).is_err());
    }
    let artifact = clap_fixture(
        ArtifactId::from_static("synthetic-clap"),
        include_bytes!("../../tests/fixtures/synthetic_clap.onnx")
            .as_slice()
            .into(),
    );
    let model = ClapAudioSession::from_trusted_bytes(&artifact, 1, Some(1)).unwrap();
    assert!(model.infer(&[], 0, 1).is_err());
    assert!(model.infer(&vec![1.0; 64], 1, 2).is_err());
    assert!(model.infer(&vec![1.0; 64], 6, 1).is_err());
    assert!(model.infer(&vec![1.0; 64], 1, 1002).is_err());
    let mut input = vec![0.0; 64];
    input[0] = f32::NAN;
    assert!(model.infer(&input, 1, 1).is_err());
}
