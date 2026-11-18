"""Synthetic log-mel references for the music CLAP front end (`MusicClapMel`).

Runs upstream kima-hub c7527577 `services/audio-analyzer-clap/music_clap_mel.py` itself (not a
copy): point KIMA_UPSTREAM_CLAP_DIR at that directory of a checkout of commit
c7527577d925b20f5adf410c7fd4e0c8ac250f69. Generated with Python 3.11.16, numpy 1.26.4 and
librosa 0.11.0 (librosa only builds the filter bank). Synthetic signals only; no model assets.

    KIMA_UPSTREAM_CLAP_DIR=<checkout>/services/audio-analyzer-clap \
    OPENBLAS_NUM_THREADS=1 python music_clap_mel_reference.py

Each output is the [1001, 64] float32 log-mel of `quantize` + `fit_window(crop_start=0)` +
`logmel`, frame-major little-endian, exactly what `MusicClapMel.__call__(audio, 0)` returns.
"""
import os
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, os.environ["KIMA_UPSTREAM_CLAP_DIR"])
from music_clap_mel import MusicClapMel  # noqa: E402

HERE = Path(__file__).resolve().parent
frontend = MusicClapMel()
for name, length in [("short", 737), ("broadband", 480000), ("impulse", 480000),
                     ("tones", 480000), ("loud", 480000)]:
    n = np.arange(length, dtype=np.int64)
    if name == "short":
        audio = (((n * 37 % 211) - 105) / 256).astype(np.float32)
    elif name == "broadband":
        audio = (((n * 97 % 1021) - 510) / 1024).astype(np.float32)
    elif name == "impulse":
        audio = np.zeros(length, np.float32)
        audio[0], audio[-1] = 1.0, -0.5
    elif name == "tones":
        # Exact-bin square waves, including energy above the 14 kHz cutoff.
        audio = (((n % 256 < 128).astype(np.float32) * 2 - 1) +
                 ((n % 16 < 8).astype(np.float32) * 2 - 1) +
                 ((n % 4 < 2).astype(np.float32) * 2 - 1)) / 4
    else:
        # Exceeds [-1, 1] (clipped by the int16 round trip) with fractional int16 truncation.
        audio = (1.7 * np.sin(n * 0.0123) + 0.000013 * (n % 7)).astype(np.float32)
    features = frontend(audio, 0)
    assert features.shape == (1001, 64) and features.dtype == np.float32
    (HERE / f"music_clap_mel_{name}.f32le").write_bytes(features.astype("<f4").tobytes())
