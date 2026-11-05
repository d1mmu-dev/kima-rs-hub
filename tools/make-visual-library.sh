#!/usr/bin/env bash
# Generates the library used by tools/run-kima-web-visual.sh: 4 artists, 6 albums, 22 tracks of ffmpeg
# sine tones with real artist/album/title tags and generated cover art (folder cover.jpg + embedded).
# Usage: FFMPEG=/path/to/ffmpeg tools/make-visual-library.sh <output-dir>
set -euo pipefail
out="${1:?usage: make-visual-library.sh <output-dir>}"
ff="${FFMPEG:-ffmpeg}"
mkdir -p "$out"
cover() { # cover <file> <hex-colour-a> <hex-colour-b>
    "$ff" -nostdin -loglevel error -y -f lavfi \
        -i "gradients=s=500x500:c0=$2:c1=$3:x0=0:y0=0:x1=500:y1=500:d=1:r=1" -frames:v 1 "$1"
}
album() { # album <artist> <album> <year> <colourA> <colourB> <title>...
    local artist="$1" alb="$2" year="$3" ca="$4" cb="$5"; shift 5
    local dir="$out/$artist/$year - $alb"; mkdir -p "$dir"
    cover "$dir/cover.jpg" "$ca" "$cb"
    local n=0
    for title in "$@"; do
        n=$((n + 1))
        "$ff" -nostdin -loglevel error -y -f lavfi -i "sine=frequency=$((200 + n * 37 + ${#alb} * 11)):duration=$((20 + n * 9))" \
            -i "$dir/cover.jpg" -map 0:a -map 1:v -c:a libmp3lame -b:a 64k -c:v mjpeg -id3v2_version 3 \
            -metadata:s:v title="Album cover" -metadata:s:v comment="Cover (front)" \
            -metadata "artist=$artist" -metadata "album_artist=$artist" -metadata "album=$alb" \
            -metadata "title=$title" -metadata "track=$n" -metadata "date=$year" \
            "$dir/$(printf '%02d' "$n") - $title.mp3"
    done
}
album "Aurora Vale" "Northern Lights" 2019 0xfca200 0x773b97 "Polar Dawn" "Glass Rivers" "Ember Sky" "Slow Aurora"
album "Aurora Vale" "Quiet Harbour" 2021 0x0e7490 0x1e293b "Tide Lines" "Lantern Boats" "Salt and Rope"
album "The Midnight Owls" "After Hours" 2016 0x7c2d12 0x1c1917 "Last Train" "Neon Rain" "Back Alley Blues" "Closing Time"
album "The Midnight Owls" "Dawn Chorus" 2018 0x65a30d 0x14532d "First Light" "Bird Song" "Morning Dew"
album "Kiloton" "Static Bloom" 2022 0xbe185d 0x312e81 "Overdrive" "White Noise" "Feedback Loop" "Flicker" "Afterimage"
album "Marta Quill" "Paper Cities" 2023 0xe5e5e5 0x525252 "Folded Maps" "Origami Skyline" "Pencil Rain"
