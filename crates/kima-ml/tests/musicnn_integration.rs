use kima_ml::musicnn::{MusicnnFrontend, mel_patches};

fn signal(length: usize) -> Vec<f32> {
    (0..length)
        .map(|i| ((i % 17) as f32 - 8.0) / 16.0)
        .collect()
}

#[test]
fn non_silent_frontend_patches_keep_offsets_order_and_essentia_values() {
    // 374 frames produce three overlapping patches and leave one frame unused.
    let samples = signal(373 * 256);
    let frames = MusicnnFrontend::new().compute(&samples).unwrap();
    assert_eq!(frames.len(), 374);
    assert!(frames.iter().flatten().all(|x| x.is_finite()));
    assert!(frames.iter().flatten().any(|&x| x > 0.0));
    assert_ne!(frames[1], frames[2]);

    let patches: Vec<_> = mel_patches(&frames).collect();
    assert_eq!(patches.len(), 3);
    for (patch, start) in patches.iter().zip([0, 93, 186]) {
        assert_eq!(patch.len(), 187);
        for (offset, frame) in patch.iter().enumerate() {
            assert_eq!(
                frame,
                &frames[start + offset],
                "patch {start} frame {offset}"
            );
        }
    }

    // This fixture row is an independently generated Essentia result for an
    // unpadded frame of the same period-17 signal. A shift by 17 hops leaves
    // its samples unchanged; compare occurrences inside every patch.
    let row = include_str!("fixtures/musicnn-synthetic.csv")
        .lines()
        .map(|line| {
            line.split(',')
                .map(|x| x.parse::<f32>().unwrap())
                .collect::<Vec<_>>()
        })
        .find(|row| row[0] == 1024.0 && row[1] == 1.0)
        .unwrap();
    assert_eq!(row.len(), 102);
    for (patch, start) in patches.iter().zip([0, 93, 186]) {
        let mut compared = 0;
        for (offset, frame) in patch.iter().enumerate() {
            let index = start + offset;
            if index >= 1 && (index - 1) % 17 == 0 {
                for (band, (&actual, &expected)) in frame.iter().zip(&row[6..]).enumerate() {
                    assert!(
                        (actual - expected).abs() < 0.00002,
                        "frame {index} band {band}: {actual} vs {expected}"
                    );
                    compared += 1;
                }
            }
        }
        assert!(compared >= 96);
    }
}

#[test]
fn exact_duration_limit_accepts_non_silent_signal_and_next_sample_is_rejected() {
    let mut frontend = MusicnnFrontend::new();
    let mut samples = signal(90 * 16_000);
    let frames = frontend.compute(&samples).unwrap();
    assert_eq!(frames.len(), 5626);
    assert!(frames.iter().flatten().all(|x| x.is_finite()));
    let patches: Vec<_> = mel_patches(&frames).collect();
    assert_eq!(patches.len(), 59);
    assert_eq!(patches.last().unwrap(), &&frames[5394..5581]);
    samples.push(0.25);
    assert_eq!(
        frontend.compute(&samples).unwrap_err(),
        "input exceeds 90 seconds"
    );
}
