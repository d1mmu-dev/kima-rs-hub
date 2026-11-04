#!/bin/sh
# Build only into a new, explicit private prefix; never install into the host.
set -eu
if [ "$#" -ne 1 ]; then
    echo "usage: sh $0 NEW_ABSOLUTE_PREFIX" >&2; exit 1
fi
case "$1" in /*) ;; *) echo 'prefix must be absolute' >&2; exit 1 ;; esac
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) ;; *) echo 'approved soxr packaging: linux/amd64 only' >&2; exit 1 ;;
esac
test ! -e "$1"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
recipe="$script_dir/../native/soxr"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
archive=libsoxr_0.1.3.orig.tar.xz
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
    "https://deb.debian.org/debian/pool/main/libs/libsoxr/$archive" -o "$work/$archive"
printf '%s  %s\n' b111c15fdc8c029989330ff559184198c161100a59312f5dc19ddeb9b5a15889 "$work/$archive" | sha256sum -c -
tar -xJf "$work/$archive" -C "$work"
src="$work/soxr-0.1.3-Source"
patch --batch --fuzz=0 -d "$src" -p1 < "$recipe/fft-cache-init.patch"
# Scope of the reviewed fix: GCC/OpenMP, FFT4G, assertions disabled. Explicit
# flags avoid silently enabling the different default PFFFT backend.
CC=gcc cmake -S "$src" -B "$work/build" \
    -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_FLAGS_RELEASE='-O3 -DNDEBUG' \
    -DCMAKE_INSTALL_PREFIX="$1" -DCMAKE_INSTALL_LIBDIR=lib \
    -DBUILD_SHARED_LIBS=ON -DWITH_OPENMP=ON -DWITH_PFFFT=OFF \
    -DWITH_AVFFT=OFF -DWITH_LSR_BINDINGS=OFF -DBUILD_TESTS=OFF -DBUILD_EXAMPLES=OFF
cmake --build "$work/build" --parallel 2
# Upstream OpenMP discovery is optional: fail closed if it was not used.
readelf -d "$work/build/src/libsoxr.so.0.1.2" | grep -q 'libgomp.so.1'
cmake --install "$work/build"
notices="$1/share/soxr"
mkdir -p "$notices"
cp "$work/$archive" "$recipe/fft-cache-init.patch" "$recipe/README.md" \
    "$recipe/libsoxr_0.1.3-4.dsc" "$script_dir/build-soxr.sh" \
    "$src/LICENCE" "$src/COPYING.LGPL" "$src/AUTHORS" "$notices/"
cp "$work/build/CMakeCache.txt" "$notices/"
sha256sum "$1/lib/libsoxr.so.0.1.2" > "$notices/library.sha256"
