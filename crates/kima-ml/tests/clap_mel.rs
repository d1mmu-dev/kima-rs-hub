use kima_ml::clap::ClapMelFrontend;

fn check_reference(samples: &[f32], reference: &[u8]) {
    let output = ClapMelFrontend::new().compute(samples).unwrap();
    assert_eq!(output.len(), 1001);
    assert_eq!(reference.len(), 1001 * 64 * 4);
    for (i, (actual, bytes)) in output
        .iter()
        .flatten()
        .zip(reference.chunks_exact(4))
        .enumerate()
    {
        let expected = f32::from_le_bytes(bytes.try_into().unwrap());
        assert!(
            (actual - expected).abs() < 0.002,
            "index {i}: {actual} != {expected}"
        );
    }
}

#[test]
fn short_repeatpad_matches_python_all_frames_and_bands() {
    let samples: Vec<f32> = (0..737)
        .map(|n| ((n * 37 % 211) as f32 - 105.0) / 256.0)
        .collect();
    check_reference(&samples, include_bytes!("fixtures/clap_mel_short.f32le"));
}

#[test]
fn broadband_matches_python_all_frames_and_bands() {
    let samples: Vec<f32> = (0..480_000)
        .map(|n| ((n * 97 % 1021) as f32 - 510.0) / 1024.0)
        .collect();
    check_reference(
        &samples,
        include_bytes!("fixtures/clap_mel_broadband.f32le"),
    );
}

#[test]
fn endpoints_and_reflection_match_python() {
    let mut samples = vec![0.0; 480_000];
    samples[0] = 1.0;
    samples[479_999] = -0.5;
    check_reference(&samples, include_bytes!("fixtures/clap_mel_impulse.f32le"));
}

#[test]
fn rejects_invalid_and_nonfinite_values_and_output() {
    let frontend = ClapMelFrontend::new();
    assert!(frontend.compute(&[]).is_err());
    assert!(frontend.compute(&vec![0.0; 480_001]).is_err());
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
        assert!(frontend.compute(&[sample]).is_err());
    }
}

#[test]
fn silence_floor_is_minus_100_db() {
    let output = ClapMelFrontend::new().compute(&[0.0]).unwrap();
    assert!(output.iter().flatten().all(|v| *v == -100.0));
}

#[test]
fn multiple_frequency_bins_match_python() {
    let samples: Vec<f32> = (0..480_000)
        .map(|n| {
            let wave = |period| if n % period < period / 2 { 1.0 } else { -1.0 };
            (wave(256) + wave(16) + wave(4)) / 4.0
        })
        .collect();
    check_reference(&samples, include_bytes!("fixtures/clap_mel_tones.f32le"));
}
