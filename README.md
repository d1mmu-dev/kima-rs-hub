# kima-rs-hub

This project is a fork of [Chevron7Locked/kima-hub](https://github.com/Chevron7Locked/kima-hub),
ported to the Rust language.

Kima is a self-hosted music server. kima-rs replaces the Node/TypeScript backend and the Python analyzers with a single Rust
`kima` binary plus Postgres, and replaces the upstream Next.js frontend with `kima-web`, a Rust/WASM
frontend (vertigo) that `kima` renders on the server. `kima-web` is the only frontend: no Next.js or Node
runs in the Docker image, the production server or CI. The `frontend/` directory (the upstream Next.js app)
is a temporary visual/functional reference for the comparison tooling (`tools/run-kima-web-visual.sh`) and is
deleted in M10. The target look is the upstream kima-hub v1.12.3 UI (after the M-SYNC milestone); `kima-web`
does not match it yet, see [`crates/kima-web/README.md`](crates/kima-web/README.md). Every difference from upstream is logged in
[`docs/DIVERGENCES.md`](docs/DIVERGENCES.md). Licensed under GPL-3.0-only, like upstream.

## Status

Milestones M1 (library and playback) and M2 (Subsonic) are done. Most M-SYNC backend work
through Task 18 is implemented locally, but M-SYNC is not accepted: Task 15's re-file route,
the legacy frontend refresh (Task 19), runtime/upgrade checks and hosted CI remain open;
see the [roadmap](docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md). On top of M0's accounts and
login, kima-rs now scans and watches a music folder, shows it as Artist, then Year - Album, then songs, and lets you
browse, search, stream (with transcodes), keep playlists and resume playback. An admin can also
preview and apply a tidy on-disk layout. Music apps that speak Subsonic/OpenSubsonic can connect
with per-app tokens (see [`docs/subsonic.md`](docs/subsonic.md)). Identification and the album
autotagger and CLAP/analysis plumbing are built; private-model inference acceptance, podcasts,
audiobooks and integrations are still open. See
`docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` for what comes next.

## Quick start

```bash
docker compose up
```

Then open <http://localhost:3030> and register. The first account becomes the admin. The image
bundles Postgres, `ffmpeg` and the `kima` binary; state lives in the `kima_data` volume (`/data`).
The bundled Postgres gets a random password on first start (`/data/postgres-password`, readable by
root only) and kima connects as an unprivileged `kima` role. Put your
music in `./music` or set `MUSIC_DIR` to its location (mounted read-write at `/music`; embeddings keep a private snapshot directory `/music/.kima-snapshots` on the same mount, see [docs/m4-input-snapshots.md](docs/m4-input-snapshots.md)); the `.kima-snapshots` directory is visible in the library on the host. A read-only library cannot hold the snapshots: either disable embeddings (`DISABLE_CLAP=true`) or use a separate read-write mount with reflink support (Btrfs, XFS `reflink=1`) for the library and the snapshot directory.

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
- Files that disappear are flagged missing and retired only after 7 days. A mass-missing guard
  (at least 10 missing tracks and more than 10% of the library) prevents retirement; when a
  same-slot copy exists, history is handed over. `GET /api/library/removals` is an admin-only,
  newest-first audit of retired tracks (`count`, `removals`; optional `limit` and `offset`).
  Restoring a file before retirement clears its missing flag. There is no `RECONCILE_MODE`
  switch as upstream has: the 7-day grace is fixed, and `KIMA_WATCH_MODE=off` stops the
  event-driven scans (D-161).
- `POST /api/library/scan` (admin) requests a normal reconcile, which skips files whose size and
  modification time are unchanged; it answers with the `jobId` that
  `GET /api/library/scan/status/:jobId` reports on.
- `POST /api/library/refile` (admin) queues a one-off re-file: a forced full reconcile that
  re-reads every present file, unchanged ones included, and files each album again under the
  artist elected from all its songs. It answers 202 `{"message":"Re-filing albums by their tags"}`,
  or 409 while a re-file is already queued or running. Albums or artists a person edited and
  folders with an unreadable file or disc subfolder are left as they are; row ids, plays and
  playlists are kept. The outcome is logged (`library re-file finished`), not returned (D-168).

**Streaming.** `GET /api/library/tracks/:id/stream?quality=...` supports `original` (the file as
is, with range requests for seeking), `high` (MP3 320 kbps), `medium` (MP3 192 kbps) and `low`
(MP3 128 kbps). Transcodes need `ffmpeg` on the `PATH` (the Docker image
bundles it) and are cached under `KIMA_DATA_DIR/transcode`, up to
`KIMA_TRANSCODE_CACHE_MB`.

**Needs attention.** Settings > Library Tools > Needs attention (admin) lists albums whose
details were guessed from folder names or look wrong: missing artist, album or year,
conflicting years, missing or duplicate track numbers, and tracks reached through symlinks.

### Organizer

Settings > Library Tools > Organize library (admin) previews and applies a clean layout:
`Album artist/Year - Album/NN - Title.ext` (multi-disc `D-NN`, compilations under
`Various Artists`). Nothing is changed until you apply a previewed run, files are never
overwritten, and every step is journaled in Postgres, so a run resumes after a crash.
Previews may cover selected albums, and are grouped by album. `.lrc` and `.nfo` sidecars
travel with their album; a scoped move leaves sidecars in a folder shared with an out-of-scope album.

- **Copy** (default, safe): writes the organized tree to `KIMA_ORGANIZE_TARGET` and leaves the
  originals alone. Mount a writable folder for it, for example uncomment the `/organized`
  volume in `docker-compose.yml` and set `KIMA_ORGANIZE_TARGET=/organized`. Without it, copy
  mode is unavailable. Once happy, use **Switch library to the organized copy** to make that
  folder the library root; plays and playlists carry over. A copy that has not been switched
  to can be undone (the copies are deleted).
- **Move**: renames files inside `MUSIC_PATH`, which must be mounted writable (the default compose file already does).
  Undo restores every original path from the journal.

### Autotagger

Settings > Library Tools > Match albums lets an admin search MusicBrainz for album releases,
review stored candidates, track mappings and proposed changes, and apply a match. Stored snapshots
can be applied or undone even with `KIMA_IDENTIFY=off`; searching needs identification enabled.
Choose a target:

- **Database (`db`)** stores overrides without changing music files, including on a read-only library.
- **Files (`files`)** writes tags to the library only when it is writable and the files are supported;
  optionally reorganize the album after tagging.
- **Copy (`copy`)** writes tags only to a writable organized copy, after a qualifying whole-library
  copy run covers the album. The original library files are not written.

The default read-only mount therefore offers database-only and, after a qualifying copy run, copy;
mount the library writable to use files. Writes are journaled and checked to preserve audio; unsupported
or unsafe files are refused. Undo restores the previous values where safe, but can be refused if files
changed or identities collide. Resolve such collisions before retrying; no accept-as-is operation is
currently provided. Automatic application of strong matches (`autotagAutoApply`) is off by default;
when enabled it targets `db` or writable `files`, never reorganizing automatically.

### Identification

Once a library is scanned, kima identifies it on its own, with no button to press: each track's
MusicBrainz recording (from file tags first, then an AcoustID fingerprint match), then
artist photos, biographies, similar artists and genres, album cover fallbacks and synced lyrics.
Progress shows under Settings > Library Tools; new files are picked up as they arrive. Identification
itself only fills the database; the separate autotagger can write tags when explicitly configured.

Admins can `POST /api/enrichment/rerun` with `{"step":"identification"}` or
`{"step":"artists"}` to reset that step and its dependent answers; the reply includes
`artists`, `tracks`, `vectors` and `message`. The other recognized steps (`analysis`,
`vectors`, `moods`, `similarity`) currently return 409 without a reset; an unknown step returns
400. This is a partial implementation of the upstream six-step route (D-112).

- **Switch it off.** `KIMA_IDENTIFY=off` stops every outbound lookup for the whole server. An admin can
  also turn "Auto enrich metadata" off in Settings > Artwork; the environment switch wins and
  the toggle then says so.
- **AcoustID key.** No AcoustID key ships with kima-rs. Register an application at
  <https://acoustid.org/new-application> and enter the key in Settings > Artwork (or set
  `ACOUSTID_CLIENT_KEY`). Without one, fingerprints are still made but nothing is asked of AcoustID;
  tracks with usable tags are matched by MusicBrainz id or name. A fanart.tv key is optional and entered
  the same way (or `FANART_API_KEY`). Saved keys are never shown again; the page says "Saved".
