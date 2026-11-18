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
# Steps, in order, each refusing (non-zero exit, nothing reset, nothing copied) when it does not hold:
#   1. the checkout is a clean git work tree (local changes are preserved; the script stops instead);
#   2. HEAD is on branch `main` whose upstream is `main` of a remote whose configured AND effective fetch URLs (after
#      any `insteadOf` rewrite) are the canonical upstream repository Chevron7Locked/kima-hub, not a fork;
#   3. `git pull --ff-only` (a failure or conflict stops the script);
#   4. HEAD equals its upstream exactly (a local commit ahead of upstream is not a reference).
# The recorded provenance is the HEAD SHA, describe, remote name + URL, branch, upstream, whether it was pulled, the
# relation to the pin below and the git tree id of `frontend/`. Inherited repository variables (GIT_DIR,
# GIT_WORK_TREE, GIT_CONFIG_PARAMETERS, ...) are dropped so they cannot redirect these git calls.
#
# The copy: `git archive <sha> frontend` is ALWAYS extracted afresh into a temporary directory and its content
# manifest (type, mode and path of every entry, SHA-256 of every file, symlink targets; the copy-local top-level
# `node_modules/` and `.next/` excluded) is compared with the existing copy's. An identical copy is kept as it is
# (node_modules and .next included); any difference (another SHA, a file edited, added or removed in the copy)
# replaces the copy with the fresh extraction, keeping only its node_modules (.next, a build cache of the old
# content, is dropped). Nothing is ever written into the checkout. KIMA_REFERENCE_COPY_RESTORED reports `new`, `no`
# (verified identical) or `yes` (replaced); a caller serving the copy restarts its server on `yes` and whenever the
# SHA, the manifest or the copy path changes (tools/run-kima-web-visual.sh does).
#
# Environment, or KEY=VALUE lines in the private config file (default ${XDG_CONFIG_HOME:-~/.config}/kima-rs/tools.env,
# override with KIMA_RS_TOOLS_ENV; it is never part of the repository):
#   KIMA_HUB_REFERENCE   path of the external kima-hub git checkout (required)
#   KIMA_REFERENCE_COPY  isolated copy (default ${XDG_CACHE_HOME:-~/.cache}/kima-rs/kima-hub-reference)
# Output (stdout, sourceable): the KIMA_HUB_REFERENCE_* provenance; with a copy also KIMA_REFERENCE_MANIFEST,
# KIMA_REFERENCE_COPY_RESTORED and KIMA_REFERENCE_FRONTEND (the copy's frontend/ directory). The copy carries the
# same lines in REFERENCE.env.
set -euo pipefail

# The upstream commit the ported backend, model hashes and compatibility fixtures are pinned to. The reference UI
# follows the latest upstream; this pin is NOT moved by a pull. This script reports only the commit distance
# (KIMA_HUB_REFERENCE_RELATION); docs/DIVERGENCES.md D-180 lists what else checks drift.
PIN=c7527577d925b20f5adf410c7fd4e0c8ac250f69
CANONICAL_BRANCH=main
canonical_url() {
  case "$1" in
    https://github.com/Chevron7Locked/kima-hub | https://github.com/Chevron7Locked/kima-hub.git) return 0 ;;
    git@github.com:Chevron7Locked/kima-hub | git@github.com:Chevron7Locked/kima-hub.git) return 0 ;;
    ssh://git@github.com/Chevron7Locked/kima-hub | ssh://git@github.com/Chevron7Locked/kima-hub.git) return 0 ;;
  esac
  return 1
}

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tools/tools-env.inc.sh
. "$here/tools-env.inc.sh"
# shellcheck disable=SC2046
unset $(git rev-parse --local-env-vars)

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

refuse() { echo "not the canonical upstream reference: $*; nothing pulled or copied" >&2; exit 5; }
branch="$(git -C "$ref" symbolic-ref --short -q HEAD)" || refuse "HEAD is detached"
[ "$branch" = "$CANONICAL_BRANCH" ] || refuse "checked-out branch is '$branch', not '$CANONICAL_BRANCH'"
remote="$(git -C "$ref" config --get "branch.$branch.remote")" || refuse "branch '$branch' has no upstream"
[ "$(git -C "$ref" config --get "branch.$branch.merge" || true)" = "refs/heads/$CANONICAL_BRANCH" ] \
  || refuse "branch '$branch' does not track '$CANONICAL_BRANCH'"
configured="$(git -C "$ref" config --get-all "remote.$remote.url" || true)"
effective="$(git -C "$ref" remote get-url --all "$remote" 2>/dev/null || true)"
{ [ -n "$configured" ] && [ -n "$effective" ]; } || refuse "remote '$remote' has no URL"
while IFS= read -r url; do
  canonical_url "$url" || refuse "remote '$remote' fetches from a non-canonical URL (a fork or an insteadOf rewrite)"
