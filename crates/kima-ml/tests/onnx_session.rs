mod common;

use kima_ml::onnx::{MINIMAL_IDENTITY_ONNX, OnnxModelConsumer, SharedSession};
use kima_ml::{ArtifactId, ModelConsumer};

#[test]
fn test_load_valid_onnx_model_from_verified_bytes() {
    let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
    let artifact = common::verified_fixture(ArtifactId::from_static("test-identity"), bytes);

    let consumer = OnnxModelConsumer::new();
    let session: SharedSession = consumer
        .load(artifact)
        .expect("valid ONNX bytes must load successfully");

    assert_eq!(session.id().as_str(), "test-identity");
}

#[test]
fn test_load_invalid_onnx_bytes_fails() {
    let invalid_bytes = b"not-a-valid-onnx-model-binary".to_vec().into_boxed_slice();
    let artifact =
        common::verified_fixture(ArtifactId::from_static("corrupted-model"), invalid_bytes);

    let consumer = OnnxModelConsumer::new();
    let err = consumer
        .load(artifact)
        .expect_err("corrupted ONNX bytes must fail session loading");

    match err {
        kima_ml::onnx::OnnxError::SessionInit { id, message } => {
            assert_eq!(id.as_str(), "corrupted-model");
            assert!(!message.is_empty(), "error message should contain details");
        }
        other => panic!("expected SessionInit error, got: {:?}", other),
    }
}

#[test]
fn test_shared_session_dummy_inference() {
    let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
    let artifact = common::verified_fixture(ArtifactId::from_static("test-identity"), bytes);
    let consumer = OnnxModelConsumer::new();
    let session = consumer.load(artifact).expect("session should load");

    let input = vec![42.0f32];
    let output = session.run_1d(&input).expect("inference should succeed");
    assert_eq!(output, vec![42.0f32]);
}

#[test]
fn test_shared_session_thread_safety() {
    use std::sync::Arc;
    use std::thread;

    let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
    let artifact = common::verified_fixture(ArtifactId::from_static("test-identity"), bytes);
    let consumer = OnnxModelConsumer::new();
    let session = Arc::new(consumer.load(artifact).expect("session should load"));

    let mut handles = Vec::new();
    for i in 0..8 {
        let session_clone = Arc::clone(&session);
        handles.push(thread::spawn(move || {
            let val = i as f32;
            let output = session_clone
                .run_1d(&[val])
                .expect("concurrent inference should succeed");
            assert_eq!(output, vec![val]);
        }));
    }

    for handle in handles {
        handle.join().expect("thread should join cleanly");
    }
}

#[test]
fn pool_panic_returns_session_and_wakes_waiter() {
    use kima_ml::onnx::BoundedSessionPool;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let artifact = common::verified_fixture(
        ArtifactId::from_static("synthetic-panic"),
        MINIMAL_IDENTITY_ONNX.into(),
    );
    let pool = Arc::new(BoundedSessionPool::from_verified_bytes(&artifact, 1, Some(1)).unwrap());
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let waiter_pool = pool.clone();
    let result = catch_unwind(AssertUnwindSafe(|| {
        pool.with_session::<_, ()>(|_| {
            std::thread::spawn(move || {
                ready_tx.send(()).unwrap();
                let result = waiter_pool.with_session(|_| Ok(42));
                done_tx.send(result).unwrap();
            });
            ready_rx.recv().unwrap();
            panic!("injected callback panic");
        })
    }));
    assert!(result.is_err());
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        42
    );
    assert_eq!(pool.available(), 1);
    pool.with_session(|sess| {
        let input = ort::value::Tensor::from_array(([1], vec![7.0f32])).unwrap();
        let outputs = sess.run(ort::inputs![input]).unwrap();
        assert_eq!(outputs[0].try_extract_tensor::<f32>().unwrap().1, &[7.0]);
        Ok(())
    })
    .unwrap();
}

#[test]
fn shared_session_poison_fails_closed() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let artifact = common::verified_fixture(
        ArtifactId::from_static("synthetic-poison"),
        MINIMAL_IDENTITY_ONNX.into(),
    );
    let session = OnnxModelConsumer::new().load(artifact).unwrap();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            session.run_with::<_, ()>(|_| panic!("injected panic"))
        }))
        .is_err()
    );
    assert!(matches!(
        session.run_1d(&[1.0]),
        Err(kima_ml::onnx::OnnxError::LockPoisoned)
    ));
}

#[test]
fn test_bounded_session_pool_concurrent_execution() {
    use kima_ml::onnx::BoundedSessionPool;
    use std::sync::Arc;
    use std::thread;

    let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
    let artifact = common::verified_fixture(ArtifactId::from_static("test-identity"), bytes);

    let pool = Arc::new(
        BoundedSessionPool::from_verified_bytes(&artifact, 4, Some(1))
            .expect("pool should initialize"),
    );
    assert_eq!(pool.capacity(), 4);
    assert_eq!(pool.available(), 4);

    let mut handles = Vec::new();
    for i in 0..16 {
        let pool_clone = Arc::clone(&pool);
        handles.push(thread::spawn(move || {
            let val = (i * 2) as f32;
            let output = pool_clone
                .with_session(|sess| {
                    let tensor =
                        ort::value::Tensor::from_array(([1], vec![val].into_boxed_slice()))
                            .map_err(|e| kima_ml::onnx::OnnxError::Inference(e.to_string()))?;
                    let outputs = sess
                        .run(ort::inputs![tensor])
                        .map_err(|e| kima_ml::onnx::OnnxError::Inference(e.to_string()))?;
                    let out_tensor = outputs[0]
                        .try_extract_tensor::<f32>()
                        .map_err(|e| kima_ml::onnx::OnnxError::Inference(e.to_string()))?;
                    Ok(out_tensor.1.to_vec())
                })
                .expect("pool inference must succeed");
            assert_eq!(output, vec![val]);
        }));
    }

    for handle in handles {
        handle.join().expect("thread should join cleanly");
    }

    assert_eq!(pool.available(), 4);
}
