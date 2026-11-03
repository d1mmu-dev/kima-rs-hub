"""Synthetic CLAP features; numpy 1.24.4, transformers 4.57.6.

Reproduce clap_mel.py at kima-hub 12faf1cd34ff53054221818b0ccde58b8e9b0020
using its audio_utils calls and default repeatpad. No model assets are used.
Run from this directory with OPENBLAS_NUM_THREADS=1 python clap_mel_reference.py.
"""
from pathlib import Path
import numpy as np
from transformers.audio_utils import mel_filter_bank, spectrogram, window_function

filters = mel_filter_bank(513, 64, 50.0, 14000.0, 48000, norm="slaney", mel_scale="slaney")
window = window_function(1024, "hann")
for name, length in [("short", 737), ("broadband", 480000), ("impulse", 480000), ("tones", 480000)]:
    n = np.arange(length, dtype=np.int64)
    if name == "short":
        audio = (((n * 37 % 211) - 105) / 256).astype(np.float32)
    elif name == "broadband":
        audio = (((n * 97 % 1021) - 510) / 1024).astype(np.float32)
    elif name == "impulse":
        audio = np.zeros(length, np.float32)
        audio[0], audio[-1] = 1.0, -0.5
    else:
        # Exact-bin square waves, including energy above the 14 kHz cutoff.
        audio = (((n % 256 < 128).astype(np.float32) * 2 - 1) +
                 ((n % 16 < 8).astype(np.float32) * 2 - 1) +
                 ((n % 4 < 2).astype(np.float32) * 2 - 1)) / 4
    audio = np.tile(audio, 480000 // length)
    audio = np.pad(audio, (0, 480000 - len(audio)))
    features = spectrogram(audio, window, 1024, 480, power=2.0, mel_filters=filters, log_mel="dB").T
    assert features.shape == (1001, 64)
    Path(f"clap_mel_{name}.f32le").write_bytes(features.astype("<f4").tobytes())
