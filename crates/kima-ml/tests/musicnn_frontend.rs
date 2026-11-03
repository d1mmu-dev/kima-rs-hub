use kima_ml::musicnn::{MusicnnFrontend, centered_frames};

#[test]
fn synthetic_frames_match_essentia_reference_in_all_bands() {
    let mut frontend = MusicnnFrontend::new();
    let mut max_error = 0.0_f32;
    let mut compared = 0;
    for row in include_str!("fixtures/musicnn-synthetic.csv").lines() {
        let values: Vec<f32> = row.split(',').map(|x| x.parse().unwrap()).collect();
        let n = values[0] as usize;
        let frame_index = values[1] as usize;
        let samples: Vec<f32> = (0..n).map(|i| ((i % 17) as f32 - 8.0) / 16.0).collect();
        let frames = frontend.compute(&samples).unwrap();
        assert_eq!(frames.len(), n.div_ceil(256) + 1);
        assert_eq!(values.len(), 102);
        let frame = centered_frames(&samples).nth(frame_index).unwrap();
        for (index, &expected) in [0, 255, 256, 511].iter().zip(&values[2..6]) {
            assert_eq!(
                frame[*index], expected,
                "n={n} frame={frame_index} index={index}"
            );
        }
        for (band, (&actual, &expected)) in frames[frame_index].iter().zip(&values[6..]).enumerate()
        {
            let error = (actual - expected).abs();
            max_error = max_error.max(error);
            compared += 1;
            assert!(
                error < 0.00002,
                "n={n} frame={frame_index} band={band}: {actual} vs {expected}"
            );
        }
    }
    assert_eq!(compared, 21 * 96);
    println!("maximum absolute error: {max_error}");
}
