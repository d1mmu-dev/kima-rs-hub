#!/bin/bash
# Starts the bundled Postgres (unless DATABASE_URL points elsewhere), then kima; stops both cleanly.
set -euo pipefail

PGDATA=/data/postgres
mkdir -p /data
chown kima:kima /data

if [ -z "${DATABASE_URL:-}" ]; then
    mkdir -p "$PGDATA"
    chown -R postgres:postgres "$PGDATA"
    chmod 700 "$PGDATA"
    if [ ! -s "$PGDATA/PG_VERSION" ]; then
        gosu postgres initdb -D "$PGDATA" --username=kima --auth=trust --encoding=UTF8 >/dev/null
        echo "listen_addresses = '127.0.0.1'" >> "$PGDATA/postgresql.conf"
    fi
    gosu postgres pg_ctl -D "$PGDATA" -w -l "$PGDATA/server.log" start
    gosu postgres psql -h 127.0.0.1 -U kima -d postgres -tc "SELECT 1 FROM pg_database WHERE datname = 'kima'" \
        | grep -q 1 || gosu postgres createdb -h 127.0.0.1 -U kima kima
    export DATABASE_URL="postgres://kima@127.0.0.1:5432/kima"
    BUNDLED_PG=1
fi

stop_pg() {
    if [ "${BUNDLED_PG:-}" = 1 ]; then gosu postgres pg_ctl -D "$PGDATA" -m fast stop || true; fi
}

gosu kima kima &
KIMA_PID=$!
trap 'kill -TERM "$KIMA_PID" 2>/dev/null || true' TERM INT
set +e
wait "$KIMA_PID"; status=$?
# `wait` returns early when a trapped signal arrives; wait again for kima to finish.
if kill -0 "$KIMA_PID" 2>/dev/null; then wait "$KIMA_PID"; status=$?; fi
set -e
stop_pg
exit "$status"
