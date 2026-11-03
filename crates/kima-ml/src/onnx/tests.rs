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
