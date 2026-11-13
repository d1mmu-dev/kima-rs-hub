# shellcheck shell=bash
# M6 podcast captures (plan 2026-11-12-m6-podcasts.md, Task 19). Sourced by
# tools/capture-api-fixtures.sh after its M-STATS part (the stats plays give
# /library/recently-listened its artist rows). Expects: base (API root), token, stage (staging
# dir), mock (loopback root of tools/podcast-capture-mock.py, already serving and configured as
# KIMA_ITUNES_BASE_URL + KIMA_NET_ALLOW_LOOPBACK for the server), request() from the caller.
#
# Real flow, no SQL seeding: both shows are followed through POST /podcasts/subscribe, which
# fetches the loopback feeds; Apple answers come from the loopback mock (synthetic, not a
# recording). Captures stop at the first non-2xx answer. Every capture is recorded with its
# upstream route template, status and X-Kima-Ai-Hidden header in podcasts_capture_manifest.json.

podcast_manifest="$stage/podcasts_manifest.tsv"
: > "$podcast_manifest"

# pcall NAME ROUTE METHOD PATH [JSON]: a JSON route; 2xx required; body sorted with jq -eS.
pcall() {
  local name="$1" route="$2" method="$3" path="$4" payload="${5:-}" status hidden
  local args=(-sS -o "$stage/.body" -D "$stage/.headers" -w '%{http_code}' -X "$method"
    -H "Authorization: Bearer $token")
  if [[ -n "$payload" ]]; then args+=(-H 'Content-Type: application/json' -d "$payload"); fi
  status="$(curl "${args[@]}" "$base$path")"
  if [[ ! "$status" =~ ^2 ]]; then
    echo "podcasts_$name: $method $path answered $status: $(head -c 400 "$stage/.body")" >&2
    exit 1
  fi
  jq -eS . "$stage/.body" > "$stage/podcasts_${name}_response.json"
  hidden="$(tr -d '\r' < "$stage/.headers" | awk -F': ' 'tolower($1) == "x-kima-ai-hidden" {print $2}')"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$route" "$method" "$path" "$status" "$hidden" >> "$podcast_manifest"
  echo "podcasts_$name $method $path $status hidden=${hidden:-none}" >&2
}

# pstatus ROUTE METHOD PATH [curl args]: a route that answers audio or a redirect; 2xx/3xx.
pstatus() {
  local route="$1" method="$2" path="$3" status
  shift 3
  status="$(curl -sS -o /dev/null -w '%{http_code}' -X "$method" -H "Authorization: Bearer $token" "$@" "$base$path")"
  if [[ ! "$status" =~ ^[23] ]]; then echo "$method $path answered $status" >&2; exit 1; fi
  printf '\t%s\t%s\t%s\t%s\t\n' "$route" "$method" "$path" "$status" >> "$podcast_manifest"
  echo "(status) $method $path $status" >&2
}

# The ids of a show's episodes by title (?ai=show, so the AI-tagged one is listed too).
episode_id() { jq -er --arg t "$2" '.episodes[] | select(.title == $t) | .id' <<< "$1"; }

pcall categories 'GET /podcasts/categories' GET /podcasts/categories
pcall subscribe 'POST /podcasts/subscribe' POST /podcasts/subscribe \
  "$(jq -nc --arg u "$mock/feeds/race.xml" '{feedUrl: $u, itunesId: "555"}')"
race="$(jq -er '.podcast.id' "$stage/podcasts_subscribe_response.json")"
[[ "$(jq -r .message "$stage/podcasts_subscribe_response.json")" == 'Subscribed successfully' ]]
robot="$(request POST /podcasts/subscribe "$(jq -nc --arg u "$mock/feeds/robot.xml" '{feedUrl: $u, itunesId: "777"}')" | jq -er '.podcast.id')"
race_eps="$(request GET "/podcasts/$race/episodes?ai=show" '')"
robot_eps="$(request GET "/podcasts/$robot/episodes?ai=show" '')"
quali="$(episode_id "$race_eps" Qualifying)"; sprint="$(episode_id "$race_eps" Sprint)"
gp="$(episode_id "$race_eps" 'Grand Prix')"; recap="$(episode_id "$race_eps" 'Race Recap')"
robot2="$(episode_id "$robot_eps" 'Robot Hour 2')"
[[ "$(jq -r '.episodes | length' <<< "$race_eps")" == 4 && -n "$recap" ]]

# Progress first, so the lists below carry populated progress objects.
pcall progress 'POST /podcasts/:podcastId/episodes/:episodeId/progress' POST \
  "/podcasts/$race/episodes/$sprint/progress" '{"currentTime":120,"duration":1200,"isFinished":false}'
request POST "/podcasts/$robot/episodes/$robot2/progress" '{"currentTime":300,"duration":1500}' >/dev/null

pcall list 'GET /podcasts' GET /podcasts
pcall list_ai_show 'GET /podcasts' GET '/podcasts?ai=show'
pcall detail 'GET /podcasts/:id' GET "/podcasts/$race"
pcall episodes 'GET /podcasts/:id/episodes' GET "/podcasts/$race/episodes"
pcall next 'GET /podcasts/:podcastId/episodes/:episodeId/next' GET "/podcasts/$race/episodes/$quali/next"
pcall next_end 'GET /podcasts/:podcastId/episodes/:episodeId/next' GET "/podcasts/$race/episodes/$gp/next"
pcall listening 'GET /podcasts/listening' GET /podcasts/listening
# The persisted setting (not ?ai=): show AI content, read the shelf, hide again.
request POST /settings '{"podcastAiContent":"show"}' | jq -e '.podcastAiContent == "show"' >/dev/null
pcall listening_ai_show 'GET /podcasts/listening' GET /podcasts/listening
request POST /settings '{"podcastAiContent":"hide"}' | jq -e '.podcastAiContent == "hide"' >/dev/null

