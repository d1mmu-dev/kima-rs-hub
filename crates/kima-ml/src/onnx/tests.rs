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
fn musicnn_synthetic_core_and_head_execute_batched() {
    let core = clap_fixture(
        ArtifactId::from_static("synthetic-core"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_core.onnx")
            .as_slice()
            .into(),
    );
    let head = clap_fixture(
        ArtifactId::from_static("synthetic-head"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_head.onnx")
            .as_slice()
            .into(),
    );
    let core = MusicnnCoreSession::from_trusted_bytes(&core, 1, Some(1)).unwrap();
    let head = MusicnnHeadSession::from_trusted_bytes(&head, MusiCnnHead::Danceability, 1, Some(1))
        .unwrap();
    let embedding = core.infer(&vec![2.0; 2 * 187 * 96], 2).unwrap();
    assert_eq!(embedding, vec![[2.0; 200]; 2]);
    assert_eq!(head.infer(&embedding).unwrap(), vec![[2.0; 2]; 2]);
}

#[test]
fn musicnn_core_rejects_bad_inputs_and_outputs() {
    let bad = clap_fixture(
        ArtifactId::from_static("synthetic-core-bad"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_bad.onnx")
            .as_slice()
            .into(),
    );
    let core = MusicnnCoreSession::from_trusted_bytes(&bad, 1, Some(1)).unwrap();
    // Shape-valid input, but the loaded graph produces a wrong output width.
    assert!(core.infer(&vec![1.0; 187 * 96], 1).is_err());

    let nan = clap_fixture(
        ArtifactId::from_static("synthetic-core-nan"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_nan.onnx")
            .as_slice()
            .into(),
    );
    let core = MusicnnCoreSession::from_trusted_bytes(&nan, 1, Some(1)).unwrap();
    assert!(core.infer(&vec![1.0; 187 * 96], 1).is_err());

    let core = clap_fixture(
        ArtifactId::from_static("synthetic-core"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_core.onnx")
            .as_slice()
            .into(),
    );
    let model = MusicnnCoreSession::from_trusted_bytes(&core, 1, Some(1)).unwrap();
    // Malformed caller-supplied shape: wrong element count for the claimed batch.
    assert!(model.infer(&vec![1.0; 187 * 96 - 1], 1).is_err());
    assert!(model.infer(&[], 0).is_err());
    assert!(
        model
            .infer(
                &vec![1.0; 187 * 96 * (MAX_MUSICNN_BATCH + 1)],
                MAX_MUSICNN_BATCH + 1
            )
            .is_err()
    );
    let mut input = vec![0.0; 187 * 96];
    input[0] = f32::NAN;
    assert!(model.infer(&input, 1).is_err());
}

#[test]
fn musicnn_head_synthetic_identity_executes_batched() {
    let head = clap_fixture(
        ArtifactId::from_static("synthetic-head-identity"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_head_identity.onnx")
            .as_slice()
            .into(),
    );
    let head =
        MusicnnHeadSession::from_trusted_bytes(&head, MusiCnnHead::Deam, 1, Some(1)).unwrap();
    let embedding = vec![[2.0; 200]; 2];
    assert_eq!(head.infer(&embedding).unwrap(), vec![[2.0; 2]; 2]);
}

#[test]
fn musicnn_head_rejects_bad_inputs_and_outputs() {
    for (bytes, head) in [
        (
            include_bytes!("../../tests/fixtures/synthetic_musicnn_bad.onnx").as_slice(),
            MusiCnnHead::Danceability,
        ),
        (
            include_bytes!("../../tests/fixtures/synthetic_musicnn_head_identity_bad.onnx")
                .as_slice(),
            MusiCnnHead::Deam,
        ),
    ] {
        let artifact = clap_fixture(ArtifactId::from_static("synthetic-head-bad"), bytes.into());
        // Wrong output width is caught at construction time, not only at infer time.
        assert!(MusicnnHeadSession::from_trusted_bytes(&artifact, head, 1, Some(1)).is_err());
    }

    for (bytes, head) in [
        (
            include_bytes!("../../tests/fixtures/synthetic_musicnn_nan.onnx").as_slice(),
            MusiCnnHead::Danceability,
        ),
        (
            include_bytes!("../../tests/fixtures/synthetic_musicnn_head_identity_nan.onnx")
                .as_slice(),
            MusiCnnHead::Deam,
        ),
    ] {
        let artifact = clap_fixture(ArtifactId::from_static("synthetic-head-nan"), bytes.into());
        let head = MusicnnHeadSession::from_trusted_bytes(&artifact, head, 1, Some(1)).unwrap();
        assert!(head.infer(&[[1.0; 200]]).is_err());
    }

    let head = clap_fixture(
        ArtifactId::from_static("synthetic-head"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_head.onnx")
            .as_slice()
            .into(),
    );
    let model =
        MusicnnHeadSession::from_trusted_bytes(&head, MusiCnnHead::Danceability, 1, Some(1))
            .unwrap();
    // Caller-supplied batch size bounds, enforced regardless of fixed-width rows.
    assert!(model.infer(&[]).is_err());
    assert!(
        model
            .infer(&vec![[1.0; 200]; MAX_MUSICNN_BATCH + 1])
            .is_err()
    );
    let mut row = [0.0; 200];
    row[0] = f32::NAN;
    assert!(model.infer(&[row]).is_err());
}

#[test]
fn musicnn_head_rejects_output_name_mismatched_to_the_specific_head() {
    // `synthetic_musicnn_head.onnx` genuinely outputs `model/Softmax:0`; pairing it
    // with an Identity-expecting head (Deam/Emomusic) must fail closed at
    // construction time rather than only later selecting the wrong tensor name.
    let softmax_named = clap_fixture(
        ArtifactId::from_static("synthetic-head-softmax-named"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_head.onnx")
            .as_slice()
            .into(),
    );
    assert!(
        MusicnnHeadSession::from_trusted_bytes(&softmax_named, MusiCnnHead::Deam, 1, Some(1))
            .is_err()
    );
    assert!(
        MusicnnHeadSession::from_trusted_bytes(&softmax_named, MusiCnnHead::Emomusic, 1, Some(1))
            .is_err()
    );
    // Correctly paired with a Softmax-expecting head, construction still succeeds.
    assert!(
        MusicnnHeadSession::from_trusted_bytes(
            &softmax_named,
            MusiCnnHead::Danceability,
            1,
            Some(1)
        )
        .is_ok()
    );

    // `synthetic_musicnn_head_identity.onnx` genuinely outputs `model/Identity:0`;
    // pairing it with any Softmax-expecting head must also fail closed.
    let identity_named = clap_fixture(
        ArtifactId::from_static("synthetic-head-identity-named"),
        include_bytes!("../../tests/fixtures/synthetic_musicnn_head_identity.onnx")
            .as_slice()
            .into(),
    );
    assert!(
        MusicnnHeadSession::from_trusted_bytes(
            &identity_named,
            MusiCnnHead::Danceability,
            1,
            Some(1)
        )
        .is_err()
    );
    assert!(
        MusicnnHeadSession::from_trusted_bytes(
            &identity_named,
            MusiCnnHead::MoodAcoustic,
            1,
            Some(1)
        )
        .is_err()
    );
    // Correctly paired with an Identity-expecting head, construction still succeeds.
    assert!(
        MusicnnHeadSession::from_trusted_bytes(&identity_named, MusiCnnHead::Deam, 1, Some(1))
            .is_ok()
    );
    assert!(
        MusicnnHeadSession::from_trusted_bytes(&identity_named, MusiCnnHead::Emomusic, 1, Some(1))
            .is_ok()
    );
}

#[test]
fn musicnn_head_rejects_wrong_model_id_or_hash() {
    use sha2::{Digest, Sha256};
    let bytes: Box<[u8]> = include_bytes!("../../tests/fixtures/synthetic_musicnn_head.onnx")
        .as_slice()
        .into();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("synthetic.onnx"), &bytes).unwrap();

    // Wrong artifact id: `from_verified_bytes` must check the pinned id, not trust the caller.
    let wrong_id = crate::verify_artifact(
        dir.path(),
        &ArtifactDescriptor {
            id: ArtifactId::from_static("not-the-pinned-head"),
            basename: "synthetic.onnx",
            expected_sha256: Sha256::digest(&bytes).into(),
            max_bytes: bytes.len() as u64,
        },
    )
    .unwrap();
    assert!(
        MusicnnHeadSession::from_verified_bytes(&wrong_id, MusiCnnHead::Danceability, 1, Some(1))
            .is_err()
    );

    // Correct id but tampered bytes must fail the pinned hash check during verification.
    let mut tampered = bytes.to_vec();
    tampered[0] ^= 0xFF;
    std::fs::write(dir.path().join("synthetic.onnx"), &tampered).unwrap();
    assert!(
        crate::verify_artifact(
            dir.path(),
            &ArtifactDescriptor {
                id: MusiCnnHead::Danceability.artifact_id(),
                basename: "synthetic.onnx",
                expected_sha256: Sha256::digest(&bytes).into(),
                max_bytes: bytes.len() as u64,
            },
        )
        .is_err()
    );
}

#[test]
fn musicnn_core_rejects_wrong_model_id_or_hash() {
    use sha2::{Digest, Sha256};
    let bytes: Box<[u8]> = include_bytes!("../../tests/fixtures/synthetic_musicnn_core.onnx")
        .as_slice()
        .into();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("synthetic.onnx"), &bytes).unwrap();

    // Wrong artifact id: `from_verified_bytes` must check the pinned id, not trust the caller.
    let wrong_id = crate::verify_artifact(
        dir.path(),
        &ArtifactDescriptor {
            id: ArtifactId::from_static("not-the-pinned-core"),
            basename: "synthetic.onnx",
            expected_sha256: Sha256::digest(&bytes).into(),
            max_bytes: bytes.len() as u64,
        },
    )
    .unwrap();
    assert!(MusicnnCoreSession::from_verified_bytes(&wrong_id, 1, Some(1)).is_err());

    // Correct id but tampered bytes must fail the pinned hash check during verification.
    let mut tampered = bytes.to_vec();
    tampered[0] ^= 0xFF;
    std::fs::write(dir.path().join("synthetic.onnx"), &tampered).unwrap();
    assert!(
        crate::verify_artifact(
            dir.path(),
            &ArtifactDescriptor {
                id: OnnxModel::MusiCnnCore.artifact_id(),
                basename: "synthetic.onnx",
                expected_sha256: Sha256::digest(&bytes).into(),
                max_bytes: bytes.len() as u64,
            },
        )
        .is_err()
    );
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
fn text_synthetic_model_runs_batched_variable_sequence() {
    let artifact = clap_fixture(
        ArtifactId::from_static("synthetic-clap-text"),
        include_bytes!("../../tests/fixtures/synthetic_clap_text.onnx")
            .as_slice()
            .into(),
    );
    let model = ClapTextSession::from_trusted_bytes(&artifact, 1, Some(1)).unwrap();
    let ids: Vec<i64> = vec![2, 2, 2, 2, 2, 2];
    let out = model.infer(&ids, 2, 3).unwrap();
    assert_eq!(out, vec![[2.0; 512]; 2]);
}

#[test]
fn text_rejects_invalid_inputs_and_outputs() {
    for bytes in [
        include_bytes!("../../tests/fixtures/synthetic_clap_text_bad_shape.onnx").as_slice(),
        include_bytes!("../../tests/fixtures/synthetic_clap_text_nonfinite.onnx").as_slice(),
    ] {
        let artifact = clap_fixture(
            ArtifactId::from_static("synthetic-clap-text-bad"),
            bytes.into(),
        );
        let model = ClapTextSession::from_trusted_bytes(&artifact, 1, Some(1)).unwrap();
        assert!(model.infer(&[1], 1, 1).is_err());
    }
    let artifact = clap_fixture(
        ArtifactId::from_static("synthetic-clap-text"),
        include_bytes!("../../tests/fixtures/synthetic_clap_text.onnx")
            .as_slice()
            .into(),
    );
    let model = ClapTextSession::from_trusted_bytes(&artifact, 1, Some(1)).unwrap();
    assert!(model.infer(&[], 0, 1).is_err());
    assert!(model.infer(&[1], 1, 2).is_err());
    assert!(
        model
            .infer(
                &vec![1; 77 * (MAX_CLAP_TEXT_BATCH + 1)],
                MAX_CLAP_TEXT_BATCH + 1,
                77
            )
            .is_err()
    );
    assert!(model.infer(&vec![1; 78], 1, 78).is_err());
    assert!(model.infer(&[-1], 1, 1).is_err());
    assert!(
        model
            .infer(&[crate::text_tokenizer::CLAP_TEXT_VOCAB_SIZE as i64], 1, 1)
            .is_err()
    );
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
