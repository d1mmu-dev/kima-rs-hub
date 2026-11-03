use kima_ml::clap::{WindowCollector, source_window_ranges};

#[test]
fn derived_windows_are_collected_from_complete_source() {
    let ranges = source_window_ranges(251, 10).unwrap();
    assert_eq!(ranges, vec![0..100, 151..251]);
    let samples: Vec<f32> = (0..251).map(|v| v as f32).collect();
    let mut collector = WindowCollector::for_clap(251, 10).unwrap();
    for chunk in samples.chunks(17) {
        collector.push(chunk).unwrap();
    }
    assert_eq!(
        collector.finish().unwrap(),
        ranges
            .iter()
            .map(|r| samples[r.clone()].to_vec())
            .collect::<Vec<_>>()
    );
}

#[test]
fn rational_boundaries_and_upstream_float_differences_are_explicit() {
    // Values from a manual transcription of the pinned Python offset code and
    // librosa's int(offset * sr), int(duration * sr) soundfile extraction.
    // At 67 seconds Python gives 964799..1444799 for window 2, while exact
    // rational 30% is one frame later. At 40 s + 1 frame Python gives the
    // last window 1440000..1920000 because of binary64 rounding.
    assert_eq!(
        source_window_ranges(3_216_000, 48_000).unwrap()[1],
        964_800..1_444_800
    );
    assert_eq!(
        source_window_ranges(1_920_001, 48_000).unwrap()[3],
        1_440_001..1_920_001
    );
    assert_eq!(
        source_window_ranges(432_001, 48_000).unwrap(),
        vec![0..432_001]
    );
    assert_eq!(
        source_window_ranges(960_000, 48_000).unwrap(),
        vec![0..480_000, 480_000..960_000]
    );
    assert_eq!(
        source_window_ranges(3_200_000, 48_000).unwrap(),
        vec![
            320_000..800_000,
            960_000..1_440_000,
            1_600_000..2_080_000,
            2_240_000..2_720_000,
            2_720_000..3_200_000
        ]
    );
}

#[test]
fn invalid_inputs_and_maximum_frame_count_are_safe() {
    use kima_ml::clap::WindowCollectorError::{FrameCount, InvalidRanges};
    for (frames, rate) in [(0, 48000), (1, 0), (1, 192001)] {
        assert_eq!(source_window_ranges(frames, rate), Err(InvalidRanges));
    }
    let ranges = source_window_ranges(usize::MAX, 192_000).unwrap();
    assert_eq!(ranges.len(), 5);
    for r in ranges {
        assert_eq!(r.end - r.start, 1_920_000);
    }
    let mut collector = WindowCollector::for_clap(251, 10).unwrap();
    collector.push(&[0.0; 250]).unwrap();
    assert_eq!(collector.finish(), Err(FrameCount));
}
