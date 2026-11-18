#!/usr/bin/env bash
# Prepares the EXTERNAL upstream kima-hub checkout as the visual/functional reference for kima-web comparisons.
# The repository no longer bundles the Next.js app (owner decision 2026-11-18, docs/DIVERGENCES.md D-180): every
# comparison reads an external, freshly pulled kima-hub checkout and builds only an isolated copy of it.
#
#   tools/prepare-kima-hub-reference.sh            check + pull the checkout, refresh the isolated copy
#   tools/prepare-kima-hub-reference.sh --install  ... and `npm ci` inside the COPY (never in the checkout)
#   tools/prepare-kima-hub-reference.sh --check    check + pull and print the provenance only (no copy)
#   tools/prepare-kima-hub-reference.sh --no-pull  skip `git pull --ff-only` (offline; recorded as such)
#
# Steps, in order: the checkout must be a clean git work tree (local changes are preserved, never reset: the
# script stops instead); `git pull --ff-only` (a failure or conflict stops the script); the fetched HEAD SHA is
# recorded together with its relation to the pin below; `git archive HEAD frontend` is extracted into the copy
# directory, so nothing (node_modules, .next, build output) is ever written into the checkout.
#
# Environment, or KEY=VALUE lines in the private config file (default ${XDG_CONFIG_HOME:-~/.config}/kima-rs/tools.env,
# override with KIMA_RS_TOOLS_ENV; it is never part of the repository):
#   KIMA_HUB_REFERENCE   path of the external kima-hub git checkout (required)
#   KIMA_REFERENCE_COPY  isolated copy (default ${XDG_CACHE_HOME:-~/.cache}/kima-rs/kima-hub-reference)
# Output (stdout, sourceable): KIMA_HUB_REFERENCE_SHA, KIMA_REFERENCE_FRONTEND (the copy's frontend/ directory).
# The copy also carries REFERENCE.env with the same provenance for comparison evidence.
set -euo pipefail

# The upstream commit the ported backend, model hashes and compatibility fixtures are pinned to. The reference UI
# follows the latest upstream; this pin is NOT moved by a pull, drift is only reported (see docs/DIVERGENCES.md).
PIN=c7527577d925b20f5adf410c7fd4e0c8ac250f69

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tools/tools-env.inc.sh
. "$here/tools-env.inc.sh"

install=0 pull=1 copy=1
for arg in "$@"; do
  case "$arg" in
    --install) install=1 ;;
    --no-pull) pull=0 ;;
    --check) copy=0 ;;
    *) echo "unknown argument: $arg" >&2; exit 64 ;;
  esac
done

ref="${KIMA_HUB_REFERENCE:-}"
[ -n "$ref" ] || { echo "KIMA_HUB_REFERENCE is not set (environment or $KIMA_RS_TOOLS_ENV)" >&2; exit 2; }
git -C "$ref" rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo "not a git work tree: KIMA_HUB_REFERENCE" >&2; exit 2; }
[ -f "$ref/frontend/package.json" ] || { echo "KIMA_HUB_REFERENCE has no frontend/package.json" >&2; exit 2; }

dirty="$(git -C "$ref" status --porcelain)"
if [ -n "$dirty" ]; then
  echo "the reference checkout has local changes; they are preserved and nothing was pulled:" >&2
  echo "$dirty" >&2
  exit 3
fi
pulled=no
if [ "$pull" = 1 ]; then
  git -C "$ref" pull --ff-only >&2 || { echo "git pull --ff-only failed in the reference checkout; stopping (no reset)" >&2; exit 4; }
  pulled=yes
fi

sha="$(git -C "$ref" rev-parse HEAD)"
describe="$(git -C "$ref" describe --tags --always 2>/dev/null || echo "$sha")"
if git -C "$ref" merge-base --is-ancestor "$PIN" "$sha" 2>/dev/null; then
  relation="pin+$(git -C "$ref" rev-list --count "$PIN..$sha")"
else
  relation="not-descendant-of-pin"
fi
provenance="KIMA_HUB_REFERENCE_SHA=$sha
KIMA_HUB_REFERENCE_DESCRIBE=$describe
KIMA_HUB_REFERENCE_PIN=$PIN
KIMA_HUB_REFERENCE_RELATION=$relation
KIMA_HUB_REFERENCE_PULLED=$pulled
KIMA_HUB_REFERENCE_CHECKED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "reference $describe ($sha), $relation, pulled=$pulled" >&2
if [ "$copy" = 0 ]; then echo "$provenance"; exit 0; fi

dest="${KIMA_REFERENCE_COPY:-${XDG_CACHE_HOME:-$HOME/.cache}/kima-rs/kima-hub-reference}"
dest="$(realpath -m -- "$dest")"
case "$dest/" in
  "$(realpath -- "$ref")/"*) echo "KIMA_REFERENCE_COPY must not be inside the reference checkout" >&2; exit 2 ;;
esac
# Only a directory this script made (it has REFERENCE.env) or an absent/empty one is ever replaced.
if [ -e "$dest" ] && [ ! -f "$dest/REFERENCE.env" ] && [ -n "$(ls -A -- "$dest")" ]; then
  echo "KIMA_REFERENCE_COPY exists and is not a reference copy; refusing to replace it" >&2; exit 2
fi
if [ "$(sed -n 's/^KIMA_HUB_REFERENCE_SHA=//p' "$dest/REFERENCE.env" 2>/dev/null)" != "$sha" ]; then
  tmp="$dest.tmp.$$"
  rm -rf -- "$tmp"; mkdir -p "$tmp"
  git -C "$ref" archive --format=tar HEAD frontend | tar -x -C "$tmp"
  rm -rf -- "$dest"; mv -- "$tmp" "$dest"
fi
printf '%s\n' "$provenance" >"$dest/REFERENCE.env"
if [ "$install" = 1 ] && [ ! -f "$dest/frontend/node_modules/.package-lock.json" ]; then
  (cd "$dest/frontend" && npm ci --no-audit --no-fund >&2)
fi
echo "$provenance"
echo "KIMA_REFERENCE_FRONTEND=$dest/frontend"
