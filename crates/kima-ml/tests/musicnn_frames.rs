use kima_ml::musicnn::centered_frames;

#[test]
fn centered_frames_match_synthetic_reference_boundaries() {
    for (n, count) in [
        (1, 2),
        (255, 2),
        (256, 2),
        (257, 3),
        (512, 3),
        (513, 4),
        (1024, 5),
    ] {
        let samples: Vec<f32> = (0..n).map(|i| ((i % 17) as f32 - 8.0) / 16.0).collect();
        let frames: Vec<_> = centered_frames(&samples).collect();
        assert_eq!(frames.len(), count, "length {n}");
        for (f, frame) in frames.iter().enumerate() {
            for (i, &actual) in frame.iter().enumerate() {
                let index = (f * 256 + i).checked_sub(256);
                let expected = index
                    .and_then(|index| samples.get(index))
                    .copied()
                    .unwrap_or(0.0);
                assert_eq!(actual, expected, "length {n}, frame {f}, sample {i}");
            }
        }
    }
    assert_eq!(centered_frames(&[]).count(), 0);
}
