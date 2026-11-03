#!/usr/bin/env python3
"""Provision a pinned Linux x86_64 CPU ORT from an operator-supplied wheel.

No network access, pip install, model extraction, or implicit runtime download.
The destination must not already exist. Preserve LICENSE + ThirdPartyNotices.
"""
import argparse
import hashlib
from pathlib import Path
import zipfile

WHEEL_SHA256 = "6964a975731afc19dc3418fad8d4e08c48920144ff590149429a5ebe0d15fb3c"
LIB_SHA256 = "7520bc1b4f649ee8fa956442a66ded303e8fd3b465b0159b7e893bdc87df4e38"
FILES = {
    "onnxruntime/capi/libonnxruntime.so.1.22.0": "libonnxruntime.so.1.22.0",
    "onnxruntime/LICENSE": "LICENSE",
    "onnxruntime/ThirdPartyNotices.txt": "ThirdPartyNotices.txt",
}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wheel", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    if hashlib.sha256(args.wheel.read_bytes()).hexdigest() != WHEEL_SHA256:
        parser.error("wheel SHA-256 mismatch")
    with zipfile.ZipFile(args.wheel) as wheel:
        contents = {target: wheel.read(source) for source, target in FILES.items()}
    if hashlib.sha256(contents["libonnxruntime.so.1.22.0"]).hexdigest() != LIB_SHA256:
        parser.error("runtime SHA-256 mismatch")
    args.destination.mkdir(mode=0o755, parents=False, exist_ok=False)
    for name, data in contents.items():
        with (args.destination / name).open("xb") as output:
            output.write(data)
        (args.destination / name).chmod(0o644)
    print(f"ORT_DYLIB_PATH={args.destination.resolve() / 'libonnxruntime.so.1.22.0'}")

if __name__ == "__main__":
    main()
