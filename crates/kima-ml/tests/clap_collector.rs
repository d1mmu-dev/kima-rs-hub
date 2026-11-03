// These are deliberately single window ranges, not expanded ranges of indices.
#![allow(clippy::single_range_in_vec_init)]

use kima_ml::clap::WindowCollector;

#[test]
fn collects_disjoint_and_overlapping_ranges_across_chunks() {
    let mut collector = WindowCollector::new(10, &[1..5, 3..7, 9..10], 9).unwrap();
    collector.push(&[0.0, 1.0]).unwrap();
    collector.push(&[2.0, 3.0, 4.0, 5.0, 6.0, 7.0]).unwrap();
    collector.push(&[8.0, 9.0]).unwrap();
    assert_eq!(
        collector.finish().unwrap(),
        vec![
            vec![1.0, 2.0, 3.0, 4.0],
            vec![3.0, 4.0, 5.0, 6.0],
            vec![9.0]
        ]
    );
}

#[test]
fn overrun_permanently_invalidates_collector() {
    use kima_ml::clap::WindowCollectorError::FrameCount;
    let mut collector = WindowCollector::new(2, &[0..2], 2).unwrap();
    collector.push(&[1.0]).unwrap();
    assert_eq!(collector.push(&[2.0, 3.0]), Err(FrameCount));
    assert_eq!(collector.push(&[2.0]), Err(FrameCount));
    assert_eq!(collector.finish(), Err(FrameCount));
}

#[test]
fn rejects_invalid_ranges_and_budgets_before_processing() {
    use kima_ml::clap::WindowCollectorError::InvalidRanges;
    for (expected, ranges, budget) in [
        (10, vec![2..5], 2),
        (10, vec![11..11], 10),
        (10, vec![std::ops::Range { start: 8, end: 7 }], 10),
        (10, vec![0..0; 6], 0),
        (usize::MAX, vec![0..usize::MAX, 0..1], usize::MAX),
    ] {
        assert!(matches!(
            WindowCollector::new(expected, &ranges, budget),
            Err(InvalidRanges)
        ));
    }
}

#[test]
fn short_input_never_yields_partial_windows() {
    let mut collector = WindowCollector::new(10, &[0..3], 3).unwrap();
    collector.push(&[1.0, 2.0, 3.0]).unwrap();
    assert_eq!(
        collector.finish(),
        Err(kima_ml::clap::WindowCollectorError::FrameCount)
    );
}

#[test]
fn empty_ranges_repeated_ranges_and_empty_chunks_are_preserved() {
    let mut collector = WindowCollector::new(3, &[1..1, 0..3, 0..3, 3..3], 6).unwrap();
    collector.push(&[]).unwrap();
    collector.push(&[0.25, -2.0, 3.0]).unwrap();
    collector.push(&[]).unwrap();
    assert_eq!(
        collector.finish().unwrap(),
        vec![vec![], vec![0.25, -2.0, 3.0], vec![0.25, -2.0, 3.0], vec![]]
    );
    assert_eq!(
        WindowCollector::new(0, &[0..0], 0)
            .unwrap()
            .finish()
            .unwrap(),
        vec![Vec::<f32>::new()]
    );
}

#[test]
fn bounded_windows_match_slices_for_many_chunk_partitions() {
    let samples: Vec<f32> = (0..1003).map(|i| i as f32 - 400.0).collect();
    let ranges = [900..1003, 0..13, 512..543, 10..17, 512..543];
    let expected: Vec<Vec<f32>> = ranges.iter().map(|r| samples[r.clone()].to_vec()).collect();
    for chunk_size in [1, 2, 7, 64, 1003, 2000] {
        let mut collector = WindowCollector::new(samples.len(), &ranges, 185).unwrap();
        for chunk in samples.chunks(chunk_size) {
            collector.push(chunk).unwrap();
        }
        assert_eq!(
            collector.finish().unwrap(),
            expected,
            "chunk size {chunk_size}"
        );
    }
}
