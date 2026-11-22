# shellcheck shell=bash
# M5b captures (plan 2026-11-19-m5b-mixes-radio.md, Task 19). Sourced by tools/capture-api-fixtures.sh when
# KIMA_CAPTURE_SET=m5b (alone). Expects: base (API root of the run's kima server), token, stage (staging dir),
# server_pid, harness_pid (empty; set here so the caller's cleanup stops it), tmp, root, request() from the caller.
#
# Two variants, every capture recorded in m5b_capture_manifest.json with its variant, upstream route template,
# path and status:
# - live: the run's own kima server as shipped: no mood heads (blocked, owner M5 Q3), kima-rs's empty release
#   catalog (D-267), KIMA_NET_DENY_REAL=1 (the update check is refused and answers like a failed check, D-270).
#   Pages that need the heads answer their real 400/200-empty replies.
# - heads: after the live captures the server is stopped and crates/kima-api/tests/m5b_capture.rs serves the same
#   router on the same database with synthetic mood heads and a one-release synthetic catalog installed, so the
#   recipe pages, the mood/workout stations and the release window answer 2xx with data.
# Before the live captures the harness seeds (plain SQL, see its module doc) mood scores, readings, DSP tempo/key,
# mood tags, mood statistics, cover tokens, owner genres and similar artists into the scanned library.

m5b_manifest="$stage/m5b_manifest.tsv"
: > "$m5b_manifest"
tz='Europe/Warsaw'

# mcall VARIANT NAME ROUTE METHOD PATH [JSON] [STATUS]: STATUS (default any 2xx) must match; body sorted (jq -eS).
mcall() {
  local variant="$1" name="$2" route="$3" method="$4" path="$5" payload="${6:-}" want="${7:-2xx}" status root_url auth
  if [[ "$variant" == live ]]; then root_url="$base"; auth="$token"; else root_url="$hbase"; auth="$htoken"; fi
  local args=(-sS -o "$stage/.body" -w '%{http_code}' -X "$method" -H "Authorization: Bearer ${auth}")
  if [[ -n "$payload" ]]; then args+=(-H 'Content-Type: application/json' -d "$payload"); fi
  status="$(curl "${args[@]}" "$root_url$path")"
  if [[ "$want" == 2xx && ! "$status" =~ ^2 ]] || [[ "$want" != 2xx && "$status" != "$want" ]]; then
    echo "m5b_$name: $method $path answered $status (wanted $want): $(head -c 400 "$stage/.body")" >&2
    exit 1
  fi
  jq -eS . "$stage/.body" > "$stage/m5b_${name}_response.json"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$variant" "$route" "$method" "$path" "$status" >> "$m5b_manifest"
  echo "m5b_$name [$variant] $method $path $status" >&2
}
fx() { echo "$stage/m5b_$1_response.json"; }

# --- The library: scan it, then play some of the M5b songs (recommendations, discovery and favorites read plays).
scan="$(request POST /library/scan '{}')"; job="$(jq -er '.jobId' <<< "$scan")"
for _ in {1..120}; do
  status="$(request GET "/library/scan/status/$job" | jq -r '.status')"
  [[ "$status" != failed ]] || { echo 'Library scan failed' >&2; exit 1; }
  [[ "$status" != completed ]] || break
  sleep 1
done
[[ "$status" == completed ]] || { echo 'Library scan timed out' >&2; exit 1; }
tracks="$(request GET '/library/tracks?limit=100')"
m5b_ids="$(jq -c '[.tracks[] | select(.title | test("^(Nineties|Noughties) ")) | .id] | sort' <<< "$tracks")"
[[ "$(jq length <<< "$m5b_ids")" == 40 ]] || { echo 'Expected the 40 M5b fixture songs' >&2; exit 1; }
for id in $(jq -r '.[0:6][]' <<< "$m5b_ids"); do
  request POST /plays "$(jq -nc --arg id "$id" '{trackId:$id,secondsListened:35}')" >/dev/null
