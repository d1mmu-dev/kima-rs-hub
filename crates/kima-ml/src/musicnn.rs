//! MusiCNN patching of already-computed mel frames.
//!
//! The frame producer is deliberately separate: no mel frontend or model
//! inference is implemented here. Patching follows the pinned upstream
//! `services/audio-analyzer/musicnn_onnx.py` at commit
//! `12faf1cd34ff53054221818b0ccde58b8e9b0020`.

/// Borrow complete 187-frame patches at a stride of 93 frames, dropping any
/// incomplete trailing patch. Each input frame contains 96 mel bands.
pub fn mel_patches(frames: &[[f32; 96]]) -> impl Iterator<Item = &[[f32; 96]]> {
    frames.windows(187).step_by(93)
}
