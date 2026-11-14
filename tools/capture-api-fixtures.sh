#!/usr/bin/env bash
# Capture M-SYNC, M-STATS and M6 podcast wire fixtures from THIS checkout's server and a fresh
# run-owned pgvector cluster. No caller database is accepted. Requires docker, ffmpeg, jq, curl,
# psql, python3, git and cargo.
# KIMA_CAPTURE_SET (required) names the fixture sets copied into the tree: a comma list of
# msync, mstats, m6. Every capture runs (later ones depend on earlier state), but only the named
# sets are copied, so a run for one milestone never rewrites another's fixtures with new ids.
set -euo pipefail
if [[ -n "${DATABASE_URL:-}" || -n "${PGDATABASE:-}" ]]; then
  echo 'No external DATABASE_URL or PGDATABASE allowed; this script owns a fresh database.' >&2; exit 2
fi
if [[ "${1:-}" == --help ]]; then
  echo 'KIMA_CAPTURE_SET=msync,mstats,m6 (any subset). No external DATABASE_URL or PGDATABASE allowed.'; exit 0
fi
[[ $# == 0 ]] || { echo 'No arguments accepted' >&2; exit 2; }
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
for cmd in docker ffmpeg jq curl psql python3 git cargo; do command -v "$cmd" >/dev/null || { echo "Missing $cmd" >&2; exit 1; }; done
msync_names=(me settings public_config library_removals playlist_create playlist_update playlists_list playlist_detail enrichment_rerun)
mstats_names=(stats_response_s1 stats_hour stats_tracks stats_track_plays stats_region stats_false_starts stats_collapse stats_remove stats_ignore stats_restore)
m6_names=(categories subscribe list list_ai_show detail episodes progress next next_end listening listening_ai_show
  charts discover_top discover_genre discover_genres for_you similar preview preview_followed preview_similar
  search search_discover recently_listened stats_spoken refresh refresh_all progress_clear ai_label_show
  ai_label_episode unsubscribe)
copy=()
IFS=',' read -r -a sets <<< "${KIMA_CAPTURE_SET:-}"
for set in "${sets[@]}"; do
  case "$set" in
    msync) for n in "${msync_names[@]}"; do copy+=("${n}_response.json"); done ;;
    mstats) for n in "${mstats_names[@]}"; do copy+=("${n}_response.json"); done ;;
    m6) for n in "${m6_names[@]}"; do copy+=("podcasts_${n}_response.json"); done
        copy+=(podcasts_capture_manifest.json) ;;
    *) echo "KIMA_CAPTURE_SET: unknown set '$set' (msync, mstats, m6)" >&2; exit 2 ;;
  esac
