#!/usr/bin/env bash
# Installs the three packages tools/gen-kima-web-css.mjs needs into an ISOLATED directory (never the repository):
# exact versions, so the generated crates/kima-web/assets/kima.css is reproducible locally and in CI.
#   tools/install-kima-web-css-deps.sh [dir]
# dir defaults to $KIMA_CSS_DEPS, then ${XDG_CACHE_HOME:-~/.cache}/kima-rs/tailwind. Prints the directory.
# The versions match the upstream kima-hub frontend lockfile at the pinned reference (tailwindcss 4.2.1,
# @tailwindcss/postcss 4.2.1, postcss 8.5.23); bump them here only together with a regenerated kima.css.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tools/tools-env.inc.sh
. "$here/tools-env.inc.sh"
dir="${1:-${KIMA_CSS_DEPS:-${XDG_CACHE_HOME:-$HOME/.cache}/kima-rs/tailwind}}"
mkdir -p "$dir"
cd "$dir"
[ -f package.json ] || echo '{"private":true}' >package.json
npm install --no-audit --no-fund --save-exact tailwindcss@4.2.1 @tailwindcss/postcss@4.2.1 postcss@8.5.23 >&2
pwd
