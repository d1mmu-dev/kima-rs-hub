"""Generate clap_ranges_reference.txt using only Python's binary64 arithmetic.

Transcribed from compute_window_offsets and _load_window at kima-hub commit
12faf1cd34ff53054221818b0ccde58b8e9b0020, services/audio-analyzer-clap/analyzer.py.
Frame conversions follow librosa 0.11.0 core/audio.py __soundfile_load:
https://github.com/librosa/librosa/blob/0.11.0/librosa/core/audio.py
This does not execute upstream code, read audio, or establish waveform parity.
Run: python3 crates/kima-ml/tests/fixtures/clap_ranges_reference.py > crates/kima-ml/tests/fixtures/clap_ranges_reference.txt
"""


def ranges(frames, sr):
    duration = frames / sr
    n = min(5, max(1, int(duration // 10.0)))
    max_start = max(0.0, duration - 10.0)
    if n == 1:
        offsets = [0.0]
    elif n < 5:
        step = (duration - 10.0) / (n - 1)
        offsets = [i * step for i in range(n)]
    else:
        offsets = [(i + 0.5) / 5 * duration for i in range(5)]
        offsets[-1] = min(offsets[-1], max_start)
        if offsets[-1] < offsets[-2] + 10.0:
            step = (duration - 10.0) / 4
            offsets = [i * step for i in range(5)]
    result = []
    for start in offsets:
        start = max(0.0, min(start, max(0.0, duration - 1e-3)))
        take = min(10.0, max(0.0, duration - start))
        first = int(start * sr)
        length = int(take * sr) if take > 0 else frames - first
        result.append((first, min(frames, first + length)))
    return result


for frames, sr in [(1, 48000), (432001, 48000), (251, 10),
                   (960000, 48000), (1920001, 48000),
                   (3200000, 48000), (3216000, 48000),
                   (12345678, 44100), (10000001, 192000),
                   (19200000, 48000)]:
    print(frames, sr, *(f"{a}:{b}" for a, b in ranges(frames, sr)))
