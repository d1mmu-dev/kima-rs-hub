//! CLAP zero-shot arithmetic; c7527577 services/audio-analyzer-clap/analyzer.py.
//! Prompt bytes/provenance and independent Python reference: tests/fixtures/zero-shot-reference.json.

/// Group order is part of the arithmetic contract; each group contains three prompts.
pub const PROMPTS: [(&str, [&str; 3]); 8] = [
    (
        "vocal",
        [
            "music with singing vocals",
            "song with a singer",
            "vocal music with lyrics",
        ],
    ),
    (
        "instrumental",
        [
            "instrumental music without vocals",
            "instrumental track with no singing",
            "music without any vocals or singing",
        ],
    ),
    (
        "acoustic",
        [
            "soft acoustic guitar and piano music",
            "unplugged unamplified intimate performance",
            "gentle stripped-back acoustic recording",
        ],
    ),
    (
        "electronic",
        [
            "heavily produced studio track with electric instruments",
            "distorted electric guitar and amplified drums",
            "polished pop production with layered synths and effects",
        ],
    ),
    (
        "happy",
        [
            "happy cheerful uplifting music",
            "joyful bright positive song",
            "feel good upbeat music",
        ],
    ),
    (
        "sad",
        [
            "sad melancholic somber music",
            "dark gloomy depressing song",
            "sorrowful mournful music",
        ],
    ),
    (
        "high_energy",
        [
            "intense energetic powerful music",
            "loud fast aggressive song",
            "high energy driving music",
        ],
    ),
    (
        "low_energy",
        [
            "calm relaxing peaceful music",
            "soft quiet gentle song",
            "mellow soothing ambient music",
        ],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Readings {
    pub instrumentalness: f64,
    pub acousticness: f64,
    pub valence_raw: f64,
    pub arousal_raw: f64,
}

/// The winner's share; temperature multiplies the cosine, it does not divide it.
pub fn softmax_share(winner: f64, other: f64, temperature: f64) -> f64 {
    let a = (winner * temperature).exp();
    let b = (other * temperature).exp();
    a / (a + b)
}

fn round3(value: f64) -> f64 {
    let value = value.clamp(0., 1.);
    if !value.is_finite() {
        return value;
    }
    // Round the exact binary64 fraction to a decimal thousandth, as Python
    // round(x,3) does. A floating x*1000 introduces a second rounding first.
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as u32;
    if exponent == 0 {
        return 0.;
    }
    let numerator = ((bits & ((1_u64 << 52) - 1)) | (1_u64 << 52)) * 1000;
    let shift = 1075 - exponent;
    if shift >= 64 {
        return 0.;
    }
    let quotient = numerator >> shift;
    let remainder = numerator & ((1_u64 << shift) - 1);
    let half = 1_u64 << (shift - 1);
    let increment = remainder > half || (remainder == half && quotient % 2 == 1);
    (quotient + u64::from(increment)) as f64 / 1000.
}

impl Readings {
    /// Similarities in PROMPTS order. Reject non-finite evidence instead of manufacturing zeros.
    pub fn from_similarities(s: [f64; 8]) -> Result<Self, &'static str> {
        if !s.iter().all(|v| v.is_finite()) {
            return Err("non-finite similarity");
        }
        let platt = |a: f64, b: f64, margin: f64| round3(1. / (1. + (-(a * margin + b)).exp()));
        Ok(Self {
            instrumentalness: round3(softmax_share(s[1], s[0], 15.)),
            acousticness: round3(softmax_share(s[2], s[3], 15.)),
            valence_raw: platt(8.264, 0.249, s[4] - s[5]),
            arousal_raw: platt(10.654, 0.018, s[6] - s[7]),
        })
    }

    /// All 512 dimensions; no audio decode. Inputs may be slightly off unit norm.
    pub fn compute(audio: &[f32; 512], prompts: &[[f32; 512]]) -> Result<Self, &'static str> {
        if prompts.len() != 24 {
            return Err("expected 24 prompt vectors");
        }
        let norm = |v: &[f32; 512]| -> Result<f64, &'static str> {
            if !v.iter().all(|x| x.is_finite()) {
                return Err("non-finite vector");
            }
            let n = v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
            if n == 0. {
                return Err("zero vector");
            }
            Ok(n)
        };
        let an = norm(audio)?;
        let mut s = [0.; 8];
        for (i, prompt) in prompts.iter().enumerate() {
            let pn = norm(prompt)?;
            let dot = audio
                .iter()
                .zip(prompt)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum::<f64>();
            s[i / 3] += dot / (an * pn) / 3.;
        }
        Self::from_similarities(s)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn round3_matches_python_at_binary64_decimal_halfways() {
        // CPython round(value,3), evaluated independently; multiplication by 1000
        // before round_ties_even loses the side of the decimal midpoint.
        for (value, expected) in [
            (0.0005, 0.001),
            (0.0015, 0.002),
            (0.5005, 0.5),
            (0.5015, 0.501),
            (0.5025, 0.502),
            (0.9995, 1.),
        ] {
            assert_eq!(super::round3(value), expected, "{value}");
        }
    }
}
