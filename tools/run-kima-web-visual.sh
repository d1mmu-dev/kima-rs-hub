#!/usr/bin/env bash
# Isolated stack for the visual comparison of kima-web against the upstream kima-hub Next.js frontend: ONE backend
# (kima-server + its own throw-away Docker postgres), ONE seeded library, and both frontends in front of it.
#
#   tools/run-kima-web-visual.sh up       start everything that is not running (idempotent), print the env
#   tools/run-kima-web-visual.sh reload   rebuild kima-web (vertigo build) and restart ONLY kima-server
#   tools/run-kima-web-visual.sh compare [args]   css --check, `up`, `reload`, run tools/compare-kima-web-visual.mjs, leave the stack up
#   tools/run-kima-web-visual.sh run [args]       `compare`, then `down`
#   tools/run-kima-web-visual.sh down     stop kima/next and remove the postgres container + state
#
# The reference is an EXTERNAL kima-hub checkout (this repository no longer bundles the Next.js app, D-180).
# `up` runs tools/prepare-kima-hub-reference.sh --install first: clean-tree check, `git pull --ff-only`, fetched SHA
# recorded, `frontend/` extracted into an isolated copy whose dependencies are installed there (never in the
# checkout). Next.js runs ONLY here, as the visual reference; it is not part of any product, dev or CI path.
# A pull that moves the reference restarts the reference dev server; compare writes the SHA into its report.
# Needs: cargo, node/npm (reference copy only), docker (pgvector/pgvector:pg17 on a random 127.0.0.1 port),
# ffmpeg, vertigo 0.13.4 (+ wasm-opt), Chrome.
# Environment, or the private tools config (tools/tools-env.inc.sh): KIMA_HUB_REFERENCE (required),
# KIMA_REFERENCE_COPY, VISUAL_DIR (default ${XDG_CACHE_HOME:-~/.cache}/kima-rs/visual), VERTIGO (default
# `vertigo` on PATH), FFMPEG (default `ffmpeg` on PATH), KIMA_TOOLS_PATH (prepended to PATH), CHROME.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=tools/tools-env.inc.sh
. "$root/tools/tools-env.inc.sh"
dir="${VISUAL_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/kima-rs/visual}"
vertigo="${VERTIGO:-vertigo}"
ffmpeg="${FFMPEG:-ffmpeg}"
name="kima-visual-pg-$$"
state="$dir/state.env"
mkdir -p "$dir"

free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
alive() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null; }
load() { [ -f "$state" ] && . "$state" || true; }
save() {
  cat >"$state" <<EOF
PG_NAME=$PG_NAME
PG_PORT=$PG_PORT
KIMA_PORT=$KIMA_PORT
SSR_PORT=$SSR_PORT
LEGACY_PORT=$LEGACY_PORT
KIMA_PID=${KIMA_PID:-}
LEGACY_PID=${LEGACY_PID:-}
REF_SHA=${REF_SHA:-}
REF_FRONTEND=${REF_FRONTEND:-}
EOF
}

start_kima() {
  KIMA_PID=""
  (cd "$root" && SQLX_OFFLINE=true cargo build -p kima-server)
  (
    cd "$root"
    DATABASE_URL="postgres://kima:kima@127.0.0.1:${PG_PORT}/kima" MUSIC_PATH="$dir/library" \
      KIMA_DATA_DIR="$dir/data" PORT="$KIMA_PORT" KIMA_BIND_HOST=127.0.0.1 KIMA_IDENTIFY=off \
      KIMA_WEB_BUILD_DIR=crates/kima-web/build KIMA_WEB_SSR_PORT="$SSR_PORT" \
      exec target/debug/kima >"$dir/kima.log" 2>&1
  ) &
  KIMA_PID=$!
  for _ in $(seq 1 240); do
    curl -fsS "http://127.0.0.1:${KIMA_PORT}/api/health" >/dev/null 2>&1 && return 0
    alive "$KIMA_PID" || { echo "kima exited during start, see $dir/kima.log" >&2; return 1; }
    sleep 1
  done
  echo "kima did not come up" >&2; return 1
}

