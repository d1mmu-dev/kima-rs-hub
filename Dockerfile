# syntax=docker/dockerfile:1

# Toolchain for `vertigo build`: vertigo-cli (same version as the `vertigo` crate, =0.13.4 in
# crates/kima-web/Cargo.toml) and wasm-opt from a binaryen release (bookworm's apt binaryen is too old).
FROM rust:1-bookworm AS web-toolchain
ARG BINARYEN_VERSION=133
RUN rustup target add wasm32-unknown-unknown \
 && cargo install vertigo-cli --version 0.13.4 --locked \
 && case "$(uname -m)" in x86_64) arch=x86_64 ;; aarch64) arch=aarch64 ;; *) echo "unsupported arch" >&2; exit 1 ;; esac \
 && curl -fsSL "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${arch}-linux.tar.gz" \
    | tar xz -C /opt \
 && ln -s "/opt/binaryen-version_${BINARYEN_VERSION}/bin/wasm-opt" /usr/local/bin/wasm-opt \
 && wasm-opt --version

# kima-web build. The output directory is the SAME path at build time and at run time: vertigo serves the
# directory under the path it is given, so the public asset URLs contain it (crates/kima-web/README.md).
FROM web-toolchain AS web
WORKDIR /src
COPY . .
RUN vertigo build kima-web --dest-dir /srv/kima/web \
 && ls -l /srv/kima/web

FROM rust:1-bookworm AS backend
WORKDIR /src
COPY . .
ENV SQLX_OFFLINE=true
RUN cargo build --release -p kima-server

# Postgres 17 + pgvector on Debian bookworm; adds the kima binary and the kima-web build.
FROM pgvector/pgvector:pg17
RUN apt-get update \
 && apt-get install -y --no-install-recommends tini ca-certificates ffmpeg libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /data --shell /usr/sbin/nologin kima
COPY --from=backend /src/target/release/kima /usr/local/bin/kima
COPY --from=web /srv/kima/web /srv/kima/web
COPY docker/entrypoint.sh /usr/local/bin/kima-entrypoint
RUN chmod +x /usr/local/bin/kima-entrypoint
ENV KIMA_DATA_DIR=/data \
    KIMA_WEB_BUILD_DIR=/srv/kima/web \
    MUSIC_PATH=/music \
    PORT=3030
EXPOSE 3030
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s \
  CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${PORT:-3030} && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -qE '^HTTP/1\\.[01] 200' <&3"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/kima-entrypoint"]
