#!/bin/bash
# Starts the bundled Postgres (unless DATABASE_URL points elsewhere), then kima; stops both cleanly.
set -euo pipefail

PGDATA=/data/postgres
PGPASS_FILE=/data/postgres-password
# /data is writable by the unprivileged kima user; never follow a symlink planted there while running as root.
for p in /data "$PGDATA" "$PGPASS_FILE"; do
    if [ -L "$p" ]; then
        echo "kima-entrypoint: refusing to start: $p is a symlink" >&2
        exit 1
    fi
done
mkdir -p /data
chown -h kima:kima /data

# psql as the postgres superuser over the local socket (peer auth): pg_sql DB [psql args]; SQL on stdin.
pg_sql() {
    local db=$1
    shift
    gosu postgres psql -X -q -v ON_ERROR_STOP=1 -d "$db" "$@"
}

if [ -z "${DATABASE_URL:-}" ]; then
    if [ -e "$PGDATA" ]; then
        # kima owns /data and could have planted its own cluster (with a crafted postgresql.conf)
        # here; only accept a directory that postgres itself owns.
        if [ -L "$PGDATA" ] || [ ! -d "$PGDATA" ] || [ "$(stat -c '%U:%G %a' "$PGDATA")" != "postgres:postgres 700" ]; then
            echo "kima-entrypoint: refusing to start: $PGDATA must be a directory owned by postgres:postgres with mode 700" >&2
            echo "kima-entrypoint: found: $(stat -c '%F %U:%G %a' "$PGDATA")" >&2
            exit 1
        fi
    else
        mkdir "$PGDATA"
        chown -h postgres:postgres "$PGDATA"
        chmod 700 "$PGDATA"
    fi
    if [ ! -s "$PGDATA/PG_VERSION" ]; then
        gosu postgres initdb -D "$PGDATA" --username=postgres --auth-local=peer --auth-host=scram-sha-256 \
            --encoding=UTF8 >/dev/null
        echo "listen_addresses = '127.0.0.1'" >> "$PGDATA/postgresql.conf"
    elif grep -qE '^[^#]*[[:space:]]trust([[:space:]]|$)' "$PGDATA/pg_hba.conf"; then
        echo "kima-entrypoint: $PGDATA was created by an older image (passwordless 'kima' superuser)." >&2
        echo "kima-entrypoint: dump it with that image, remove $PGDATA, then start this image and restore." >&2
        exit 1
    fi
    # Only the postgres OS user may use the superuser; kima logs in with a password over 127.0.0.1.
    # Written as postgres, not root: a planted symlink inside PGDATA then cannot reach root-owned files.
    gosu postgres sh -c 'umask 077 && cat > "$1/pg_hba.conf"' sh "$PGDATA" <<'HBA'
local all  postgres                peer
host  kima kima     127.0.0.1/32   scram-sha-256
HBA

    # Generated once, readable by root only; kima receives it through DATABASE_URL.
    if [ ! -s "$PGPASS_FILE" ]; then
        rm -f "$PGPASS_FILE"
        (umask 077 && set -o noclobber && head -c 48 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' > "$PGPASS_FILE")
    fi
    # A hardlink planted by kima would make the chown/chmod below hit another file.
    if [ -L "$PGPASS_FILE" ] || [ "$(stat -c '%h %F' "$PGPASS_FILE")" != "1 regular file" ]; then
        echo "kima-entrypoint: refusing to start: $PGPASS_FILE is not a regular single-link file" >&2
        exit 1
    fi
    chown -h root:root "$PGPASS_FILE"
    chmod 600 "$PGPASS_FILE"
    PGPASS=$(cat "$PGPASS_FILE")
    # kima owns /data and could swap the file; only accept what this script generates.
    if ! printf '%s' "$PGPASS" | grep -qxE '[A-Za-z0-9]{32,}'; then
        echo "kima-entrypoint: refusing to start: $PGPASS_FILE is not a generated password" >&2
        exit 1
    fi

    gosu postgres pg_ctl -D "$PGDATA" -w -l "$PGDATA/server.log" start
    # Idempotent on every start: an unprivileged login role owning the kima database.
    pg_sql postgres -v pass="$PGPASS" <<'SQL'
SELECT 'CREATE ROLE kima LOGIN' WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'kima')\gexec
ALTER ROLE kima LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD :'pass';
SELECT 'CREATE DATABASE kima OWNER kima ENCODING ''UTF8''' WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'kima')\gexec
ALTER DATABASE kima OWNER TO kima;
SQL
    # Extensions a non-superuser cannot create; the migrations then find them present.
    pg_sql kima <<'SQL'
CREATE EXTENSION IF NOT EXISTS unaccent;
SQL
    export DATABASE_URL="postgres://kima:${PGPASS}@127.0.0.1:5432/kima"
    unset PGPASS
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
