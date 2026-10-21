#!/usr/bin/env bash
# Regenerates the chromaprint known-answer fixture (spec §9.5):
#   tools/fixtures/chromaprint/tone.wav          30 s, 11025 Hz mono s16, a fixed multi-tone sequence
#   tools/fixtures/chromaprint/tone.fpcalc.json  `fpcalc -json tone.wav`, plus "_fpcalc" (its version)
# The committed JSON was made with: fpcalc version 1.6.2 (FFmpeg Lavc63.1.102 Lavf63.1.102 SwR7.1.102)
# (chromaprint 1.6.2: the spec asked for 1.5.x; 1.6.2 is what the dev machine ships, see D-106).
# The WAV is already 11025 Hz mono, so neither fpcalc nor kima resamples it and the
# fingerprints must be equal. Regenerating needs ffmpeg and fpcalc (chromaprint) on PATH.
# Usage: tools/make-fingerprint-fixture.sh [output-dir]
set -euo pipefail

out="${1:-$(dirname "$0")/fixtures/chromaprint}"
mkdir -p "$out"

# A chord whose root steps every 2 s, a bass note stepping every 3 s and a high tone pulsing
# at 2 Hz: enough change over time for a fingerprint with structure. Keep it fixed.
expr='0.30*sin(2*PI*(220+55*mod(floor(t/2),5))*t)'
expr+='+0.20*sin(2*PI*(330+55*mod(floor(t/2),5))*t)'
expr+='+0.15*sin(2*PI*(110*(1+mod(floor(t/3),4)))*t)'
expr+='+0.10*sin(2*PI*1760*t)*(0.5+0.5*sin(2*PI*2*t))'

ffmpeg -nostdin -v error -y -f lavfi -i "aevalsrc='${expr}':s=11025:d=30" \
    -map_metadata -1 -fflags +bitexact -flags:a +bitexact \
    -ac 1 -ar 11025 -c:a pcm_s16le "$out/tone.wav"

version="$(fpcalc -version)"
fpcalc -json "$out/tone.wav" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d["_fpcalc"]=sys.argv[1]; print(json.dumps(d, indent=2))' "$version" \
    > "$out/tone.fpcalc.json"
