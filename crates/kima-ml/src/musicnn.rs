//! MusiCNN log-mel preparation and patching, without model inference.
//!
//! The frontend accepts mono 16 kHz samples, and is independently checked
//! against synthetic output from pinned Essentia. Patching follows
//! `services/audio-analyzer/musicnn_onnx.py` at commit
//! `12faf1cd34ff53054221818b0ccde58b8e9b0020`.

mod aggregate;
mod compute;
pub use aggregate::{MusicnnAggregate, MusicnnAnalysisResult, aggregate, aggregate_patches};
pub use compute::{MusicnnComputation, MusicnnCompute, MusicnnComputeError, MusicnnPatchOutputs};

/// Produce centered 512-sample frames at a 256-sample hop, padding with zero.
///
/// An empty input produces no frames. A nonempty input includes a final frame
/// centered at or just beyond its end. Each frame is produced on demand.
pub fn centered_frames(samples: &[f32]) -> impl Iterator<Item = [f32; 512]> + '_ {
    let count = if samples.is_empty() {
        0
    } else {
        samples.len().div_ceil(256) + 1
    };
    (0..count).map(move |frame_index| {
        let center = frame_index * 256;
        let mut frame = [0.0; 512];
        for (i, sample) in frame.iter_mut().enumerate() {
            if let Some(index) = center.checked_add(i).and_then(|n| n.checked_sub(256)) {
                *sample = samples.get(index).copied().unwrap_or(0.0);
            }
        }
        frame
    })
}

/// Borrow complete 187-frame patches at a stride of 93 frames, dropping any
/// incomplete trailing patch. Each input frame contains 96 mel bands.
pub fn mel_patches(frames: &[[f32; 96]]) -> impl Iterator<Item = &[[f32; 96]]> {
    frames.windows(187).step_by(93)
}

/// Computes the 16 kHz MusiCNN log-mel input without loading a model.
pub struct MusicnnFrontend {
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    filters: [[f32; 257]; 96],
    window: [f32; 512],
}

impl Default for MusicnnFrontend {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicnnFrontend {
    pub fn new() -> Self {
        let mut planner = rustfft::FftPlanner::new();
        let fft = planner.plan_fft_forward(512);
        let log_step = (6.4_f64.ln() / 27.0) as f32;
        let max_mel = 15.0 + 8.0_f32.ln() / log_step;
        let step = max_mel / 97.0;
        let mut m = 0.0_f32;
        let mut hz = [0.0_f32; 98];
        for value in &mut hz {
            *value = if m < 15.0 {
                m / 0.015
            } else {
                1000.0 * ((m - 15.0) * log_step).exp()
            };
            m += step;
        }
        let mut filters = [[0.0; 257]; 96];
        for b in 0..96 {
            let [lo, mid, hi] = [hz[b], hz[b + 1], hz[b + 2]];
            for (k, value) in filters[b].iter_mut().enumerate() {
                let freq = k as f32 * 31.25;
                let triangular = ((freq - lo) / (mid - lo))
                    .min((hi - freq) / (hi - mid))
                    .max(0.0);
                *value = triangular / ((hi - lo) / 2.0);
            }
        }
        let window = std::array::from_fn(|i| {
            (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 511.0).cos()) as f32
        });
        Self {
            fft,
            filters,
            window,
        }
    }

    pub fn compute(&mut self, samples: &[f32]) -> Result<Vec<[f32; 96]>, &'static str> {
        if samples.len() > 90 * 16_000 {
            return Err("input exceeds 90 seconds");
        }
        if samples.iter().any(|s| !s.is_finite()) {
            return Err("non-finite input");
        }
        let mut result = Vec::new();
        for frame in centered_frames(samples) {
            let mut fft: Vec<_> = frame
                .iter()
                .zip(self.window)
                .map(|(&s, w)| rustfft::num_complex::Complex32::new(s * w, 0.0))
                .collect();
            self.fft.process(&mut fft);
            let power: Vec<_> = fft[..257].iter().map(|x| x.norm_sqr()).collect();
            let bands = std::array::from_fn(|b| {
                let sum: f32 = power.iter().zip(self.filters[b]).map(|(&x, w)| x * w).sum();
                (1.0 + 10000.0 * sum).log10()
            });
            if bands.iter().any(|x| !x.is_finite()) {
                return Err("non-finite frontend output");
            }
            result.push(bands);
        }
        Ok(result)
    }
}
