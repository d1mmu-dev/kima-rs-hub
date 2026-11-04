#!/bin/sh
# Verify the loader's actual dynamic dependency resolution for a native probe.
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: sh $0 ELF_BINARY ABSOLUTE_SOXR_PREFIX" >&2; exit 1
fi
links=$(ldd "$1")
printf '%s\n' "$links"
case "$2" in /*) ;; *) echo 'soxr prefix must be absolute' >&2; exit 1 ;; esac
if printf '%s\n' "$links" | grep -Eq '=>[[:space:]]*not found([[:space:]]|$)'; then
    echo 'unresolved dynamic dependency' >&2; exit 1
fi
printf '%s\n' "$links" | grep -F "libsoxr.so.0 => $2/lib/libsoxr.so.0 ("
printf '%s\n' "$links" | grep -E '^[[:space:]]*libgomp\.so\.1[[:space:]]+=>[[:space:]]+/[^[:space:]]+[[:space:]]+\('
