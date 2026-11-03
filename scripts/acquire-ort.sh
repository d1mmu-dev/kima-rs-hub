#!/bin/sh
# Explicit build-time acquisition only; provisioning verifies both pinned hashes.
set -eu
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) ;;
    *) echo 'ORT packaging supports only linux/amd64; approve a runtime for this platform first' >&2; exit 1 ;;
esac
if [ "$#" -ne 1 ]; then
    echo "usage: $0 NEW_DESTINATION" >&2
    exit 1
fi
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
wheel=$(mktemp)
trap 'rm -f "$wheel"' EXIT HUP INT TERM
curl --fail --location --proto '=https' --tlsv1.2 \
    'https://files.pythonhosted.org/packages/8c/60/16d219b8868cc8e8e51a68519873bdb9f5f24af080b62e917a13fff9989b/onnxruntime-1.22.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl' \
    --output "$wheel"
python3 "$script_dir/provision-ort.py" "$wheel" "$1"
chmod -R a-w "$1"