done

# --- The harness binary (built from this checkout), then the seed.
cargo test -p kima-api --test m5b_capture --no-run --message-format=json 2>> "$tmp/build.log" > "$tmp/harness-build.json" \
  || { cat "$tmp/build.log" >&2; exit 1; }
harness="$(jq -r 'select(.reason == "compiler-artifact" and .target.name == "m5b_capture" and .executable != null)
  | .executable' "$tmp/harness-build.json" | tail -n1)"
[[ -x "$harness" ]] || { echo 'm5b capture harness was not built' >&2; exit 1; }
KIMA_M5B_CAPTURE=1 "$harness" --ignored --exact m5b_capture_seed --nocapture > "$tmp/seed.log" 2>&1 \
  || { cat "$tmp/seed.log" >&2; exit 1; }
grep -F 'm5b capture seed: ' "$tmp/seed.log" >&2

# --- Variant live (the shipped server).
mcall live mixes 'GET /mixes' GET "/mixes?tz=$tz"
mix="$(jq -er '[.[] | select(.trackCount >= 8)][0].id' "$(fx mixes)")"
mcall live mix 'GET /mixes/:id' GET "/mixes/$mix?tz=$tz"
mcall live mix_save 'POST /mixes/:id/save' POST "/mixes/$mix/save?tz=$tz" '{"name":"Saved capture mix"}'
mcall live mixes_refresh 'POST /mixes/refresh' POST "/mixes/refresh?tz=$tz" '{}'
mcall live presets 'GET /mixes/mood/buckets/presets' GET /mixes/mood/buckets/presets
mcall live bucket_no_heads 'GET /mixes/mood/buckets/:mood' GET /mixes/mood/buckets/chill '' 400
picked="$(jq -c '.[0:5]' <<< "$m5b_ids")"
mcall live bucket_save 'POST /mixes/mood/buckets/:mood/save' POST /mixes/mood/buckets/chill/save \
  "$(jq -nc --argjson ids "$picked" '{trackIds:$ids}')"
mcall live mixes_with_mood 'GET /mixes' GET "/mixes?tz=$tz"
mcall live decades 'GET /library/decades' GET /library/decades
mcall live radio_stations 'GET /library/radio/stations' GET /library/radio/stations
mcall live radio_row 'GET /library/radio/row' GET "/library/radio/row?tz=$tz"
mcall live radio_row_all 'GET /library/radio/row' GET "/library/radio/row?scope=all&tz=$tz"
artist="$(jq -er '.tracks[0].album.artist.id' "$(fx mix)")"
decade="$(jq -er '.decades[0].decade' "$(fx decades)")"
genre="$(jq -er '[.stations[] | select(.type == "genre")][0].value' "$(fx radio_row_all)")"
# A recipe station (not Wildcard, not the personal Late Night one).
station="$(jq -er '[.stations[] | select(.value != "wildcard" and .value != "late-night")][0].value' "$(fx radio_stations)")"
mcall live radio_all 'GET /library/radio' GET '/library/radio?type=all&limit=10'
mcall live radio_genre 'GET /library/radio' GET "/library/radio?type=genre&value=$genre&limit=10"
mcall live radio_decade 'GET /library/radio' GET "/library/radio?type=decade&value=$decade&limit=10"
mcall live radio_artist 'GET /library/radio' GET "/library/radio?type=artist&value=$artist&limit=10"
mcall live radio_discovery 'GET /library/radio' GET '/library/radio?type=discovery&limit=10'
mcall live radio_favorites 'GET /library/radio' GET '/library/radio?type=favorites&limit=10'
mcall live radio_mood_no_heads 'GET /library/radio' GET "/library/radio?type=mood&value=$station&seed=capture&offset=0"
mcall live whats_new_pending 'GET /whats-new/pending' GET /whats-new/pending
mcall live whats_new_current_none 'GET /whats-new/current' GET /whats-new/current '' 404
mcall live whats_new_changelog 'GET /whats-new/changelog' GET /whats-new/changelog
mcall live whats_new_open 'GET /whats-new/open' GET /whats-new/open
mcall live update_status 'GET /whats-new/update-status' GET /whats-new/update-status
mcall live mood_mixes 'GET /whats-new/mood-mixes' GET /whats-new/mood-mixes
mood_mix="$(jq -er '.mix.id' "$(fx mood_mixes)")"
mcall live mood_mixes_decision 'POST /whats-new/mood-mixes/decision' POST /whats-new/mood-mixes/decision '{"decision":"keep"}'
mcall live mood_mixes_save 'POST /whats-new/mood-mixes/save' POST /whats-new/mood-mixes/save \
  "$(jq -nc --arg m "$mood_mix" '{mixId:$m}')"
