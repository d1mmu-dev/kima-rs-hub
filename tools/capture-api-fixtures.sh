#!/usr/bin/env bash
# Capture M-SYNC wire fixtures from THIS checkout's server and a fresh run-owned pgvector cluster.
# No caller database is accepted. Requires docker, ffmpeg, jq, curl, psql and cargo.
set -euo pipefail
if [[ -n "${DATABASE_URL:-}" || -n "${PGDATABASE:-}" ]]; then
  echo 'No external DATABASE_URL or PGDATABASE allowed; this script owns a fresh database.' >&2; exit 2
fi
if [[ "${1:-}" == --help ]]; then
  echo 'No external DATABASE_URL or PGDATABASE allowed. Run from a checkout with Docker and ffmpeg.'; exit 0
fi
[[ $# == 0 ]] || { echo 'No arguments accepted' >&2; exit 2; }
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
for cmd in docker ffmpeg jq curl psql cargo; do command -v "$cmd" >/dev/null || { echo "Missing $cmd" >&2; exit 1; }; done
run="$(date +%s)-$$"
container_name="msync18-capture-${run}"
tmp="$(mktemp -d "${TMPDIR:-$HOME/.hermes/cache/scratch}/msync18-capture.XXXXXXXX")"
server_pid=''
cleanup() {
  # The server must release all database connections before its DB disappears.
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  docker rm -f -v "$container_name" >/dev/null 2>&1 || true
  rm -rf "$tmp"
}
trap cleanup EXIT
export KIMA_NET_DENY_REAL=1 SQLX_OFFLINE=true
export PGDATABASE=kima
export DATABASE_URL=''
docker run -d --name "$container_name" --label "kima.run=$container_name" -e POSTGRES_USER=kima -e POSTGRES_PASSWORD=kima -e POSTGRES_DB=kima -p 127.0.0.1::5432 pgvector/pgvector:pg17 >/dev/null
port="$(docker inspect -f '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container_name")"
[[ "$port" =~ ^[0-9]+$ && "$port" != 5456 && "$port" != 55433 && "$port" != 52064 ]] || { echo 'Unsafe database port' >&2; exit 1; }
export DATABASE_URL="postgres://kima:kima@127.0.0.1:${port}/kima"
for _ in {1..90}; do
  if psql "$DATABASE_URL" -Atqc 'select current_database(),current_user' 2>/dev/null > "$tmp/identity"; then break; fi
  sleep 1
done
[[ "$(cat "$tmp/identity")" == 'kima|kima' ]] || { echo 'Isolated PostgreSQL identity check failed' >&2; exit 1; }
[[ "$(docker inspect -f '{{index .Config.Labels "kima.run"}}' "$container_name")" == "$container_name" ]] || exit 1
echo "capture provenance: HEAD=$(git rev-parse HEAD), container=$container_name, host-port=$port, db=kima, role=kima" >&2
"$root/tools/make-fixture-library.sh" "$tmp/music"
export MUSIC_PATH="$tmp/music" KIMA_DATA_DIR="$tmp/data" KIMA_IDENTIFY=off KIMA_BIND_HOST=127.0.0.1
export KIMA_DISABLE_CLAP=1
mkdir -p "$KIMA_DATA_DIR"
# Build from the checked-out sources; never run a stale target/debug/kima.
cargo build -p kima-server > "$tmp/build.log" 2>&1 || { cat "$tmp/build.log" >&2; exit 1; }
# Choose an OS-assigned free port; only the run-owned server may answer health.
export PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
./target/debug/kima > "$tmp/server.log" 2>&1 & server_pid=$!
base="http://127.0.0.1:$PORT/api"
for _ in {1..90}; do
  if curl -fsS "$base/health" > /dev/null 2>&1; then break; fi
  kill -0 "$server_pid" 2>/dev/null || { cat "$tmp/server.log" >&2; exit 1; }
  sleep 1
done
curl -fsS "$base/health" >/dev/null
request() { local method="$1" path="$2" payload="${3:-}"; shift 3 || true
  if [[ "$method" == GET ]]; then curl --fail-with-body -sS -H "Authorization: Bearer $token" "$base$path"
  else curl --fail-with-body -sS -X "$method" -H "Authorization: Bearer $token" -H 'Content-Type: application/json' -d "$payload" "$base$path"; fi
}
register="$(curl --fail-with-body -sS -X POST -H 'Content-Type: application/json' -d '{"username":"fixtureadmin","password":"fixture-pass-18"}' "$base/onboarding/register")"
# Sign in independently rather than using the registration token.
token="$(curl --fail-with-body -sS -X POST -H 'Content-Type: application/json' -d '{"username":"fixtureadmin","password":"fixture-pass-18"}' "$base/auth/login" | jq -er '.token')"
request POST /onboarding/complete '{}' >/dev/null
stage="$tmp/fixtures"; mkdir -p "$stage"
capture() { local name="$1" method="$2" path="$3" payload="${4:-}"; request "$method" "$path" "$payload" | jq -eS . > "$stage/${name}_response.json"; echo "$name $method $path" >&2; }
capture me GET /auth/me
capture settings GET /settings
capture public_config GET /system-settings/public-config
capture library_removals GET /library/removals
scan="$(request POST /library/scan '{}')"; job="$(jq -er '.jobId' <<< "$scan")"
for _ in {1..90}; do
  status="$(request GET "/library/scan/status/$job" | jq -r '.status')"
  [[ "$status" != failed ]] || { echo 'Library scan failed' >&2; exit 1; }
  [[ "$status" != completed ]] || break
  sleep 1
done
[[ "$status" == completed ]] || { echo 'Library scan timed out' >&2; exit 1; }
tracks="$(request GET '/library/tracks?limit=100')"
ids="$(jq -c '[.tracks[] | select(.title == "First" or .title == "Second") | .id] | sort' <<< "$tracks")"
[[ "$(jq length <<< "$ids")" == 2 ]] || { echo 'Expected two fixture tracks' >&2; exit 1; }
capture playlist_create POST /playlists '{"name":"Road trip","isPublic":false}'
id="$(jq -er .id "$stage/playlist_create_response.json")"
request POST "/playlists/$id/items" "$(jq -nc --argjson ids "$ids" '{trackIds:$ids}')" >/dev/null
capture playlist_update PUT "/playlists/$id" '{"reverseOrder":true}'
capture playlists_list GET /playlists
capture playlist_detail GET "/playlists/$id"
# Valid rerun on the fresh cluster; no external provider traffic (KIMA_IDENTIFY=off).
capture enrichment_rerun POST /enrichment/rerun '{"step":"identification"}'
for name in me settings public_config library_removals playlist_create playlist_update playlists_list playlist_detail enrichment_rerun; do
  cp "$stage/${name}_response.json" "crates/kima-api-types/tests/fixtures/${name}_response.json"
done
echo 'Capture complete (raw dynamic UUIDs/timestamps retained); DB and server removed on exit.' >&2
