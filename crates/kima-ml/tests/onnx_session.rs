use kima_ml::onnx::{MINIMAL_IDENTITY_ONNX, OnnxModelConsumer, SharedSession};
use kima_ml::{ArtifactId, ModelConsumer, VerifiedBytes};

#[test]
fn test_load_valid_onnx_model_from_verified_bytes() {
    let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
    let artifact =
        VerifiedBytes::from_verified_bytes(ArtifactId::from_static("test-identity"), bytes);

    let consumer = OnnxModelConsumer::new();
    let session: SharedSession = consumer
        .load(artifact)
        .expect("valid ONNX bytes must load successfully");

    assert_eq!(session.id().as_str(), "test-identity");
}

#[test]
fn test_load_invalid_onnx_bytes_fails() {
    let invalid_bytes = b"not-a-valid-onnx-model-binary".to_vec().into_boxed_slice();
    let artifact = VerifiedBytes::from_verified_bytes(
        ArtifactId::from_static("corrupted-model"),
        invalid_bytes,
    );

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
    let artifact =
        VerifiedBytes::from_verified_bytes(ArtifactId::from_static("test-identity"), bytes);
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
    let artifact =
        VerifiedBytes::from_verified_bytes(ArtifactId::from_static("test-identity"), bytes);
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
fn test_bounded_session_pool_concurrent_execution() {
    use kima_ml::onnx::BoundedSessionPool;
    use std::sync::Arc;
    use std::thread;

    let bytes = MINIMAL_IDENTITY_ONNX.to_vec().into_boxed_slice();
    let artifact =
        VerifiedBytes::from_verified_bytes(ArtifactId::from_static("test-identity"), bytes);

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
