# Sourced by capture-api-fixtures.sh after registration/onboarding. Never run standalone.
# Only actual 2xx server bodies become fixtures. Keep observed UUIDs and timestamps intact.
profile_routes="$tmp/profile-routes.jsonl"
: > "$profile_routes"
pcall() {
  local name="$1" method="$2" path="$3" payload="${4:-}"
  local raw="$tmp/profile-$name.raw" status
  local data=()
  [[ -z "$payload" ]] || data=(--data "$payload")
  status="$(curl -sS -o "$raw" -w '%{http_code}' -X "$method" \
    -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    "${data[@]}" "$base$path")"
  [[ "$status" =~ ^2[0-9][0-9]$ ]] || { echo "Profile capture $name failed (HTTP $status)" >&2; return 1; }
  # Authentication secrets are the ONLY changed values. Original bodies stay in private tmp.
  if [[ "$name" == login || "$name" == switch ]]; then
    jq -eS '.token = "[REDACTED]" | .refreshToken = "[REDACTED]"' "$raw"
  else
    jq -eS . "$raw"
  fi > "$stage/profiles_${name}_response.json"
  jq -nc --arg file "profiles_${name}_response.json" --arg method "$method" \
    --arg path "$path" --argjson status "$status" \
    '{file:$file,method:$method,path:$path,status:$status}' >> "$profile_routes"
}
main_token="$token"
pcall login POST /auth/login '{"username":"fixtureadmin","password":"fixture-pass-18"}'
pcall me GET /auth/me
pcall create POST /profiles '{"name":"Fixture Kids","kind":"kids","avatar":"fox"}'
profile_id="$(jq -er .id "$stage/profiles_create_response.json")"
pcall update PUT "/profiles/$profile_id" '{"name":"Fixture Reader","kind":"standard","avatar":"owl"}'
pcall list GET /profiles
pcall settings GET /settings
playlist_raw="$(request POST /playlists '{"name":"Main fixture","isPublic":false}')"
playlist_id="$(jq -er .id <<< "$playlist_raw")"
pcall playlist GET "/playlists/$playlist_id"
pcall switch POST "/profiles/$profile_id/switch" '{}'
token="$(jq -er .token "$tmp/profile-switch.raw")"
pcall me_switched GET /auth/me
request POST /settings '{"wifiOnly":true}' >/dev/null
pcall settings_switched GET /settings
playlist_raw="$(request POST /playlists '{"name":"Reader fixture","isPublic":false}')"
playlist_id="$(jq -er .id <<< "$playlist_raw")"
pcall playlist_switched GET "/playlists/$playlist_id"
token="$main_token"
pcall delete DELETE "/profiles/$profile_id"
pcall list_after_delete GET /profiles
jq -nS --arg commit "$(git rev-parse HEAD)" --slurpfile routes "$profile_routes" \
  '{sourceCommit:$commit, captureSet:"profiles", routes:$routes,
    conditions:["Fresh run-owned database; synthetic account and empty playlists",
      "Main profile then standard profile (created as kids, updated before switch)",
      "Main settings default; switched settings wifiOnly=true; main deletes sibling"],
    redactions:{files:["profiles_login_response.json","profiles_switch_response.json"],
      fields:["token","refreshToken"],replacement:"[REDACTED]"},
    preservation:"All other response values, including dynamic UUIDs and timestamps, are unchanged"}' \
  > "$stage/profiles_capture_manifest.json"
