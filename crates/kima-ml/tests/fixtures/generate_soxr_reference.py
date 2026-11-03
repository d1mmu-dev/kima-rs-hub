"""Regenerate synthetic f32le references with librosa 0.11.0 and soxr 1.1.0.

Requires numpy; no model assets or copyrighted recordings are used.
"""
from pathlib import Path
import librosa
import numpy as np
import soxr

assert librosa.__version__ == '0.11.0' and soxr.__version__ == '1.1.0'
x = np.array([((i * 37 % 101) - 50) / 64 for i in range(257)], dtype=np.float32)
x[0], x[-1] = 1.0, -1.0
for rate in (8000, 16000, 44100, 48000, 96000, 192000):
    y = librosa.resample(x, orig_sr=rate, target_sr=48000, res_type='soxr_hq')
    (Path(__file__).parent / f'soxr-{rate}.f32le').write_bytes(y.astype('<f4').tobytes())