mcall live mood_mixes_saved 'GET /whats-new/mood-mixes' GET /whats-new/mood-mixes
mcall live choice_unknown_release 'POST /whats-new/:version/choice' POST /whats-new/0.2.0/choice \
  '{"key":"capture.choice","enabled":true}' 404
mcall live dismiss_unknown_release 'POST /whats-new/:version/dismiss' POST /whats-new/0.2.0/dismiss '{}' 404
mcall live recommendations 'GET /recommendations/for-you' GET /recommendations/for-you

# --- Variant heads: stop the server, serve the harness on the same database.
kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true
server_pid=''
hport="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
KIMA_M5B_CAPTURE=1 KIMA_M5B_CAPTURE_PORT="$hport" "$harness" --ignored --exact m5b_capture_serve --nocapture \
  > "$tmp/harness.log" 2>&1 & harness_pid=$!
hbase="http://127.0.0.1:$hport/api"
for _ in {1..60}; do
  if curl -fsS "$hbase/health" > /dev/null 2>&1; then break; fi
  kill -0 "$harness_pid" 2>/dev/null || { cat "$tmp/harness.log" >&2; exit 1; }
  sleep 0.5
done
curl -fsS "$hbase/health" >/dev/null
htoken="$(curl --fail-with-body -sS -X POST -H 'Content-Type: application/json' \
  -d '{"username":"fixtureadmin","password":"fixture-pass-18"}' "$hbase/auth/login" | jq -er '.token')"
mcall heads heads_mixes 'GET /mixes' GET "/mixes?tz=$tz"
hmix="$(jq -er '[.[] | select(.trackCount >= 8)][0].id' "$(fx heads_mixes)")"
mcall heads heads_mix 'GET /mixes/:id' GET "/mixes/$hmix?tz=$tz"
mcall heads heads_presets 'GET /mixes/mood/buckets/presets' GET /mixes/mood/buckets/presets
mcall heads heads_bucket 'GET /mixes/mood/buckets/:mood' GET /mixes/mood/buckets/chill
mcall heads heads_bucket_save 'POST /mixes/mood/buckets/:mood/save' POST /mixes/mood/buckets/party/save '{}'
mcall heads heads_radio_row_all 'GET /library/radio/row' GET "/library/radio/row?scope=all&tz=$tz"
mcall heads heads_radio_mood 'GET /library/radio' GET "/library/radio?type=mood&value=$station&seed=capture&offset=0"
mcall heads heads_radio_workout 'GET /library/radio' GET '/library/radio?type=workout&seed=capture&offset=0'
mcall heads heads_whats_new_pending 'GET /whats-new/pending' GET /whats-new/pending
mcall heads heads_whats_new_current 'GET /whats-new/current' GET /whats-new/current
mcall heads heads_whats_new_choice 'POST /whats-new/:version/choice' POST /whats-new/0.2.0/choice \
  '{"key":"capture.choice","enabled":true}'