- **First run on a big library is slow.** MusicBrainz is asked at most once every 1.1 seconds, and
  an untagged track takes one to three requests: about 6-18 hours for 20 000 untagged tracks.
  Tagged files cost nothing, and later runs reuse a cache. Progress survives restarts. For a large library
  point `MUSICBRAINZ_BASE_URL` at a local MusicBrainz mirror.
- Fingerprinting uses half the CPU cores by default; set `KIMA_FINGERPRINT_WORKERS` to change that.

## Local development

Requirements: Rust 1.85+ with the `wasm32-unknown-unknown` target, `vertigo-cli` 0.13.4 and `wasm-opt` (for
`kima-web`, see [`crates/kima-web/README.md`](crates/kima-web/README.md)), Docker (for the dev database). Node.js is
needed only for the stale-CSS check and the visual comparison tooling, never to run or build the product. Building or testing `kima-ml` (and `kima-ident`, which links it) needs libsoxr development files (Debian/Ubuntu: `libsoxr-dev`; Arch: `soxr`).

libsoxr is LGPL-2.1-or-later; see [upstream source and license](https://sourceforge.net/projects/soxr/) and the license notices shipped by the OS package. Distributors must provide the applicable notices and corresponding library source and preserve users' ability to replace the shared library, among other LGPL obligations. The Dockerfile prepares for this dependency by installing Debian's shared library package; the resulting image has not been verified here. Check the exact distribution and its obligations separately; this is not legal advice.

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
4. Frontend dev loop (`kima-web`): `MUSIC_PATH=$HOME/Music tools/run-kima-web-dev.sh` starts the server and the
   vertigo dev server on <http://127.0.0.1:4444> (`/api` is proxied to the server); `--ssr` instead serves the
   production shape (embedded server-side rendering) on the server's own port. Details and env vars are in
   [`crates/kima-web/README.md`](crates/kima-web/README.md).

Checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`; for `kima-web` also
`cargo clippy -p kima-web --target wasm32-unknown-unknown -- -D warnings` and `cargo test -p kima-web`.
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
| `KIMA_WEB_BUILD_DIR` | unset (set to `/srv/kima/web` in the Docker image) | `vertigo build` output of `kima-web`. Setting it turns on the embedded server-side rendering of the web UI; unset means the server serves the API only. |
| `KIMA_WEB_SSR_PORT` | `3031` | Loopback port of the embedded SSR server (never exposed). |
| `KIMA_FRONTEND_DIR` | unset | Directory of a legacy static export (Next.js era). Not set in the image and no longer produced by any build; removed with `frontend/` in M10. |
| `KIMA_BIND_HOST` | `0.0.0.0` | Address to bind. |
| `KIMA_WATCH_MODE` | `auto` | Library watching: `auto` (native events, fall back to polling), `poll` or `off`. |
| `KIMA_WATCH_POLL_SECS` | `60` | Poll interval in seconds (must be > 0) for `poll` mode and the `auto` fallback. |
| `KIMA_ORGANIZE_TARGET` | unset | Writable directory for the organizer's copy mode. Unset means copy mode is unavailable. |
| `KIMA_LIBRARY_SYNC_CRON` | `0 4 * * *` | Five-field cron (UTC) of the daily safety-net reconcile. `off` disables it. An invalid expression stops startup. |
| `KIMA_PUBLIC_URL` | unset | Where clients reach this server, such as `https://music.example.com`. Must start with `http://` or `https://`; a trailing `/` is stripped. Used in the `hls` playlist and shown on the settings page. |
| `KIMA_CONTACT` | `https://github.com/d1mmu-dev/kima-rs-hub` | Contact sent in the User-Agent of every outbound request (`kima-rs/<version> ( <contact> )`), as MusicBrainz asks. |
| `KIMA_IDENTIFY` | `on` | `off` turns off every outbound lookup (MusicBrainz, AcoustID, Wikidata, fanart.tv, Deezer, ListenBrainz, Cover Art Archive, LRCLIB) and wins over the Settings toggle; fingerprinting still runs. Any other value stops startup. |
| `MUSICBRAINZ_BASE_URL` | `https://musicbrainz.org/ws/2` | MusicBrainz or a mirror (upstream's name). Must start with `http://` or `https://`; a trailing `/` is stripped. |
| `KIMA_FINGERPRINT_WORKERS` | half the cores, at least 1 | How many files are fingerprinted at once (ffmpeg decodes, chromaprint runs in-process). Must be a whole number of at least 1. |
| `ACOUSTID_CLIENT_KEY` | unset | AcoustID client key (upstream's name), used when no key is stored in Settings. A secret: do not commit it. |
| `FANART_API_KEY` | unset | fanart.tv key (upstream's name), used when no key is stored in Settings. A secret: do not commit it. |
| `ACOUSTID_DISABLED` | `false` | `true` (any case) turns off fingerprinting and AcoustID lookups (upstream's name); `/api/system/features` then answers `fpcalc: false`. |
| `KIMA_NET_DENY_REAL` | unset | `1` makes any outbound request to a host other than this machine panic. Set in CI so no test reaches the network. |
| `KIMA_TRANSCODE_CACHE_MB` | `2048` | Size cap of the transcode cache under `KIMA_DATA_DIR`, in MB. |
| `KIMA_ARTWORK_CACHE_MB` | `512` | Size cap of the cover-art cache under `KIMA_DATA_DIR/artwork`, in MB. The oldest albums are dropped first. `0` means nothing is kept, so every request rebuilds. |

Keep `secrets.json` (or the two secret variables) safe: losing it makes stored encrypted settings
unreadable. A corrupt file stops startup rather than being replaced.

## Differences from upstream

Every place where kima-rs differs from upstream Kima is logged in
[`docs/DIVERGENCES.md`](docs/DIVERGENCES.md), with the reason and a status, so each can be revisited.
