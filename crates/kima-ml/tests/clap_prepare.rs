use kima_ml::clap::prepare_window;

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
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
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
fn ceiling_output_repeats_then_zero_pads() {
    // 5 samples at 192 kHz produce ceil(5/4) = 2 samples; floor repeat
    // fills 480000 exactly. A nearest-rounded implementation uses one sample.
    let output = prepare_window(&[1.0, 0.0, 0.0, 0.0, 0.0], 192_000).unwrap();
    assert_ne!(output[0], output[1]);
    assert!(output.chunks_exact(2).all(|pair| pair == &output[..2]));
}
