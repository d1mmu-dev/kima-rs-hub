# kima-rs-hub

This project is a fork of [Chevron7Locked/kima-hub](https://github.com/Chevron7Locked/kima-hub),
ported to the Rust language.

Kima is a self-hosted music server. kima-rs keeps the upstream Next.js frontend (built as a static
export) and replaces the Node/TypeScript backend and the Python analyzers with a single Rust
`kima` binary plus Postgres. Every difference from upstream is logged in
[`docs/DIVERGENCES.md`](docs/DIVERGENCES.md). Licensed under GPL-3.0-only, like upstream.

## Status

Milestone M1 (library and playback) is done. On top of M0's accounts and login, kima-rs now
scans and watches a music folder, shows it as Artist, then Year - Album, then songs, and lets you
browse, search, stream (with transcodes), keep playlists and resume playback. An admin can also
preview and apply a tidy on-disk layout. Not yet built: Subsonic, identification (MusicBrainz),
audio analysis, podcasts and audiobooks, integrations. See
`docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` for what comes next.

## Quick start

```bash
docker compose up
```

Then open <http://localhost:3030> and register. The first account becomes the admin. The image
bundles Postgres, `ffmpeg` and the `kima` binary; state lives in the `kima_data` volume (`/data`).
The bundled Postgres gets a random password on first start (`/data/postgres-password`, readable by
root only) and kima connects as an unprivileged `kima` role. Put your
music in `./music` or set `MUSIC_DIR` to its location (mounted read-only at `/music`).

## Library

The library is whatever is under `MUSIC_PATH` (`/music` in the image). Tags are read from the
files; missing fields fall back to the folder names. Albums are grouped per album artist, title
and year, so a remaster stays a separate album while CD1/CD2 folders still merge.

**Watching.** kima watches the folder and picks up added, changed, renamed and removed files
without a full rescan; a file still being copied is read once it stops changing. A full
reconcile also runs at startup.

- `KIMA_WATCH_MODE=auto` (default) uses native file events and falls back to polling if they
  fail. `poll` always polls, every `KIMA_WATCH_POLL_SECS` seconds (default `60`, must be
  greater than 0). `off` watches nothing.
- On Linux, native watching can hit the inotify limit on large libraries (the watcher status,
  `GET /api/library/watch-status`, then
  names `fs.inotify.max_user_watches`). Raise it on the host, for example
  `sudo sysctl fs.inotify.max_user_watches=524288`, or use `poll`. Network mounts (SMB/NFS)
  usually do not deliver events, so use `poll` there.
- Files that disappear are flagged missing, not deleted, and are purged only after 7 days, so
  an unplugged disk loses nothing. Plays and playlists stay attached when it comes back.

**Streaming.** `GET /api/library/tracks/:id/stream?quality=...` supports `original` (the file as
is, with range requests for seeking), `high` (MP3 320 kbps), `medium` (MP3 192 kbps) and `low`
(MP3 128 kbps). Transcodes need `ffmpeg` on the `PATH` (the Docker image
bundles it) and are cached under `KIMA_DATA_DIR/transcode`, up to
`KIMA_TRANSCODE_CACHE_MB`.

**Needs attention.** Settings > Library Tools > Needs attention (admin) lists albums whose
details were guessed from folder names or look wrong: missing artist, album or year,
conflicting years, missing or duplicate track numbers.

### Organizer

Settings > Library Tools > Organize library (admin) previews and applies a clean layout:
`Album artist/Year - Album/NN - Title.ext` (multi-disc `D-NN`, compilations under
`Various Artists`). Nothing is changed until you apply a previewed run, files are never
overwritten, and every step is journaled in Postgres, so a run resumes after a crash.

- **Copy** (default, safe): writes the organized tree to `KIMA_ORGANIZE_TARGET` and leaves the
  originals alone. Mount a writable folder for it, for example uncomment the `/organized`
  volume in `docker-compose.yml` and set `KIMA_ORGANIZE_TARGET=/organized`. Without it, copy
  mode is unavailable. Once happy, use **Switch library to the organized copy** to make that
  folder the library root; plays and playlists carry over. A copy that has not been switched
  to can be undone (the copies are deleted).
- **Move**: renames files inside `MUSIC_PATH`, which must be mounted writable (drop `:ro`).
  Undo restores every original path from the journal.

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
   Use two terminals. In the first, start the server and leave it running:
   ```bash
   set -a; source .env; set +a; PORT=3111 cargo run -p kima-server
   ```
   In the second, start the frontend:
   ```bash
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
| `KIMA_WATCH_MODE` | `auto` | Library watching: `auto` (native events, fall back to polling), `poll` or `off`. |
| `KIMA_WATCH_POLL_SECS` | `60` | Poll interval in seconds (must be > 0) for `poll` mode and the `auto` fallback. |
| `KIMA_ORGANIZE_TARGET` | unset | Writable directory for the organizer's copy mode. Unset means copy mode is unavailable. |
| `KIMA_TRANSCODE_CACHE_MB` | `2048` | Size cap of the transcode cache under `KIMA_DATA_DIR`, in MB. |

Keep `secrets.json` (or the two secret variables) safe: losing it makes stored encrypted settings
unreadable. A corrupt file stops startup rather than being replaced.

## Differences from upstream

Every place where kima-rs differs from upstream Kima is logged in
[`docs/DIVERGENCES.md`](docs/DIVERGENCES.md), with the reason and a status, so each can be revisited.
