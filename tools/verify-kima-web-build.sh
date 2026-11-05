#!/usr/bin/env bash
# Asserts `vertigo build` in crates/kima-web produces build/index.json (the manifest `vertigo serve` renders
# index.html from), a hashed .wasm and a hashed wasm_run.<hash>.js. (vertigo 0.13 writes no static index.html or
# unhashed wasm_run.js; the plan's original artifact list was written against an older layout.)
# VERTIGO overrides the CLI (default: `vertigo` on PATH); its version should match the vertigo crate.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
web="$root/crates/kima-web"
vertigo="${VERTIGO:-vertigo}"

if [ ! -f "$web/Cargo.toml" ]; then
  echo "FAIL: $web/Cargo.toml does not exist (kima-web crate not scaffolded)" >&2
  exit 1
fi
"$vertigo" --version

cd "$web"
rm -rf build
"$vertigo" build

fail=0
[ -f build/index.json ] || { echo "FAIL: build/index.json missing" >&2; fail=1; }
if ! ls build/*.wasm 2>/dev/null | grep -Eq '/[^/]*\.[A-Za-z0-9_-]{8,}\.wasm$'; then
  echo "FAIL: no hashed .wasm in build/" >&2
  fail=1
fi
if ! ls build/wasm_run.*.js 2>/dev/null | grep -Eq '/wasm_run\.[A-Za-z0-9_-]{8,}\.js$'; then
  echo "FAIL: no hashed wasm_run.<hash>.js in build/" >&2
  fail=1
fi
[ "$fail" = 0 ] || { ls -la build >&2 || true; exit 1; }
echo "OK:"; ls -la build
