use kima_ml::clap::prepare_window;

#[test]
fn synthetic_waveforms_match_pinned_librosa_at_multiple_rates() {
    let mut input: Vec<f32> = (0..257)
        .map(|i| ((i * 37 % 101) as f32 - 50.0) / 64.0)
        .collect();
    input[0] = 1.0;
    input[256] = -1.0;
    let cases: &[(u32, &[u8])] = &[
        (8000, include_bytes!("fixtures/soxr-8000.f32le")),
        (16000, include_bytes!("fixtures/soxr-16000.f32le")),
        (44100, include_bytes!("fixtures/soxr-44100.f32le")),
        (48000, include_bytes!("fixtures/soxr-48000.f32le")),
        (96000, include_bytes!("fixtures/soxr-96000.f32le")),
        (192000, include_bytes!("fixtures/soxr-192000.f32le")),
    ];
    for &(rate, bytes) in cases {
        let reference: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let actual = prepare_window(&input, rate).unwrap();
        let repeated = 480_000 / reference.len() * reference.len();
        for (i, &value) in actual.iter().enumerate() {
            let expected = if i < repeated {
                reference[i % reference.len()]
            } else {
                0.0
            };
            assert!(
                (value - expected).abs() < 3e-6,
                "rate={rate} index={i}: {value} != {expected}"
            );
        }
    }
}

#[test]
fn short_upsample_matches_librosa_soxr_hq() {
    // librosa 0.11.0, python-soxr 1.1.0, input float32 [1.0].
    let expected = [
        0.9431157, 0.990415, 0.8996531, 0.69999254, 0.44553354, 0.19518423,
    ];
    let output = prepare_window(&[1.0], 8_000).unwrap();
    for (actual, expected) in output.iter().zip(expected) {
        assert!((actual - expected).abs() < 2e-6, "{actual} != {expected}");
    }
}

#[test]
fn same_rate_repeatpad_uses_whole_repeats_then_zero_remainder() {
    let input = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
    let output = prepare_window(&input, 48_000).unwrap();
    assert_eq!(output.len(), 480_000);
    for (i, sample) in output.iter().enumerate() {
        let expected = if i < (480_000 / 7) * 7 {
            input[i % 7]
        } else {
            0.0
        };
        assert_eq!(*sample, expected);
    }
}

#[test]
fn downsample_single_frame_uses_librosa_ceiling_length() {
    let output = prepare_window(&[0.0], 192_000).unwrap();
    assert_eq!(output.len(), 480_000);
    assert!(output.iter().all(|sample| *sample == 0.0));
    let constant = prepare_window(&[0.75], 48_000).unwrap();
    assert!(constant.iter().all(|sample| *sample == 0.75));
}

#[test]
fn rejects_unsupported_rate_and_oversized_window() {
    use kima_ml::audio::AudioError;
    assert!(matches!(
        prepare_window(&[1.0], 1),
        Err(AudioError::UnsupportedSampleRate(1))
    ));
    assert!(matches!(
        prepare_window(&vec![0.0; 480_001], 48_000),
        Err(AudioError::BufferOverflow)
    ));
}

#[test]
fn empty_and_nonfinite_inputs_are_rejected() {
    use kima_ml::audio::AudioError;
    assert!(matches!(
        prepare_window(&[], 48_000),
        Err(AudioError::EmptyAudio)
    ));
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
        assert!(matches!(
            prepare_window(&[sample], 48_000),
            Err(AudioError::CorruptAudio(_))
        ));
        assert!(matches!(
            prepare_window(&[sample], 44_100),
            Err(AudioError::CorruptAudio(_))
        ));
    }
}

#[test]
fn separate_resamplers_do_not_leak_between_windows() {
    let silent = vec![0.0; 4_410];
    let mut impulse = silent.clone();
    impulse[4_409] = 1.0;
    let _ = prepare_window(&impulse, 44_100).unwrap();
    let after = prepare_window(&silent, 44_100).unwrap();
    assert!(after.iter().all(|v| *v == 0.0));
}

#[test]
fn concurrent_resampling_matches_independent_serial_results() {
    use std::sync::{Arc, Barrier};

    let cases: Vec<_> = [8_000, 16_000, 44_100, 96_000, 192_000]
        .into_iter()
        .enumerate()
        .map(|(case, rate)| {
            let input: Vec<f32> = (0..4_411)
                .map(|i| (((i * (case + 3)) % 113) as f32 - 56.0) / 64.0)
                .collect();
            let expected = prepare_window(&input, rate).unwrap();
            (input, rate, expected)
        })
        .collect();
    let barrier = Arc::new(Barrier::new(cases.len()));
    let threads: Vec<_> = cases
        .into_iter()
        .map(|(input, rate, expected)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..3 {
                    assert_eq!(
                        prepare_window(&input, rate).unwrap(),
                        expected,
                        "rate={rate}"
                    );
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn ceiling_output_repeats_then_zero_pads() {
    // 5 samples at 192 kHz produce ceil(5/4) = 2 samples; floor repeat
    // fills 480000 exactly. A nearest-rounded implementation uses one sample.
    let output = prepare_window(&[1.0, 0.0, 0.0, 0.0, 0.0], 192_000).unwrap();
    assert_ne!(output[0], output[1]);
    assert!(output.chunks_exact(2).all(|pair| pair == &output[..2]));
}
