#!/usr/bin/env bash
# Verifies the toolchain kima-web needs: rustc, the wasm32-unknown-unknown target, wasm-opt.
set -euo pipefail

rustc --version --verbose

if ! rustup target list --installed 2>/dev/null | grep -qx 'wasm32-unknown-unknown'; then
  echo "FAIL: wasm32-unknown-unknown target is not installed (rustup target add wasm32-unknown-unknown)" >&2
  exit 1
fi
echo "target wasm32-unknown-unknown: installed"

if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "FAIL: wasm-opt (binaryen) not found on PATH; see crates/kima-web/README.md" >&2
  exit 1
fi
wasm-opt --version
