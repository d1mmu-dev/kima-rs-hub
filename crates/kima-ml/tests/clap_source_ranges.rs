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
fn source_ranges_match_transcribed_python_reference() {
    for line in include_str!("fixtures/clap_ranges_reference.txt").lines() {
        let mut fields = line.split_whitespace();
        let frames: usize = fields.next().unwrap().parse().unwrap();
        let rate: u32 = fields.next().unwrap().parse().unwrap();
        let expected: Vec<_> = fields
            .map(|pair| {
                let (start, end) = pair.split_once(':').unwrap();
                start.parse::<usize>().unwrap()..end.parse::<usize>().unwrap()
            })
            .collect();
        assert_eq!(
            source_window_ranges(frames, rate).unwrap(),
            expected,
            "{line}"
        );
    }
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
