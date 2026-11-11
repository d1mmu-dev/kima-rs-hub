#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
script="$root/tools/capture-api-fixtures.sh"
if DATABASE_URL=postgres://anything bash "$script" --help > /dev/null 2>&1; then
  echo 'capture must reject a caller database URL' >&2; exit 1
fi
if PGDATABASE=existing env -u DATABASE_URL bash "$script" --help > /dev/null 2>&1; then
  echo 'capture must reject a caller database name' >&2; exit 1
fi
if ! env -u DATABASE_URL -u PGDATABASE bash "$script" --help 2>&1 | grep -q 'No external DATABASE_URL'; then
  echo 'capture script missing safety contract' >&2; exit 1
fi
if env -u DATABASE_URL -u PGDATABASE bash "$script" unexpected > /dev/null 2>&1; then
  echo 'capture must reject positional arguments before any infrastructure starts' >&2; exit 1
fi

# Exercise the real cleanup function offline: Docker may not be removed until
# the server child is reaped (not merely sent a signal).
source <(sed -n '/^cleanup() {/,/^}/p' "$script")
sleep 60 & server_pid=$!
container_name=offline-capture-test
tmp="$(mktemp -d "${TMPDIR:-$HOME/.hermes/cache/scratch}/msync18-safety.XXXXXXXX")"
db_removed_early=0
docker() {
  if kill -0 "$server_pid" 2>/dev/null; then db_removed_early=1; fi
  return 0
}
cleanup
if [[ "$db_removed_early" == 1 ]]; then
  echo 'database removed while server was still alive/unreaped' >&2; exit 1
fi
if kill -0 "$server_pid" 2>/dev/null || [[ -e "$tmp" ]]; then
  echo 'capture cleanup left server or temporary data behind' >&2; exit 1
fi

# Execute the request helper with a fake curl so both methods must forward the
# login token as an Authorization header, never a literal or the register reply.
source <(sed -n '/^request() {/,/^}/p' "$script")
token=offline-login-token
base=http://127.0.0.1:1/api
curl() { printf '%s\n' "$@"; }
for method in GET POST; do
  args="$(request "$method" /probe '{}')"
  [[ "$args" == *"Authorization: Bearer $token"* ]] || {
    echo "$method did not use the login token" >&2; exit 1;
  }
done
# The token must originate from an independent login, not the registration reply.
if ! grep -Eq '^token=.*"\$base/auth/login".*jq -er '\''.token'\''\)' "$script"; then
  echo 'capture must extract its bearer token from login' >&2; exit 1
fi
echo 'capture script safety/auth contract: green'
