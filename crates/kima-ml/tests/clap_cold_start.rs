//! Cold native initialization must be exercised before any serial reference call.
use kima_ml::clap::prepare_window;
use std::sync::{Arc, Barrier};

#[test]
fn simultaneous_first_use_matches_pinned_reference() {
    const CHILD: &str = "KIMA_TEST_SOXR_COLD_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Each process has a fresh libsoxr FFT cache. Bound both process count
        // and worker count; retain ordinary harness parallelism in other tests.
        for attempt in 0..8 {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "simultaneous_first_use_matches_pinned_reference",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!("cold attempt {attempt} timed out: {output:?}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let result = child.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "cold attempt {attempt}: {result:?}"
            );
        }
        return;
    }
    let reference: Vec<f32> = include_bytes!("fixtures/soxr-8000.f32le")
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let mut input: Vec<f32> = (0..257)
        .map(|i| ((i * 37 % 101) as f32 - 50.0) / 64.0)
        .collect();
    input[0] = 1.0;
    input[256] = -1.0;
    let barrier = Arc::new(Barrier::new(32));
    std::thread::scope(|scope| {
        for _ in 0..32 {
            let (barrier, input, reference) = (&barrier, &input, &reference);
            scope.spawn(move || {
                barrier.wait();
                let actual = prepare_window(input, 8_000).unwrap();
                assert_eq!(actual.len(), 480_000);
                let repeated = 480_000 / reference.len() * reference.len();
                for (i, value) in actual.iter().enumerate() {
                    let expected = if i < repeated {
                        reference[i % reference.len()]
                    } else {
                        0.0
                    };
                    assert!(
                        (value - expected).abs() < 3e-6,
                        "index={i}: {value} != {expected}"
                    );
                }
            });
        }
    });
}