pcall charts 'GET /podcasts/charts' GET /podcasts/charts
pcall discover_top 'GET /podcasts/discover/top' GET /podcasts/discover/top
pcall discover_genre 'GET /podcasts/discover/genre/:genreId' GET /podcasts/discover/genre/1495
pcall discover_genres 'GET /podcasts/discover/genres' GET '/podcasts/discover/genres?genres=1495,1318'
pcall for_you 'GET /podcasts/for-you' GET /podcasts/for-you
pcall similar 'GET /podcasts/:id/similar' GET "/podcasts/$race/similar"
pcall preview 'GET /podcasts/preview/:itunesId' GET /podcasts/preview/888
pcall preview_followed 'GET /podcasts/preview/:itunesId' GET /podcasts/preview/555
pcall preview_similar 'GET /podcasts/preview/:itunesId/similar' GET /podcasts/preview/888/similar
pcall search 'GET /search' GET '/search?q=race&type=all'
pcall search_discover 'GET /search/discover' GET '/search/discover?q=race&type=podcasts'
pcall recently_listened 'GET /library/recently-listened' GET /library/recently-listened
pcall stats_spoken 'GET /stats' GET '/stats?period=all&tz=UTC'

pstatus 'GET /podcasts/:podcastId/episodes/:episodeId/stream' GET "/podcasts/$race/episodes/$gp/stream" -H 'Range: bytes=0-15'
pstatus 'GET /podcasts/preview/:itunesId/snippet' GET /podcasts/preview/888/snippet -H 'Range: bytes=0-15'
pstatus 'GET /podcasts/:id/cover' GET "/podcasts/$race/cover"
pstatus 'GET /podcasts/episodes/:episodeId/cover' GET "/podcasts/episodes/$gp/cover"

# The feed gains "Pit Stop"; the manual refresh must find exactly it.
curl -fsS -X POST "$mock/control/race-new-episode" >/dev/null
pcall refresh 'GET /podcasts/:id/refresh' GET "/podcasts/$race/refresh"
pcall refresh_all 'POST /podcasts/refresh-all' POST /podcasts/refresh-all '{}'
pcall progress_clear 'DELETE /podcasts/:podcastId/episodes/:episodeId/progress' DELETE \
  "/podcasts/$race/episodes/$sprint/progress"
pcall ai_label_show 'PUT /podcasts/:id/ai-label' PUT "/podcasts/$race/ai-label" '{"label":"human"}'
pcall ai_label_episode 'PUT /podcasts/:podcastId/episodes/:episodeId/ai-label' PUT \
  "/podcasts/$race/episodes/$gp/ai-label" '{"label":"ai"}'
pcall unsubscribe 'DELETE /podcasts/:id/unsubscribe' DELETE "/podcasts/$robot/unsubscribe"

# Fail before any copy: 30 named captures, all 24 upstream podcast routes answered.
jq -Rn --arg head "$(git rev-parse HEAD)" '
  {source: {head: $head,
            apple: "tools/podcast-capture-mock.py on loopback (synthetic data, not an Apple recording)",
            feeds: "tools/podcast-capture-mock.py on loopback, fetched by the server (KIMA_NET_ALLOW_LOOPBACK)"},
   captures: [inputs | split("\t") | {name: (if .[0] == "" then null else .[0] end), route: .[1],
              method: .[2], path: .[3], status: (.[4] | tonumber),
              aiHidden: (if .[5] == "" then null else .[5] end)}]}' \
  < "$podcast_manifest" > "$stage/podcasts_capture_manifest.json"
[[ "$(jq '[.captures[] | select(.name != null)] | length' "$stage/podcasts_capture_manifest.json")" == 30 ]] \
  || { echo 'Expected 30 named podcast captures' >&2; exit 1; }
[[ "$(jq '[.captures[].route | select(startswith("GET /podcasts") or startswith("POST /podcasts")
          or startswith("DELETE /podcasts")) | select(contains("ai-label") | not)] | unique | length' \
      "$stage/podcasts_capture_manifest.json")" == 24 ]] \
  || { echo 'Expected all 24 upstream podcast routes answered' >&2; exit 1; }
# Critical cross-ids before copying (the Rust tests re-check these and more).
[[ "$(jq -r .id "$stage/podcasts_detail_response.json")" == "$race" ]]
[[ "$(jq -r '.episode.id' "$stage/podcasts_next_response.json")" == "$sprint" ]]
[[ "$(jq -r '.inProgress[0].episode.id' "$stage/podcasts_listening_response.json")" == "$sprint" ]]
[[ "$(jq -r '.newEpisodesCount' "$stage/podcasts_refresh_response.json")" == 1 ]]
jq -e '.previewEpisodes | length == 2' "$stage/podcasts_preview_response.json" >/dev/null
jq -e '.snippet.title == "Preview One"' "$stage/podcasts_preview_response.json" >/dev/null
jq -e --arg s "$sprint" '[.items[] | select(.type == "podcast")][0].episodeId == $s' \
  "$stage/podcasts_recently_listened_response.json" >/dev/null
