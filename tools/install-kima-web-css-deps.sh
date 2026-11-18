#!/usr/bin/env bash
# Installs the three packages tools/gen-kima-web-css.mjs needs into an ISOLATED directory (never the repository):
# exact versions, so the generated crates/kima-web/assets/kima.css is reproducible locally and in CI.
#   tools/install-kima-web-css-deps.sh [dir]
# dir defaults to $KIMA_CSS_DEPS, then ${XDG_CACHE_HOME:-~/.cache}/kima-rs/tailwind. Prints the directory.
# The versions come from tools/kima-web-css-deps.json (the upstream kima-hub frontend lockfile at the pin:
# tailwindcss 4.2.1, @tailwindcss/postcss 4.2.1, postcss 8.5.23), which the generator also enforces. Transitive
# dependencies are resolved by npm at install time (no lockfile is checked in).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tools/tools-env.inc.sh
. "$here/tools-env.inc.sh"
dir="${1:-${KIMA_CSS_DEPS:-${XDG_CACHE_HOME:-$HOME/.cache}/kima-rs/tailwind}}"
mapfile -t specs < <(node -e 'const p=require(process.argv[1]).packages;for(const [n,v] of Object.entries(p))console.log(`${n}@${v}`)' \
  "$here/kima-web-css-deps.json")
[ "${#specs[@]}" -eq 3 ] || { echo "tools/kima-web-css-deps.json must list exactly three packages" >&2; exit 2; }
mkdir -p "$dir"
cd "$dir"
[ -f package.json ] || echo '{"private":true}' >package.json
npm install --no-audit --no-fund --save-exact "${specs[@]}" >&2
pwd