done
[[ ${#copy[@]} -gt 0 ]] || { echo 'Set KIMA_CAPTURE_SET (msync, mstats, m6; comma-separated)' >&2; exit 2; }
# Refuse before anything expensive when a fixture this run would overwrite has uncommitted
# changes or exists untracked: either would be lost. A file absent from disk is not flagged.
dirty=()
for f in "${copy[@]}"; do
  f="crates/kima-api-types/tests/fixtures/$f"
  if [[ -e "$f" ]] && { ! git diff --quiet HEAD -- "$f" 2>/dev/null ||
    [[ -n "$(git status --porcelain -- "$f")" ]]; }; then dirty+=("$f"); fi
done
if [[ ${#dirty[@]} -gt 0 ]]; then
  printf 'Uncommitted or untracked fixtures this run would overwrite:\n' >&2
  printf '  %s\n' "${dirty[@]}" >&2
  exit 1
fi
run="$(date +%s)-$$"
container_name="msync18-capture-${run}"
tmp="$(mktemp -d "${TMPDIR:-$HOME/.hermes/cache/scratch}/msync18-capture.XXXXXXXX")"
server_pid=''
mock_pid=''
cleanup() {
  # The server must release all database connections before its DB disappears.
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  if [[ -n "$mock_pid" ]]; then
    kill "$mock_pid" 2>/dev/null || true
    wait "$mock_pid" 2>/dev/null || true
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
# M6: loopback Apple/feed/audio stand-in (synthetic data). The server may reach exactly this
# socket despite its SSRF guard (KIMA_NET_ALLOW_LOOPBACK, accepted only with KIMA_NET_DENY_REAL=1).
python3 "$root/tools/podcast-capture-mock.py" > "$tmp/mock.port" 2> "$tmp/mock.log" & mock_pid=$!
mock_port=''
for _ in {1..50}; do
  mock_port="$(head -n1 "$tmp/mock.port" 2>/dev/null || true)"
  [[ "$mock_port" =~ ^[0-9]+$ ]] && break
  kill -0 "$mock_pid" 2>/dev/null || { cat "$tmp/mock.log" >&2; exit 1; }
  sleep 0.2
done
[[ "$mock_port" =~ ^[0-9]+$ ]] || { echo 'Podcast mock did not start' >&2; exit 1; }
mock="http://127.0.0.1:$mock_port"
export KIMA_ITUNES_BASE_URL="$mock" KIMA_NET_ALLOW_LOOPBACK="127.0.0.1:$mock_port"
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
# M-STATS: the ten /stats routes. Two plays through the API (kima-rs records them heard), then
# two heard=false rows by SQL (no kima-rs writer produces one, and only they can be a provable
# skip): "First" (900 s) left after 60 s for "Third" (6 s, too short to count).
id_of() { jq -er --arg t "$1" '.tracks[] | select(.title == $t) | .id' <<< "$tracks"; }
first="$(id_of First)"; second="$(id_of Second)"; third="$(id_of Third)"
request POST /plays "$(jq -nc --arg id "$first" '{trackId:$id,secondsListened:900}')" >/dev/null
request POST /plays "$(jq -nc --arg id "$second" '{trackId:$id,secondsListened:6}')" >/dev/null
user="$(request GET /auth/me '' | jq -er .id)"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -qX -v u="$user" -v a="$first" -v b="$third" <<'SQL'
INSERT INTO plays (id, user_id, track_id, played_at, source, heard) VALUES
  (gen_random_uuid(), :'u', :'a', now() - interval '1 day', 'LIBRARY', false),
  (gen_random_uuid(), :'u', :'b', now() - interval '1 day' + interval '60 seconds', 'LIBRARY', false);
SQL
capture stats_response_s1 GET '/stats?period=all&tz=UTC'
last="$(jq -er '.listening.lastPlay' "$stage/stats_response_s1_response.json")"
capture stats_hour GET "/stats/hour?dow=$(date -u -d "$last" +%w)&hour=$(date -u -d "$last" +%-H)&tz=UTC"
capture stats_tracks GET '/stats/tracks?of=top'
capture stats_track_plays GET "/stats/track/$first/plays"
capture stats_region GET '/stats/region?x=12&y=12'
capture stats_ignore POST /stats/false-starts/ignore "$(jq -nc --arg id "$second" '{trackIds:[$id]}')"
capture stats_false_starts GET /stats/false-starts
capture stats_collapse POST /stats/false-starts/collapse '{"trackIds":[]}'
capture stats_remove POST /stats/false-starts/remove "$(jq -nc --arg id "$first" '{trackIds:[$id]}')"
capture stats_restore POST /stats/false-starts/restore "$(jq -nc --arg id "$second" '{trackIds:[$id]}')"
# M6 podcasts (Task 19): real subscribe/refresh/preview flow against the loopback mock.
# shellcheck source=tools/capture-podcast-fixtures.inc.sh
source "$root/tools/capture-podcast-fixtures.inc.sh"
for f in "${copy[@]}"; do
  cp "$stage/$f" "crates/kima-api-types/tests/fixtures/$f"
done
echo "Copied ${#copy[@]} fixtures (sets: ${KIMA_CAPTURE_SET}); raw dynamic UUIDs/timestamps retained; DB, mock and server removed on exit." >&2
