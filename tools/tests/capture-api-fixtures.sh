#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
script="$root/tools/capture-api-fixtures.sh"
if DATABASE_URL=postgres://anything bash "$script" --help > /dev/null 2>&1; then
  echo 'capture must reject a caller database URL' >&2; exit 1
fi
if env -u DATABASE_URL -u PGDATABASE "$script" --help 2>&1 | grep -q 'No external DATABASE_URL'; then
  echo 'capture script safety contract: green'
else
  echo 'capture script missing safety contract' >&2; exit 1
fi