done <<<"$configured"$'\n'"$effective"
remote_url="$(head -1 <<<"$effective")"

pulled=no
if [ "$pull" = 1 ]; then
  git -C "$ref" pull --ff-only >&2 || { echo "git pull --ff-only failed in the reference checkout; stopping (no reset)" >&2; exit 4; }
  pulled=yes
fi

sha="$(git -C "$ref" rev-parse HEAD)"
upstream_sha="$(git -C "$ref" rev-parse '@{upstream}' 2>/dev/null)" || refuse "the upstream of '$branch' is not fetched"
upstream="$(git -C "$ref" rev-parse --abbrev-ref '@{upstream}')"
[ "$sha" = "$upstream_sha" ] || refuse "HEAD $sha is not its upstream $upstream ($upstream_sha): local commits or an incomplete pull"
tree="$(git -C "$ref" rev-parse "$sha:frontend")"
describe="$(git -C "$ref" describe --tags --always 2>/dev/null || echo "$sha")"
if git -C "$ref" merge-base --is-ancestor "$PIN" "$sha" 2>/dev/null; then
  relation="pin+$(git -C "$ref" rev-list --count "$PIN..$sha")"
else
  relation="not-descendant-of-pin"
fi
provenance="KIMA_HUB_REFERENCE_SHA=$sha
KIMA_HUB_REFERENCE_DESCRIBE=$describe
KIMA_HUB_REFERENCE_REMOTE=$remote
KIMA_HUB_REFERENCE_REMOTE_URL=$remote_url
KIMA_HUB_REFERENCE_BRANCH=$branch
KIMA_HUB_REFERENCE_UPSTREAM=$upstream
KIMA_HUB_REFERENCE_PULLED=$pulled
KIMA_HUB_REFERENCE_PIN=$PIN
KIMA_HUB_REFERENCE_RELATION=$relation
KIMA_HUB_REFERENCE_TREE=$tree
KIMA_HUB_REFERENCE_CHECKED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "reference $describe ($sha) from $remote_url $upstream, $relation, pulled=$pulled" >&2
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

# Content manifest of a frontend/ tree (see the header); the top-level node_modules/ and .next/ are excluded.
manifest() {
  (
    cd "$1"
    find . \( -path ./node_modules -o -path ./.next \) -prune -o -printf '%y %m %p\n' | LC_ALL=C sort
    find . \( -path ./node_modules -o -path ./.next \) -prune -o -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
    find . \( -path ./node_modules -o -path ./.next \) -prune -o -type l -printf '%p -> %l\n' | LC_ALL=C sort
  ) | sha256sum | cut -d' ' -f1
}

tmp="$dest.tmp.$$"
trap 'rm -rf -- "$tmp"' EXIT
rm -rf -- "$tmp"; mkdir -p "$tmp"
git -C "$ref" archive --format=tar "$sha" frontend | tar -x -C "$tmp"
fresh="$(manifest "$tmp/frontend")"
if [ ! -d "$dest/frontend" ]; then
  restored=new
elif [ "$(manifest "$dest/frontend")" = "$fresh" ]; then
  restored=no
else
  restored=yes
fi
if [ "$restored" != no ]; then
  if [ -d "$dest/frontend/node_modules" ] && [ ! -L "$dest/frontend/node_modules" ]; then
    mv -- "$dest/frontend/node_modules" "$tmp/frontend/node_modules"
  fi
  rm -rf -- "$dest"; mv -- "$tmp" "$dest"
fi
[ "$(manifest "$dest/frontend")" = "$fresh" ] || { echo "the reference copy does not match $sha:frontend after extraction" >&2; exit 6; }
[ "$restored" = yes ] && echo "the reference copy differed from $sha:frontend and was re-extracted (node_modules kept)" >&2
provenance="$provenance
KIMA_REFERENCE_MANIFEST=$fresh
KIMA_REFERENCE_COPY_RESTORED=$restored"
printf '%s\n' "$provenance" >"$dest/REFERENCE.env"
if [ "$install" = 1 ]; then
  # Reinstall whenever the lockfile differs from the one node_modules was installed from.
  lock="$(sha256sum "$dest/frontend/package-lock.json" | cut -d' ' -f1)"
  if [ "$(cat "$dest/frontend/node_modules/.kima-reference-lock.sha256" 2>/dev/null)" != "$lock" ]; then
    (cd "$dest/frontend" && npm ci --no-audit --no-fund >&2)
    printf '%s\n' "$lock" >"$dest/frontend/node_modules/.kima-reference-lock.sha256"
  fi
fi
echo "$provenance"
echo "KIMA_REFERENCE_FRONTEND=$dest/frontend"
