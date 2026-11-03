mod common;
use kima_ml::onnx::{BoundedSessionPool, MINIMAL_IDENTITY_ONNX, OnnxModelConsumer};
use kima_ml::{ArtifactId, ModelConsumer};

fn fixture() -> kima_ml::VerifiedBytes {
    common::verified_fixture(
        ArtifactId::from_static("synthetic-resources"),
        MINIMAL_IDENTITY_ONNX.into(),
    )
}

#[test]
fn thread_counts_are_nonzero_and_bounded() {
    for threads in [0, 5, usize::MAX] {
        assert!(
            OnnxModelConsumer::new()
                .with_intra_threads(threads)
                .load(fixture())
                .is_err(),
            "accepted threads={threads}"
        );
        assert!(BoundedSessionPool::from_verified_bytes(&fixture(), 1, Some(threads)).is_err());
    }
    assert!(
        OnnxModelConsumer::new()
            .with_intra_threads(4)
            .load(fixture())
            .is_ok()
    );
}

#[test]
fn pool_size_has_a_small_upper_bound() {
    assert!(BoundedSessionPool::from_verified_bytes(&fixture(), 0, Some(1)).is_err());
    assert!(BoundedSessionPool::from_verified_bytes(&fixture(), 5, Some(1)).is_err());
}

#[test]
fn aggregate_sessions_and_threads_are_reserved_until_last_clone_drops() {
    if std::env::var_os("KIMA_AGGREGATE_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "aggregate_sessions_and_threads_are_reserved_until_last_clone_drops",
                "--nocapture",
            ])
            .env("KIMA_AGGREGATE_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let consumer = OnnxModelConsumer::new();
    let mut sessions: Vec<_> = (0..32).map(|_| consumer.load(fixture()).unwrap()).collect();
    assert!(
        consumer.load(fixture()).is_err(),
        "aggregate session limit absent"
    );
    let clone = sessions[0].clone();
    sessions.remove(0);
    assert!(
        consumer.load(fixture()).is_err(),
        "clone released a live session reservation"
    );
    drop(clone);
    assert!(consumer.load(fixture()).is_ok());
    drop(sessions);
    let threaded = OnnxModelConsumer::new().with_intra_threads(4);
    let sessions: Vec<_> = (0..16).map(|_| threaded.load(fixture()).unwrap()).collect();
    assert!(
        threaded.load(fixture()).is_err(),
        "aggregate thread limit absent"
    );
    assert!(
        BoundedSessionPool::from_verified_bytes(&fixture(), 1, Some(1)).is_err(),
        "pool bypassed shared budget"
    );
    drop(sessions);
    assert!(consumer.load(fixture()).is_ok());
    for _ in 0..40 {
        let invalid = common::verified_fixture(
            ArtifactId::from_static("synthetic-invalid"),
            Box::from(&b"invalid"[..]),
        );
        assert!(consumer.load(invalid).is_err());
    }
    assert!(
        consumer.load(fixture()).is_ok(),
        "failed loads leaked budget"
    );
}

#[test]
fn default_runtime_does_not_spawn_machine_sized_thread_pools() {
    if std::env::var_os("KIMA_RESOURCE_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "default_runtime_does_not_spawn_machine_sized_thread_pools",
                "--nocapture",
            ])
            .env("KIMA_RESOURCE_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    let tasks = || std::fs::read_dir("/proc/self/task").unwrap().count();
    let before = tasks();
    let session = OnnxModelConsumer::new().load(fixture()).unwrap();
    assert_eq!(session.run_1d(&[5.0]).unwrap(), [5.0]);
    let after = tasks();
    assert!(
        after <= before + 1,
        "default session grew thread count from {before} to {after}"
    );
}
