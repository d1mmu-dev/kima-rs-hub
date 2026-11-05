# kima-web

Rust/WASM frontend for Kima, built on [vertigo](https://github.com/vertigo-web/vertigo) and served via
server-side rendering from `kima-server`.

**Toolchain (confirmed in M9 Task 0, not assumed).** Target: `wasm32-unknown-unknown`
(`rustup target add wasm32-unknown-unknown`). The plan named vertigo 0.6.3 and a nightly requirement; both
turned out stale: `vertigo-macro` 0.6.x/0.6.4 fails on current nightly (`Span::source_file` was removed from
`proc_macro`) and `vertigo-cli` < 0.12 has no library target, which the in-process SSR design (spec §3) needs.
vertigo/vertigo-macro/vertigo-cli **0.13.4** build for `wasm32-unknown-unknown` on both stable and
nightly (verified with a scratch crate on `cargo +stable` and the default nightly 1.96), so no nightly is
required and the workspace toolchain is not changed. `vertigo build` needs `wasm-opt` (binaryen) on `PATH`:
Arch `pacman -S binaryen`, or without root download a release from
<https://github.com/WebAssembly/binaryen/releases> and put `bin/wasm-opt` on `PATH`. Check the whole setup with
`tools/verify-wasm-toolchain.sh`. Install the CLI matching the library version:
`cargo install vertigo-cli --version 0.13.4 --locked`.

**Build.** `tools/verify-kima-web-build.sh` runs `vertigo build` in this crate and checks the output in `build/`
(gitignored): `index.json` (manifest; `vertigo serve` renders the HTML from it), a hashed `kima_web.<hash>.wasm`
and a hashed `wasm_run.<hash>.js`. vertigo 0.13 writes no static `index.html` and no unhashed `wasm_run.js`.
Set `VERTIGO=/path/to/vertigo` to pick a CLI that is not first on `PATH`; keep it at the same version as the
`vertigo` crate (`=0.13.4` in `Cargo.toml`).

## Run it locally (dev)

`vertigo build` shells out to `rustup` and `wasm-opt`, so both must be on `PATH`.

```sh
MUSIC_PATH=$HOME/Music VERTIGO=~/.local/opt/vertigo-cli-0.13.4/bin/vertigo tools/run-kima-web-dev.sh
# then open http://127.0.0.1:4444
```

`tools/run-kima-web-dev.sh` starts `kima-server` and the vertigo dev server (`vertigo serve --proxy
/api=http://127.0.0.1:$KIMA_PORT/api`), so the browser sees one origin and `/api/*` (including the
`/api/library/tracks/:id/stream?token=` audio requests) is proxied to kima-server. `--watch` uses
`vertigo watch` (debug wasm, rebuild on change) instead of build + serve.

- **Library location** is the server's `MUSIC_PATH` env var (kima-server default `/music`); the script
  requires it explicitly and passes it through. The UI has no setting for it. kima-server scans the folder at
  start, and the **Scan library** button (admin, `POST /api/library/scan`) rescans.
- **Database**: set `DATABASE_URL` yourself, or leave it unset and the script starts/reuses a dedicated
  Docker postgres: container `kima-web-dev-postgres`, label `kima.run=kima-web-dev`, named volume
  `kima-web-dev-pgdata`, `127.0.0.1:55433` (`KIMA_WEB_DEV_PG_PORT`). It never falls back to the 5456 dev DB.
  Remove it with `docker rm -f kima-web-dev-postgres && docker volume rm kima-web-dev-pgdata`.
- Other env: `KIMA_DATA_DIR` (default `~/.local/share/kima-web-dev`), `KIMA_PORT` (3030), `WEB_PORT` (4444),
  `KIMA_IDENTIFY` (default `off`: no MusicBrainz/AcoustID calls in dev).
- First run: open the page; with no account yet `/login` sends you to `/setup` (create the admin, you are
  signed in and onboarding is marked complete); the library is then browsable and playable.

## Embedded SSR (the production shape, M9 Task 3)

`kima-server` links `vertigo-cli` as a library and runs `vertigo_cli::serve::run` in-process (wasmtime executes
the `kima-web` wasm for every server-rendered page). That server binds **`127.0.0.1` only**; `kima-server`
reverse-proxies the `kima-web` routes to it, so the browser sees one origin: `kima-server`'s.

| Env | Meaning |
|---|---|
| `KIMA_WEB_BUILD_DIR` | The `vertigo build` output (`index.json`, `kima_web.<hash>.wasm`, `wasm_run.<hash>.js`). **Setting it turns the embedded SSR and the proxy on; unset (the default) means neither**, and `kima-server` starts without any kima-web build output, exactly as before. |
| `KIMA_WEB_SSR_PORT` | Loopback port of the embedded server (default `3031`; 1-65535). |

vertigo serves the build directory under the path it is given, so the public asset URLs contain
`KIMA_WEB_BUILD_DIR` as written (an absolute directory shows up in the page's `<script src>`; harmless, but
the URL layout follows the setting). Relative fetches made while rendering resolve against `kima-server`
itself (`http://127.0.0.1:$PORT`), so `/api/...` reaches the real API.

**Proxy allow-list** (`kima_server::web_proxy`): `GET`/`HEAD` of `/` (exactly), `/login`, `/setup`,
`/collection`, `/search`, `/playlists`, `/settings` and anything below those (whole path segments:
`/collection/albums` yes, `/loginx` no), plus the exact public paths of the two build files `index.json`
names. Everything else (`/api`, `/rest`, `/podcasts`, other files of the build directory, non-`GET` methods,
unknown paths) goes to `kima-server`'s normal routing and 404 and never reaches the SSR server; this is
tested (`crates/kima-server/tests/web_ssr_embed.rs`). A page task adds its route to the one constant
`WEB_ROUTES`. When the embedded SSR is on, a kima-web route wins over the legacy export (`KIMA_FRONTEND_DIR`)
for the same path (`/`, `/setup`, ...); the rest of that export keeps being served.

**Liveness: crash and restart.** `kima-server` starts the SSR server first and waits until it accepts
connections (the first start compiles the wasm: seconds) before it opens its own port; a bad build
directory or a busy SSR port fails the start. Afterwards, if the SSR task panics, returns an error (a failed
bind included) or just returns without a shutdown having been requested, `kima-server` stops serving, drains
(at most 10 s) and **exits non-zero**, so a container restart policy recovers it. There is no mode that keeps
the API up while `kima-web` answers 5xx. Tests: `a_panicking_ssr_task_...`, `a_failing_ssr_task_...`,
`an_ssr_task_that_just_returns_...`, `the_process_exits_non_zero_when_the_ssr_server_cannot_start` (real binary).

**Try it.** `MUSIC_PATH=$HOME/Music VERTIGO=~/.local/opt/vertigo-cli-0.13.4/bin/vertigo
tools/run-kima-web-dev.sh --ssr` builds kima-web, starts `kima-server` with the embedded SSR and serves
<http://127.0.0.1:3030/> (single origin; no vertigo dev server; `SSR_PORT` picks the loopback port). Check it
with `curl -s http://127.0.0.1:3030/login` (the sign-in form is in the markup) and
`tools/verify-kima-web-e2e.mjs --ssr`, which targets that origin and adds checks that pages are server-rendered
and that only kima-web's routes are proxied. The dev flow below (`vertigo serve` + `/api` proxy) is unchanged.

**Cost.** `vertigo-cli` pulls wasmtime/cranelift and actix (it is an actix-web server, not axum 0.6/poem as
the plan assumed; kima-server's axum 0.8 stack is independent of it): +169 crates in `Cargo.lock`, about one
extra minute of cold build time, a much larger binary (release `kima`: 41.8 MB to 71.9 MB). Licences: vertigo,
vertigo-cli and vertigo-macro are `MIT OR Apache-2.0`; wasmtime/cranelift are `Apache-2.0 WITH
LLVM-exception`; the new crates are otherwise MIT/Apache/BSD/Unlicense/Zlib/CDLA-Permissive-2.0 (the two
`ittapi` crates are `GPL-2.0-only OR BSD-3-Clause`: the BSD-3-Clause option applies). All can be combined into a
GPL-3.0-only binary (Apache-2.0 is GPLv3-compatible). `actix-proxy`/`actix-tls` link the system OpenSSL
(`libssl.so.3`), so the runtime image needs `libssl3` (the pgvector/Debian bookworm image has it).

## What kima-web contains (M9 Tasks 4-15)

Routes: `/login`, `/setup`, `/collection` (artists), `/collection/albums[?artist=]`,
`/collection/tracks[?album=]`, `/search`, `/playlists`, `/playlists/<uuid>`, `/settings`, `NotFound`. Session
token: `localStorage` key `kima-web.token` (the legacy Next.js app uses `auth_token`). Player: persistent bar,
`<audio>` on the existing stream endpoint (`?token=`), queue persisted through `/api/playback-state`.

- **Search** (`pages/search.rs`): query box (300 ms debounce, Enter searches at once) over `GET /api/search`,
  results grouped artists / albums / tracks. An empty or blank query shows a hint and sends no request. The
  query is page state, not part of the URL. Track results play (queue = the visible tracks) and can be added
  to a playlist.
- **Playlists** (`pages/playlists/`): list with a create form (creating opens the new playlist), detail with
  play all, rename, two-step delete and per-track remove. Every change refetches the playlist (no optimistic
  edits). Every track list (tracks, search, playlist) has a "＋ Playlist" picker (`tracklist.rs`); adding a
  track that is already there answers 200 with `duplicated`, shown as "Already in <name>". Reordering and
  public/hidden playlists are not in the UI.
- **Settings** (`pages/settings.rs`, spec §5.7 account slice only): username and role from `GET /auth/me`, and
  a password change (`POST /auth/change-password`). The backend revokes the old tokens on a change and returns
  a new access token, which replaces the stored session; the audio element of a track that is already playing
  keeps the old `?token=` until the next track loads.
- **Resume position** (`position.rs`): `POST /api/playback-state` stores the queue, index, track id and
  shuffle, but never the in-track position for web saves (`position` is written only by the Subsonic queue, as
  upstream), so `GET /api/playback-state` reports `position: 0`. kima-web keeps the position itself under
  `localStorage` key `kima-web.position` as `{"id": <track id>, "t": <seconds>}`, written about once a second
  while the position moves (not below 1 s), and applies it after a reload only when `id` is the track the
  restored queue points at and it is not within the last second. It is applied once the audio metadata is
  loaded (setting `currentTime` on a fresh element earlier did not stick in Chromium). The backend is
  unchanged.

Deviations from the plan text, all forced by the real vertigo 0.13.4 API:

- Fetch bodies are `JsJson`, not text: `api::codec` bridges `serde_json` and `JsJson` (the plan's
  `RequestBody::Text` does not exist in 0.13).
- vertigo's fetch mock (`ApiFetch::set_mock_handler`) is `#[cfg(test)]` inside the vertigo crate, and vertigo's
  DOM test harness is not exported, so native `cargo test -p kima-web` covers the pure logic only (codec with
  the real fixtures, response decoding, request building, routing, guard decision, session storage, form
  validation, queue/paging helpers). Fetching, rendering and playback are verified in a real browser
  (`tools/verify-kima-web-e2e.mjs`).
- The dev server runs with hydration enabled; pages render only a shell on the server (no fetches, no
  `localStorage`) and the browser draws the real page.
- The login page shows the backend's `error` text; the `Retry-After` header of a 429 is not visible through
  vertigo's fetch API, so only the message is shown.
- `kima-api` route modules still build their `json!` literals; they are not migrated to the new
  `kima-api-types::library` / `playback_state` DTOs yet. The fixture round-trip tests are the drift guard.

## End-to-end check

`tools/verify-kima-web-e2e.mjs` drives headless Chromium over CDP against a running stack (see the header of
the file): login, browse, playback, seek, resume position after a reload, search (debounce, play), playlists
(create, add, play, rename, remove, delete), password change and re-login, scan, 401 handling. Chromium and
its remote-debugging port are `CHROME` and `DEBUG_PORT`. The run changes the admin password through the
Settings page and changes it back, so it can be repeated.