stop_legacy() {
  if alive "${LEGACY_PID:-}"; then pkill -P "$LEGACY_PID" 2>/dev/null || true; kill "$LEGACY_PID" 2>/dev/null || true; fi
  # next dev leaves a child behind on its port.
  [ -n "${LEGACY_PORT:-}" ] && (fuser -k "${LEGACY_PORT}/tcp" >/dev/null 2>&1 || true)
  LEGACY_PID=""
}

stop_kima() {
  if alive "${KIMA_PID:-}"; then kill "$KIMA_PID" 2>/dev/null || true; for _ in $(seq 1 30); do alive "$KIMA_PID" || break; sleep 0.5; done; fi
  KIMA_PID=""
}

up() {
  load
  if [ -z "${PG_NAME:-}" ] || ! docker ps -q -f "name=^${PG_NAME}$" | grep -q .; then
    PG_NAME="$name"; PG_PORT="$(free_port)"
    docker run -d --name "$PG_NAME" --label kima.run=kima-web-visual \
      -e POSTGRES_USER=kima -e POSTGRES_PASSWORD=kima -e POSTGRES_DB=kima \
      -p "127.0.0.1:${PG_PORT}:5432" pgvector/pgvector:pg17 >/dev/null
    KIMA_PORT="$(free_port)"; SSR_PORT="$(free_port)"; LEGACY_PORT="$(free_port)"
    KIMA_PID=""; LEGACY_PID=""
    save
  fi
  for _ in $(seq 1 90); do
    docker exec "$PG_NAME" pg_isready -U kima -d kima -h 127.0.0.1 >/dev/null 2>&1 && break
    sleep 1
  done
  [ -d "$dir/library" ] || FFMPEG="$ffmpeg" "$root/tools/make-visual-library.sh" "$dir/library"
  [ -f "$root/crates/kima-web/build/index.json" ] || (cd "$root/crates/kima-web" && "$vertigo" build)
  if ! alive "${KIMA_PID:-}"; then
    start_kima; save
    seed
  fi
  # Fresh reference before every comparison: check the external checkout, pull, record the SHA, isolated copy.
  local prov sha
  prov="$("$root/tools/prepare-kima-hub-reference.sh" --install)"
  sha="$(sed -n 's/^KIMA_HUB_REFERENCE_SHA=//p' <<<"$prov")"
  REF_FRONTEND="$(sed -n 's/^KIMA_REFERENCE_FRONTEND=//p' <<<"$prov")"
  if alive "${LEGACY_PID:-}" && [ "$sha" != "${REF_SHA:-}" ]; then
    echo "reference moved to $sha: restarting the reference dev server" >&2
    stop_legacy
  fi
  REF_SHA="$sha"; save
  if ! alive "${LEGACY_PID:-}"; then
    (
      cd "$REF_FRONTEND"
      NEXT_PUBLIC_BACKEND_URL="http://127.0.0.1:${KIMA_PORT}" KIMA_DEV_ORIGINS="127.0.0.1:${LEGACY_PORT}" \
        NEXT_TELEMETRY_DISABLED=1 exec npx next dev -p "$LEGACY_PORT" -H 127.0.0.1 >"$dir/legacy.log" 2>&1
    ) &
    LEGACY_PID=$!; save
    for _ in $(seq 1 120); do
      curl -fsS "http://127.0.0.1:${LEGACY_PORT}/login" >/dev/null 2>&1 && break
      alive "$LEGACY_PID" || { echo "legacy dev server exited, see $dir/legacy.log" >&2; exit 1; }
      sleep 1
    done
  fi
  env_out
}

