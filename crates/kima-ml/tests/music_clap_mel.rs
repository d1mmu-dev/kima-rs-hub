//! The music CLAP log-mel front end against upstream `music_clap_mel.py` (kima-hub c7527577),
//! run itself on synthetic signals by `fixtures/music_clap_mel_reference.py`.
use kima_ml::clap::MusicClapMel;

/// Largest absolute difference in dB tolerated per value (outputs are float32 dB).
const TOLERANCE_DB: f32 = 1e-3;

fn check_reference(samples: &[f32], reference: &[u8]) {
    let output = MusicClapMel::new().compute(samples).unwrap();
    assert_eq!(output.len(), 1001);
    assert_eq!(reference.len(), 1001 * 64 * 4);
    let mut worst = 0.0_f32;
    for (i, (actual, bytes)) in output
        .iter()
        .flatten()
        .zip(reference.chunks_exact(4))
        .enumerate()
    {
        let expected = f32::from_le_bytes(bytes.try_into().unwrap());
        let error = (actual - expected).abs();
        worst = worst.max(error);
        assert!(error < TOLERANCE_DB, "index {i}: {actual} != {expected}");
    }
    println!("maximum absolute error {worst:e} dB");
}

#[test]
fn short_repeatpad_matches_upstream_all_frames_and_bands() {
    let samples: Vec<f32> = (0..737)
        .map(|n| ((n * 37 % 211) as f32 - 105.0) / 256.0)
        .collect();
    check_reference(
        &samples,
        include_bytes!("fixtures/music_clap_mel_short.f32le"),
    );
}

#[test]
fn broadband_matches_upstream_all_frames_and_bands() {
    let samples: Vec<f32> = (0..480_000)
        .map(|n| ((n * 97 % 1021) as f32 - 510.0) / 1024.0)
        .collect();
    check_reference(
        &samples,
        include_bytes!("fixtures/music_clap_mel_broadband.f32le"),
    );
}

#[test]
fn endpoints_and_reflection_match_upstream() {
    let mut samples = vec![0.0; 480_000];
    samples[0] = 1.0;
    samples[479_999] = -0.5;
    check_reference(
        &samples,
        include_bytes!("fixtures/music_clap_mel_impulse.f32le"),
    );
}

#[test]
fn tones_above_the_cutoff_match_upstream() {
    let square = |n: usize, period: usize| if n % period < period / 2 { 1.0 } else { -1.0 };
    let samples: Vec<f32> = (0..480_000)
        .map(|n| (square(n, 256) + square(n, 16) + square(n, 4)) / 4.0)
        .collect();
    check_reference(
        &samples,
        include_bytes!("fixtures/music_clap_mel_tones.f32le"),
    );
}

#[test]
fn int16_round_trip_clips_and_truncates_like_upstream() {
    // numpy evaluates the whole expression in float64 and rounds once to float32; values
    // beyond [-1, 1] are clipped by the int16 round trip.
    let samples: Vec<f32> = (0..480_000_u32)
        .map(|n| (1.7 * (f64::from(n) * 0.0123).sin() + 0.000013 * f64::from(n % 7)) as f32)
        .collect();
    check_reference(
        &samples,
        include_bytes!("fixtures/music_clap_mel_loud.f32le"),
    );
}

#[test]
fn quantize_matches_the_laion_int16_round_trip() {
    use kima_ml::clap::music_quantize;
    // clip to [-1, 1], float32 multiply by 32767, truncate to int16, divide in float64.
    assert_eq!(music_quantize(2.0), 1.0);
    assert_eq!(music_quantize(-3.0), -1.0);
    assert_eq!(music_quantize(0.0), 0.0);
    let x = 0.123_456_7_f32;
    let expected = (((x * 32767.0) as i16) as f64 / 32767.0) as f32;
    assert_eq!(music_quantize(x), expected);
    // Truncation toward zero, not rounding: 0.99999 * 32767 = 32766.67 -> 32766.
    assert_eq!(music_quantize(0.99999), (32766.0_f64 / 32767.0) as f32);
    assert_eq!(music_quantize(-0.99999), (-32766.0_f64 / 32767.0) as f32);
}

#[test]
fn rejects_empty_nonfinite_and_over_ten_second_input() {
    let frontend = MusicClapMel::new();
    assert!(frontend.compute(&[]).is_err());
    assert!(frontend.compute(&[f32::NAN]).is_err());
    assert!(frontend.compute(&[f32::INFINITY, 0.0]).is_err());
    assert!(frontend.compute(&vec![0.0; 480_001]).is_err());
    assert_eq!(frontend.compute(&[0.0]).unwrap().len(), 1001);
}
