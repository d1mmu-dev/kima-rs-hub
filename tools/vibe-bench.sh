#!/usr/bin/env bash
# M5 Task 6 measurements: runs crates/kima-db/examples/vibe_bench.rs against a run-owned pgvector container.
# Never touches any other database; the container is removed on exit.
set -euo pipefail
cd "$(dirname "$0")/.."
NAME="kima-m5-bench-$$"
PORT=$(python3 -c "
import socket,random
while True:
    p=random.randint(20000,60000)
    s=socket.socket()
    try:
        s.bind(('127.0.0.1',p)); s.close(); print(p); break
    except OSError: pass")
trap 'docker rm -f "$NAME" >/dev/null 2>&1 || true' EXIT
docker run -d --name "$NAME" -e POSTGRES_PASSWORD=pw -e POSTGRES_DB=m5bench -p "127.0.0.1:$PORT:5432" \
  pgvector/pgvector:pg17 -c shared_buffers="${BENCH_SHARED_BUFFERS:-1GB}" -c fsync=off -c max_wal_size=8GB ${BENCH_PG_EXTRA:-} >/dev/null
for _ in $(seq 60); do docker exec "$NAME" pg_isready -U postgres -d m5bench >/dev/null 2>&1 && break; sleep 1; done
sleep 2
export BENCH_DATABASE_URL="postgres://postgres:pw@127.0.0.1:$PORT/m5bench"
export DATABASE_URL="$BENCH_DATABASE_URL" SQLX_OFFLINE=true
cargo run --release -j2 -p kima-db --example vibe_bench
