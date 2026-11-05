#!/usr/bin/env bash
# Starts a local kima-web stack in one of two shapes:
#   dev (default / --watch): kima-server (API) + the vertigo dev server, which serves kima-web and
#       proxies /api/* to kima-server, so the browser sees ONE origin (http://127.0.0.1:$WEB_PORT).
#   --ssr: the production shape. `vertigo build`, then kima-server alone with the SSR server embedded
#       in-process (KIMA_WEB_BUILD_DIR): ONE origin, http://127.0.0.1:$KIMA_PORT/, pages server-rendered
#       by kima-server's wasmtime and proxied to a loopback-only port ($SSR_PORT). No vertigo dev server.
#
# Usage:  MUSIC_PATH=/path/to/music tools/run-kima-web-dev.sh [--watch | --ssr] [--no-build]
#   (default)   `vertigo build` once, then `vertigo serve` (release wasm, needs wasm-opt on PATH)
#   --watch     `vertigo watch` instead: debug wasm, rebuilds on source changes
#   --ssr       build, then run kima-server with the embedded SSR (see above); no WEB_PORT is used
#   --no-build  reuse crates/kima-web/build/ as it is (serve and --ssr modes)
#
# Environment:
#   MUSIC_PATH         REQUIRED. Directory kima-server scans. First-run flow: open the UI, create the admin
#                      account, press "Scan library" (it calls POST /api/library/scan on this directory).
#   DATABASE_URL       Postgres (pgvector) for kima-server. If unset, a dedicated Docker postgres is started
#                      or reused: container kima-web-dev-postgres, label kima.run=kima-web-dev, named volume
#                      kima-web-dev-pgdata, bound to 127.0.0.1:$KIMA_WEB_DEV_PG_PORT (default 55433).
#                      It never defaults to the dev DB on 5456.
#   KIMA_DATA_DIR      kima-server state (secrets, caches). Default: $HOME/.local/share/kima-web-dev
#   KIMA_PORT          kima-server port (default 3030)        WEB_PORT   dev server port (default 4444)
#   SSR_PORT           loopback port of the embedded SSR server in --ssr mode (default 3031)
#   VERTIGO            vertigo CLI (0.13.4, default: `vertigo` on PATH)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
web="$root/crates/kima-web"
vertigo="${VERTIGO:-vertigo}"
kima_port="${KIMA_PORT:-3030}"
web_port="${WEB_PORT:-4444}"
mode=serve
build=1
ssr_port="${SSR_PORT:-3031}"
for arg in "$@"; do
  case "$arg" in
    --watch) mode=watch ;;
    --ssr) mode=ssr ;;
    --no-build) build=0 ;;
    -h | --help) sed -n '2,29p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

: "${MUSIC_PATH:?MUSIC_PATH must point at the music directory to scan (e.g. MUSIC_PATH=\$HOME/Music)}"
[ -d "$MUSIC_PATH" ] || { echo "MUSIC_PATH is not a directory: $MUSIC_PATH" >&2; exit 1; }
export MUSIC_PATH
export KIMA_DATA_DIR="${KIMA_DATA_DIR:-$HOME/.local/share/kima-web-dev}"
mkdir -p "$KIMA_DATA_DIR"

# Waits until Postgres answers a real connection from the host (not just an open port).
wait_pg() {
  for _ in $(seq 1 60); do
    if psql "$1" -qtAc 'select 1' >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  echo "Postgres did not answer on the host within 60s: $1" >&2
  return 1
}

if [ -z "${DATABASE_URL:-}" ]; then
  pg_port="${KIMA_WEB_DEV_PG_PORT:-55433}"
  name=kima-web-dev-postgres
  if [ -z "$(docker ps -aq -f "name=^${name}$")" ]; then
    docker run -d --name "$name" --label kima.run=kima-web-dev \
      -e POSTGRES_USER=kima -e POSTGRES_PASSWORD=kima -e POSTGRES_DB=kima \
      -v kima-web-dev-pgdata:/var/lib/postgresql/data \
      -p "127.0.0.1:${pg_port}:5432" pgvector/pgvector:pg17 >/dev/null
    echo "started dedicated Docker postgres '$name' (label kima.run=kima-web-dev, volume kima-web-dev-pgdata, 127.0.0.1:${pg_port})"
  else
    docker start "$name" >/dev/null
    echo "reusing dedicated Docker postgres '$name' (volume kima-web-dev-pgdata)"
  fi
  export DATABASE_URL="postgres://kima:kima@127.0.0.1:${pg_port}/kima"
  echo "remove it with: docker rm -f $name && docker volume rm kima-web-dev-pgdata"
fi
wait_pg "$DATABASE_URL"

pids=()
cleanup() { for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT INT TERM

echo "building kima-server..."
(cd "$root" && SQLX_OFFLINE=true cargo build -p kima-server)

if [ "$mode" = ssr ]; then
  if [ "$build" = 1 ]; then (cd "$web" && "$vertigo" build); fi
  [ -f "$web/build/index.json" ] || { echo "no vertigo build output in $web/build (run without --no-build)" >&2; exit 1; }
  # KIMA_WEB_BUILD_DIR turns the embedded SSR on; kima exits non-zero if that server cannot start or dies.
  (
    cd "$root"
    PORT="$kima_port" KIMA_BIND_HOST=127.0.0.1 KIMA_IDENTIFY="${KIMA_IDENTIFY:-off}" \
      KIMA_WEB_BUILD_DIR=crates/kima-web/build KIMA_WEB_SSR_PORT="$ssr_port" \
      exec target/debug/kima
  ) &
  kima_pid=$!
  pids+=("$kima_pid")
  # The first start compiles the wasm with wasmtime before the port opens: allow it time.
  for _ in $(seq 1 180); do
    curl -fsS "http://127.0.0.1:${kima_port}/api/health" >/dev/null 2>&1 && break
    kill -0 "$kima_pid" 2>/dev/null || { wait "$kima_pid" || true; echo "kima-server exited during start" >&2; exit 1; }
    sleep 1
  done
  curl -fsS "http://127.0.0.1:${kima_port}/api/health" >/dev/null || { echo "kima-server did not come up on :$kima_port" >&2; exit 1; }
  echo
  echo "kima-web (embedded SSR): http://127.0.0.1:${kima_port}/   (SSR server on loopback :${ssr_port})"
  echo "music dir: $MUSIC_PATH"
  # Stay in the foreground for as long as kima-server runs; its exit status is the script's.
  wait "$kima_pid"
  exit $?
fi

# KIMA_IDENTIFY=off: no outbound MusicBrainz/AcoustID lookups in dev.
(
  cd "$root"
  PORT="$kima_port" KIMA_BIND_HOST=127.0.0.1 KIMA_IDENTIFY="${KIMA_IDENTIFY:-off}" \
    exec target/debug/kima
) &
pids+=($!)
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:${kima_port}/api/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS "http://127.0.0.1:${kima_port}/api/health" >/dev/null || { echo "kima-server did not come up on :$kima_port" >&2; exit 1; }

proxy="--proxy /api=http://127.0.0.1:${kima_port}/api"
cd "$web"
echo
echo "kima-web: http://127.0.0.1:${web_port}   (API proxied to http://127.0.0.1:${kima_port}/api)"
echo "music dir: $MUSIC_PATH"
if [ "$mode" = watch ]; then
  # shellcheck disable=SC2086
  "$vertigo" watch --port "$web_port" $proxy
else
  [ "$build" = 1 ] && "$vertigo" build
  # shellcheck disable=SC2086
  "$vertigo" serve --port "$web_port" $proxy
fi
