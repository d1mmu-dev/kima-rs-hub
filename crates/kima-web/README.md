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

## What the slice contains (M9 Tasks 4, 5, 6-reduced, 7, 8, 9, 10, 11, 14)

Routes: `/login`, `/setup`, `/collection` (artists), `/collection/albums[?artist=]`,
`/collection/tracks[?album=]`, `NotFound`. Search, playlists and settings from the plan's table are not
built yet (they fall under `NotFound`). Session token: `localStorage` key `kima-web.token` (the legacy
Next.js app uses `auth_token`). Player: persistent bar, `<audio>` on the existing stream endpoint
(`?token=`), queue persisted through `/api/playback-state`; the backend does not store the in-track position
for web saves, so a reload resumes at the start of the saved track.

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