mcall heads heads_whats_new_changelog 'GET /whats-new/changelog' GET /whats-new/changelog
mcall heads heads_whats_new_open 'GET /whats-new/open' GET /whats-new/open
mcall heads heads_whats_new_dismiss 'POST /whats-new/:version/dismiss' POST /whats-new/0.2.0/dismiss '{}'
kill "$harness_pid" 2>/dev/null || true
wait "$harness_pid" 2>/dev/null || true
harness_pid=''

# --- Fail before any copy: the manifest, all 22 upstream routes answered 2xx, no token in any body, cross-ids.
jq -Rn --arg head "$(git rev-parse HEAD)" --arg tz "$tz" '
  {source: {head: $head, tz: $tz,
            live: "the run'"'"'s kima server: no mood heads, the shipped empty release catalog, KIMA_NET_DENY_REAL=1",
            heads: "crates/kima-api/tests/m5b_capture.rs m5b_capture_serve: same router and database, synthetic mood heads and a one-release synthetic catalog",
            seed: "crates/kima-api/tests/m5b_capture.rs m5b_capture_seed (SQL: vectors, mood scores, readings, tempo/key, mood tags, mood statistics, cover tokens, owner genres, similar artists)"},
   captures: [inputs | split("\t") | {name: .[0], variant: .[1], route: .[2], method: .[3], path: .[4],
              status: (.[5] | tonumber)}]}' < "$m5b_manifest" > "$stage/m5b_capture_manifest.json"
[[ "$(jq '.captures | length' "$stage/m5b_capture_manifest.json")" == "${#m5b_names[@]}" ]] \
  || { echo "Expected ${#m5b_names[@]} m5b captures" >&2; exit 1; }
[[ "$(jq '[.captures[] | select(.status < 300) | .route] | unique | length' "$stage/m5b_capture_manifest.json")" == 22 ]] \
  || { echo 'Expected all 22 M5b routes answered 2xx' >&2; exit 1; }
if grep -l 'eyJ' "$stage"/m5b_*_response.json "$stage/m5b_capture_manifest.json"; then
  echo 'A token-like string is in an m5b fixture' >&2; exit 1
fi
jq -e --slurpfile m "$(fx mix)" '.[0].id == $m[0].id' "$(fx mixes)" >/dev/null
jq -e '.trackIds == [.tracks[].id]' "$(fx mix)" >/dev/null
jq -e --slurpfile m "$(fx mix)" '.trackCount == $m[0].trackCount and .name == "Saved capture mix"' "$(fx mix_save)" >/dev/null
jq -e --argjson ids "$picked" '.mix.trackIds == $ids and .success == true' "$(fx bucket_save)" >/dev/null
jq -e --slurpfile b "$(fx bucket_save)" 'any(.[]; .id == $b[0].mix.id and .type == "mood")' "$(fx mixes_with_mood)" >/dev/null
jq -e --slurpfile b "$(fx bucket_save)" '.mix.id == $b[0].mix.id' "$(fx mood_mixes)" >/dev/null
jq -e --slurpfile s "$(fx mood_mixes_save)" '.playlist.id == $s[0].id and .decision == "keep"' "$(fx mood_mixes_saved)" >/dev/null
jq -e --arg a "$artist" '(.tracks | length) > 0 and all(.tracks[]; .artist.id == $a)' "$(fx radio_artist)" >/dev/null
jq -e '(.artists | length) > 0' "$(fx recommendations)" >/dev/null
jq -e '.trackIds == [.tracks[].id] and (.tracks | length) >= 8' "$(fx heads_bucket)" >/dev/null
jq -e '(.tracks | length) > 0' "$(fx heads_radio_mood)" >/dev/null
jq -e '.release.version == "0.2.0"' "$(fx heads_whats_new_current)" >/dev/null
jq -e '.version == "0.2.0" and .dismissed == true' "$(fx heads_whats_new_dismiss)" >/dev/null
