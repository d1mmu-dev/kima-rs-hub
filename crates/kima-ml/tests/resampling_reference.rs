use kima_ml::audio::resample_mono;
use std::f64::consts::TAU;

// Analytic sine waves are independent of rubato and require no model assets.
fn tone(rate: u32, frequency: f64, len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| (0.5 * (TAU * frequency * i as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}

#[test]
fn passband_sine_preserves_phase_and_amplitude() {
    for (input_rate, output_rate) in [(44_100, 16_000), (44_100, 48_000), (48_000, 16_000)] {
        let input = tone(input_rate, 1_000.0, input_rate as usize);
        let output = resample_mono(&input, input_rate, output_rate).unwrap();
        assert_eq!(output.len(), output_rate as usize);
        // Exclude finite-clip boundaries. 1% RMS error is a broad signal sanity
        // check, not a model-parity threshold or a complete filter specification.
        // Rubato's finite symmetric filter may leave a fractional-sample
        // phase offset; fit within one output sample, not an arbitrary phase.
        let error = (-20..=20)
            .map(|step| {
                let shift = f64::from(step) / 20.0;
                output[512..output.len() - 512]
                    .iter()
                    .enumerate()
                    .map(|(j, x)| {
                        let expected = 0.5
                            * (TAU * 1_000.0 * ((j + 512) as f64 + shift) / f64::from(output_rate))
                                .sin();
                        (f64::from(*x) - expected).powi(2)
                    })
                    .sum::<f64>()
                    / (output.len() - 1024) as f64
            })
            .fold(f64::INFINITY, f64::min)
            .sqrt();
        assert!(
            error < 0.005,
            "{input_rate}->{output_rate} rms error {error}"
        );
    }
}

#[test]
fn downsampling_suppresses_above_nyquist_tone() {
    let input = tone(48_000, 12_000.0, 48_000);
    let output = resample_mono(&input, 48_000, 16_000).unwrap();
    let interior = &output[512..output.len() - 512];
    let rms = (interior.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / interior.len() as f64)
        .sqrt();
    // A naive sample drop aliases 12 kHz to 4 kHz with RMS 0.354.
    assert!(rms < 0.02, "aliased RMS {rms}");
}

#[test]
fn output_length_is_rounded_to_nearest_sample() {
    for (input_rate, output_rate) in [(44_100u32, 16_000u32), (44_100, 48_000), (48_000, 16_000)] {
        for len in [1usize, 2, 17, 1023, 1024, 1025, 44101] {
            let expected = (len as u64 * u64::from(output_rate) * 2 + u64::from(input_rate))
                / (2 * u64::from(input_rate));
            let output = resample_mono(&vec![0.0; len], input_rate, output_rate).unwrap();
            assert_eq!(
                output.len() as u64,
                expected,
                "{input_rate}->{output_rate}, {len} samples"
            );
        }
    }
}

#[test]
fn impulse_alignment_at_start_block_boundary_and_end() {
    // Choose positions on both input/output sample grids, including around an
    // FFT block boundary and at both finite-clip edges.
    // 1029 is the 44.1 kHz input block boundary at this rate pair.
    for position in [0usize, 1_029, 44_055] {
        let mut input = vec![0.0; 44_100];
        input[position] = 1.0;
        let output = resample_mono(&input, 44_100, 48_000).unwrap();
        let peak = output
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap();
        let expected = position * 160 / 147;
        assert!(
            peak.0.abs_diff(expected) <= 1,
            "input impulse {position}, output {} vs {expected}",
            peak.0
        );
        assert!(peak.1.abs() > 0.5, "lost impulse at {position}");
    }
}