# Admin account + scan, once. The password is a throw-away of this isolated stack.
seed() {
  local base="http://127.0.0.1:${KIMA_PORT}"
  if [ "$(curl -fsS "$base/api/onboarding/status" | python3 -c 'import json,sys;print(json.load(sys.stdin).get("hasAccount"))')" != "True" ]; then
    local tok
    tok="$(curl -fsS -X POST "$base/api/onboarding/register" -H 'content-type: application/json' \
      -d '{"username":"admin","password":"visual-pass-1"}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])')"
    curl -fsS -X POST "$base/api/onboarding/complete" -H "authorization: Bearer $tok" >/dev/null
    local job
    job="$(curl -fsS -X POST "$base/api/library/scan" -H "authorization: Bearer $tok" | python3 -c 'import json,sys;print(json.load(sys.stdin)["jobId"])')"
    for _ in $(seq 1 120); do
      st="$(curl -fsS "$base/api/library/scan/status/$job" -H "authorization: Bearer $tok" | python3 -c 'import json,sys;print(json.load(sys.stdin)["status"])')"
      [ "$st" = completed ] && break
      sleep 1
    done
    [ "$st" = completed ] || { echo "scan did not complete ($st)" >&2; exit 1; }
    # A playlist so the playlists pages have content.
    local pl
    pl="$(curl -fsS -X POST "$base/api/playlists" -H "authorization: Bearer $tok" -H 'content-type: application/json' \
      -d '{"name":"Late Night Drive"}' | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d.get("id") or d.get("playlist",{}).get("id"))')"
    curl -fsS "$base/api/library/tracks?limit=5" -H "authorization: Bearer $tok" | python3 -c '
import json,sys
d=json.load(sys.stdin)
for t in d.get("tracks",[]): print(t["id"])' | while read -r tid; do
      curl -fsS -X POST "$base/api/playlists/$pl/items" -H "authorization: Bearer $tok" -H 'content-type: application/json' \
        -d "{\"trackId\":\"$tid\"}" >/dev/null
    done
    curl -fsS -X POST "$base/api/playlists" -H "authorization: Bearer $tok" -H 'content-type: application/json' \
      -d '{"name":"Empty Shelf"}' >/dev/null
  fi
}

env_out() {
  load
  cat <<EOF
export WEB_URL=http://127.0.0.1:${KIMA_PORT}
export LEGACY_URL=http://127.0.0.1:${LEGACY_PORT}
export ADMIN_USER=admin ADMIN_PASS=visual-pass-1
export KIMA_HUB_REFERENCE_SHA=${REF_SHA:-}
EOF
}

reload() {
  load
  (cd "$root/crates/kima-web" && "$vertigo" build)
  stop_kima
  start_kima; save
}

down() {
  load
  stop_kima
  stop_legacy
  [ -n "${PG_NAME:-}" ] && docker rm -f -v "$PG_NAME" >/dev/null 2>&1 || true
  rm -f "$state"
  echo "stopped"
}

cmd="${1:-up}"; shift || true
case "$cmd" in
  up) up ;;
  reload) reload ;;
  compare | run)
    # A stale checked-in Tailwind sheet made kima-web render without any utility class once; refuse to compare then.
    node "$root/tools/gen-kima-web-css.mjs" --check || { echo "run: node tools/gen-kima-web-css.mjs" >&2; exit 1; }
    up >/dev/null
    reload >/dev/null   # always a real `vertigo build` + server rebuild/restart, never a leftover build directory
    load
    set +e
    WEB_URL="http://127.0.0.1:${KIMA_PORT}" LEGACY_URL="http://127.0.0.1:${LEGACY_PORT}" \
      ADMIN_USER=admin ADMIN_PASS=visual-pass-1 KIMA_HUB_REFERENCE_SHA="${REF_SHA:-}" node "$root/tools/compare-kima-web-visual.mjs" "$@"
    rc=$?
    set -e
    [ "$cmd" = run ] && down
    exit $rc
    ;;
  down) down ;;
  *) sed -n 2,13p "$0"; exit 2 ;;
esac
