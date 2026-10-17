#!/usr/bin/env bash
# Generates a small, deliberately messy music library of ffmpeg sine tones (no real audio).
# Usage: tools/make-fixture-library.sh <output-dir>
set -euo pipefail

out="${1:?usage: make-fixture-library.sh <output-dir>}"
mkdir -p "$out"

# track <relative path> <frequency> [key=value tag ...]
track() {
    local rel="$1" freq="$2"
    shift 2
    local args=()
    for kv in "$@"; do args+=(-metadata "$kv"); done
    mkdir -p "$(dirname "$out/$rel")"
    ffmpeg -nostdin -loglevel error -y -f lavfi -i "sine=frequency=${freq}:duration=6" \
        -map_metadata -1 -c:a libmp3lame -b:a 64k "${args[@]}" "$out/$rel"
}

# 1. Untagged: artist, year and album come from the folder names only.
track "Untagged Artist/1999 - Plain Album/01 - First.mp3" 220
track "Untagged Artist/1999 - Plain Album/02 - Second.mp3" 240
track "Untagged Artist/1999 - Plain Album/03 - Third.mp3" 260

# 2. Tagged multi-disc album in CD1/CD2 folders.
for d in 1 2; do
    for n in 1 2 3; do
        track "Disc Band/Double Album/CD${d}/0${n} - Song ${d}-${n}.mp3" $((300 + d * 20 + n)) \
            "artist=Disc Band" "album_artist=Disc Band" "album=Double Album" \
            "title=Song ${d}-${n}" "track=${n}" "disc=${d}" "date=2004"
    done
done

# 3. Compilation with different track artists.
for n in 1 2 3; do
    track "Compilations/Mixed Bag/0${n} - Tune ${n}.mp3" $((400 + n * 10)) \
        "artist=Guest ${n}" "album_artist=Various Artists" "album=Mixed Bag" \
        "title=Tune ${n}" "track=${n}" "compilation=1" "date=2010"
done

# 4. Hyphenated artist folder.
for n in 1 2; do
    track "Sleater-Kinney/2005 - The Woods/0${n} - Woods ${n}.mp3" $((500 + n * 10)) \
        "artist=Sleater-Kinney" "album_artist=Sleater-Kinney" "album=The Woods" \
        "title=Woods ${n}" "track=${n}" "date=2005"
done
