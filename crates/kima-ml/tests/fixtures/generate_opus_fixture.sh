#!/bin/sh
# Regenerates tone_stereo_3s.opus and opus_reference.txt (synthetic, no third-party audio).
#
# Fixture: 3 s of stereo tones (440 Hz left, 660 Hz right) synthesised by ffmpeg's lavfi,
# encoded with libopus at 32 kb/s into Ogg Opus. Bitexact flags and no metadata keep it small
# and free of encoder/version tags. libopus writes a 312-frame pre-skip and an end granule
# that trims the final packet, so an exact decoder returns exactly 144000 frames at 48 kHz.
#
# Reference: the same file decoded by ffmpeg with the libopus decoder (pre-skip and end trim
# applied), stereo f32le at the native 48 kHz, averaged to mono here: "frames N" and then
# "index value" lines for a fixed set of frame indices (the first 32 frames, which move by
# 312 frames if pre-skip is ignored, 64 spread across the track, the last 32 frames).
#
# Recorded generator: ffmpeg n9.0.1-11-ge47273f4d9-20260831 (static build with --enable-libopus),
# python3 3.14.10. Output: 20621 bytes, ffprobe reports opus, 48000 Hz, 2 channels,
# initial_padding=312.
set -eu
cd "$(dirname "$0")"
ffmpeg -nostdin -hide_banner -loglevel error -y \
  -f lavfi -i 'aevalsrc=0.5*sin(2*PI*440*t)|0.3*sin(2*PI*660*t):s=48000:d=3' \
  -c:a libopus -b:a 32k -map_metadata -1 -fflags +bitexact -flags:a +bitexact \
  tone_stereo_3s.opus
ffmpeg -nostdin -hide_banner -loglevel error -c:a libopus -i tone_stereo_3s.opus \
  -f f32le -acodec pcm_f32le - | python3 -c '
import struct, sys
raw = sys.stdin.buffer.read()
pairs = struct.unpack("<%df" % (len(raw) // 4), raw)
mono = [(pairs[i] + pairs[i + 1]) / 2.0 for i in range(0, len(pairs), 2)]
n = len(mono)
idx = sorted(set(list(range(32)) + [i * (n // 64) for i in range(64)] + list(range(n - 32, n))))
print("frames", n)
for i in idx:
    print(i, repr(mono[i]))
' > opus_reference.txt
