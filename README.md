# kima-rs

kima-rs is a Rust rewrite of [Kima](https://github.com/Chevron7Locked/kima-hub), a self-hosted
music server. It keeps the upstream Next.js frontend (built as a static export) and replaces the
backend with a single `kima` binary plus Postgres. Licensed under GPL-3.0-only.

## Status

Milestone M0 (skeleton): accounts and login only. You can register the first (admin) account,
sign in, and see the app shell. There is no music yet: no scanner, library, playback or
integrations. See `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` for what comes next.

## Quick start

```bash
docker compose up
```

Then open <http://localhost:3030> and register. The first account becomes the admin. The image
bundles Postgres and the `kima` binary; state lives in the `kima_data` volume (`/data`). Put your
music in `./music` or set `MUSIC_DIR` to its location (mounted read-only at `/music`).

## Local development

Requirements: Rust 1.85+, Node.js, Docker (for the dev database).

1. Start the dev Postgres (listens on `localhost:5456`):
   ```bash
   docker compose -f docker-compose.dev.yml up -d
   ```
2. Copy the environment file: `cp .env.example .env`
3. Run the server (nothing loads `.env` for you, so export it into the shell first):
   ```bash
   set -a; source .env; set +a; cargo run -p kima-server
   ```
   Migrations run on startup; it listens on port 3030 unless `PORT` is set.
   `.env.example` sets `KIMA_DATA_DIR=./.data`, so a non-root run doesn't need a writable `/data`.
4. Frontend dev loop: run the Rust server on another port and point the Next dev server at it.
   ```bash
   set -a; source .env; set +a; PORT=3111 cargo run -p kima-server
   cd frontend && npm install && npm run dev
   ```
   `npm run dev` serves on port 3030 and rewrites `/api`, `/rest` and `/health` to the backend.

Note: under `npm run dev`, `/api/events` goes through a Next dev rewrite, which may buffer
server-sent events. Test event-stream behaviour against the built export served by kima
(`npm run build` in `frontend/`, then run kima with `KIMA_FRONTEND_DIR=frontend/out`).

Checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, and in `frontend/`: `npx tsc --noEmit`, `npm run test:unit`.
CI builds with `SQLX_OFFLINE=true`; after changing any `sqlx::query!` macro run
`cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/`.

## Environment variables

Upstream's names are kept.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | none | Postgres connection string. In the all-in-one image, setting it skips the bundled Postgres. |
| `MUSIC_PATH` | `/music` | Music library directory. |
| `PORT` | `3030` | HTTP port. |
| `LOG_LEVEL` | `info` | `debug`, `info`, `warn`, `error` or `silent`. |
| `ALLOWED_ORIGINS` | unset (any origin) | Comma-separated CORS origins. |
| `SESSION_SECRET` | generated | JWT signing secret; overrides the generated one. |
| `SETTINGS_ENCRYPTION_KEY` | generated | Key for encrypted settings; overrides the generated one. |

New in kima-rs:

| Variable | Default | Meaning |
|---|---|---|
| `KIMA_DATA_DIR` | `/data` | State directory. Holds `secrets.json` (mode 0600), which stores the generated secrets. |
| `KIMA_FRONTEND_DIR` | unset | Directory of the exported frontend. Unset means the server serves the API only. |
| `KIMA_BIND_HOST` | `0.0.0.0` | Address to bind. |

Keep `secrets.json` (or the two secret variables) safe: losing it makes stored encrypted settings
unreadable. A corrupt file stops startup rather than being replaced.

## Differences from upstream

Every place where kima-rs differs from upstream Kima is logged in
[`docs/DIVERGENCES.md`](docs/DIVERGENCES.md), with the reason and a status, so each can be revisited.
