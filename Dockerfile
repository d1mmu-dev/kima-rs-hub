# syntax=docker/dockerfile:1

# Only linux/amd64 has an approved, checksum-pinned ORT package.
FROM debian:bookworm-slim AS ort-runtime
RUN apt-get update && apt-get install -y --no-install-recommends curl python3 ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY scripts/acquire-ort.sh scripts/provision-ort.py /scripts/
RUN mkdir -p /opt/kima && sh /scripts/acquire-ort.sh /opt/kima/ort

FROM node:24-slim AS frontend
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS backend
RUN apt-get update && apt-get install -y --no-install-recommends libsoxr-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
ENV SQLX_OFFLINE=true
RUN cargo build --release -p kima-server

# Postgres 17 + pgvector on Debian bookworm; adds the kima binary and the exported frontend.
FROM pgvector/pgvector:pg17 AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends tini ca-certificates ffmpeg libsoxr0 libstdc++6 libgcc-s1 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /data --shell /usr/sbin/nologin kima
COPY --from=ort-runtime /opt/kima/ort /opt/kima/ort
RUN chown -R root:root /opt/kima && chmod -R a-w /opt/kima/ort
ENV ORT_DYLIB_PATH=/opt/kima/ort/libonnxruntime.so.1.22.0
COPY --from=backend /src/target/release/kima /usr/local/bin/kima
COPY --from=frontend /src/frontend/out /srv/kima/web
COPY docker/entrypoint.sh /usr/local/bin/kima-entrypoint
RUN chmod +x /usr/local/bin/kima-entrypoint
ENV KIMA_DATA_DIR=/data \
    KIMA_FRONTEND_DIR=/srv/kima/web \
    MUSIC_PATH=/music \
    PORT=3030
EXPOSE 3030
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${PORT:-3030} && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -qE '^HTTP/1\\.[01] 200' <&3"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/kima-entrypoint"]
