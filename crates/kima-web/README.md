# kima-web

Rust/WASM frontend for Kima, built on [vertigo](https://github.com/vertigo-web/vertigo) and served via
server-side rendering from `kima-server`.

**kima-web is the only frontend.** Next.js is in no production, Docker, dev-loop or CI path. The `frontend/`
directory (the upstream Next.js app, v1.11.0) stays in the repository only as the visual/functional reference
for the comparison tooling (`tools/run-kima-web-visual.sh` installs it in a scratch copy) until M10 deletes it.
The Docker image builds kima-web with `vertigo build kima-web --dest-dir /srv/kima/web` and sets
`KIMA_WEB_BUILD_DIR=/srv/kima/web`, so the embedded SSR is on by default there.

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
  hidden playlists are not in the UI (the detail page can toggle public/private).
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

## Podcasts (M6 Phase G)

`/podcasts` (home), `/podcasts/show?id=` (followed show), `/podcasts/show?itunesId=` (Apple preview) and
`/podcasts/category?id=` (a category's chart); episodes play in the shared player and save their progress. Differences
from upstream (category links instead of per-category shelves, search results inline, no sort selects yet, episodes not
restored after a reload) are listed in `docs/DIVERGENCES.md` D-215. Not yet visually compared.

**Compromises on the podcast pages (M6a close-out, 2026-11-14)**, each also in D-215 or D-207:

- Not measured with `tools/compare-kima-web-visual.mjs`: the podcast pages have no diff/ink/SSIM numbers yet, so their
  look is unverified against v1.12.3 (the legacy reference itself was only smoke-tested against this backend: home,
  category, preview, follow, show page, play with a ranged seek, next episode, unfollow, no failing `/api/podcasts*`).
  **Deferred to M10 visual-parity work** (M6 plan Definition of done 8), not a styling gap fixed in M6a.
- Home: top categories are links to their chart page, not one shelf each (each shelf would be one Apple request through
  the 1-per-3-s limiter); search results render inline under the box, not in a floating dropdown.
- Category page: 24 shows per page with a "more shows" button instead of loading on scroll; no hidden-AI count (the
  chart answer is a bare list).
- Show page: no sort or page-size selects (newest/oldest button, 20 per page); feed descriptions as plain text; a manual
  refresh does not reload the episode list.
- Player: an episode stops at its end — no automatic or "next episode" step to the show's following episode (upstream
  `podcast-next.spec.ts`; the server's `/next` route exists and is tested); an episode is not written to
  `/api/playback-state`, so a reload does not restore it (its progress is kept); switching away or signing out can
  lose up to 15 s of progress.
- Badges: cards name a known AI/human label in their subtitle (no badge slot); no separate explicit ("E") badge.

**AI-content labels (kima-only, M6 Task 8, D-200/D-207).** The show and preview heroes and every episode row carry a badge:
"AI", "Human" (tooltip: declared by the publisher for the show / for this episode, or confirmed by an admin) or "Unlabelled"
(no disclosure; never treated as human). Lists hide AI-labelled shows and episodes; each section says how many it hides, and
a pill (`name="ai-pill"`, on the home, category and show pages) shows or hides them for that view. The pill starts from the
persistent switch in Settings ("Show AI-generated podcasts", `podcastAiContent`). Admins get "mark as AI / confirm human /
clear label" buttons for the show and for each episode; a change shows at once (an episode marked AI leaves the list while
hiding) and the episode page is fetched again, so episodes inheriting a changed show label update too. Paged lists (a show's
episodes, a category's chart, continue listening / new episodes) are fetched with the pill's `?ai=hide|show` and fetched again
when it flips, so the server filters before paging (full pages, a true "x–y of N"); their hidden count is the body's
`aiHidden`, because vertigo 0.13's fetch API exposes no response headers (`X-Kima-Ai-Hidden` cannot be read). Short unpaged
lists (your shows, for-you, similar, search, the home page's top shows) are fetched with `?ai=show` and filtered on the page.
Lists and the pill render only once the Settings value is known (no flash of the hidden view), and the pill keeps its
position when the page re-renders. Compromises: the category chart's answer is a bare list (upstream shape), so that page
shows no hidden count; media cards have no badge slot, so cards name a known label in their subtitle ("Host · AI") and say
nothing for unknown; there is no separate explicit ("E") badge because upstream's wire shapes carry no `explicit` key
(plan S4 lists one; recorded in D-207).

## Visual parity: compromises and assets

kima-web does **not** reproduce the upstream UI as a finished goal. The target is the upstream kima-hub **v1.12.3** UI,
reached after the M-SYNC milestone. The only comparison available today is against the legacy Next.js frontend **v1.11.0**
(`frontend/`, a temporary reference used only by the compare tooling in a scratch copy until M10 deletes it; Next.js is not
part of any product, Docker or CI path). Treat the numbers below as a snapshot against v1.11.0, not as a goal for the next
round.

`tools/run-kima-web-visual.sh compare` runs `node tools/gen-kima-web-css.mjs --check`, a real `vertigo build` and a
server restart, then screenshots kima-web and the legacy app (same backend, same seeded library) at 1440x900, 1024x768 and
390x844 and writes `<page>-<viewport>-{legacy,web,diff}.png` under `target/kima-web-visual/`.

**Why the old numbers were wrong.** The previous tool reported the share of pixels with a channel delta above 24. A
dark UI is mostly flat background, so a page without any utility class scored 2-5% (measured: unstyled login 2.5% against
2.3% for the styled one). The tool now also reports, per page and viewport: **ink** (share of *foreground* pixels, i.e. not
the page's dominant colour in either image, that differ), **SSIM** (mean over non-flat 8x8 luminance blocks), and a DOM
**style probe** (web font actually loaded, at least 100 class rules, no default-styled buttons/links/inputs, body
background set). A page fails on any of: diff > 1%, ink > 8%, SSIM < 0.93, a probe problem. `--negative-control` strips
all class-selector rules from kima-web and requires every page to be detected (done for login, albums, tracks, settings:
all detected, the unstyled albums page looked unstyled in the screenshot). The probe found a real bug on the way: the
server's CSP had `font-src 'self'`, which silently dropped kima-web's embedded Montserrat (data: URI), so every page
used a fallback font; fixed (`font-src 'self' data:`, tested). The e2e now also runs `--check` and asserts the login page
is styled with the font loaded. A second real bug: Tailwind's `max-[1024px]` is false at exactly 1024px, so the desktop
sidebar and player bar appeared on 1024px tablets; replaced by the `tablet` variant.

**Measured table** (diff% / ink% / SSIM; legacy v1.11.0; images inspected by eye for every row):

| page | 1440x900 | 1024x768 | 390x844 |
|---|---|---|---|
| login | 0.00 / 0.0 / 1.00 | 0.00 / 0.0 / 1.00 | 0.00 / 0.0 / 1.00 |
| setup (status mocked to a fresh install) | 8.38 / 32.4 / 0.14 | 13.87 / 39.8 / 0.16 | 16.59 / 51.5 / 0.16 |
| artists | 0.67 / 8.6 / 0.78 | 0.52 / 4.2 / 0.90 | 1.03 / 6.2 / 0.89 |
| albums | 1.19 / 4.9 / 0.84 | 1.36 / 4.0 / 0.92 | 0.81 / 1.7 / 0.96 |
| tracks | 0.80 / 9.0 / 0.86 | 0.64 / 4.7 / 0.94 | 1.74 / 9.2 / 0.88 |
| search (query "blues") | 1.64 / 30.6 / 0.52 | 1.94 / 23.5 / 0.56 | 3.93 / 41.9 / 0.46 |
| playlists | 0.67 / 9.3 / 0.77 | 0.48 / 4.3 / 0.89 | 0.89 / 5.9 / 0.90 |
| playlist | 0.66 / 9.1 / 0.80 | 0.40 / 3.4 / 0.94 | 1.31 / 9.0 / 0.87 |
| settings | 1.40 / 23.6 / 0.59 | 1.43 / 16.5 / 0.64 | 3.35 / 43.5 / 0.53 |
| player (first track loaded) | 1.12 / 7.8 / 0.83 | 0.57 / 1.8 / 0.94 | 1.59 / 4.8 / 0.90 |

Before this round (same tool, new metrics, 1440x900): playlists 6.1 diff / 60 ink, playlist 6.0 / 60, settings 3.6 / 20,
login 2.8 / 35; the 1024px column was 5-28% (the breakpoint bug). **Only login is under every bound; the rest are not
under 1%/8%/0.93 everywhere.** What remains is explained by the exclusions below, and by nothing else that I found in the
diff images. `ssr->hydrated` in the compare output compares JavaScript-disabled output of an *anonymous* request (the
login shell for guarded pages) with the hydrated page, so it is not a layout-shift measure for authenticated pages
(e.g. albums shows 17-34% for that reason); no authenticated SSR comparison exists.

### Deferred to M10 (not a styling gap)

The owner placed these outside M9; they are omitted, not stubbed:

- Sidebar entries queue, vibe, browse, stats (kima-web has collection / search / playlists instead; the playlist stack
  below is therefore ~94px higher). This is the largest block of diff in every desktop table row.
- Player bar: "match this vibe" and "show lyrics" buttons (the transport group is ~32px narrower), keyboard-shortcut
  info button, expand-to-fullscreen button (the volume slider is longer: 150px against 40px).
- Header: bell (notifications) and the version badge shows kima-web's `v0.1.0`, legacy shows its own `v1.11.0`.
- Settings: every section except Account (native apps, playback, scrobbling, admin sections), two-factor, the mobile
  "jump to section" select; setup: the second wizard step "integrations" and the step indicator (about 46px
  shift of everything below the heading); search: the "all results / library / discover" tabs and the soulseek tip.
- Playlists: Import URL, Import File, Browse, share links, hide, reordering (move up/down/drag), pending/failed
  tracks. Albums/tracks/artists: delete buttons and the "Recommended" filter of Refine (the backend has no delete
  routes for library items and no discovery source in kima-rs).

### Differences that are M9 choices

- Create-playlist is an inline panel without the public/private toggle; the playlist detail has a public/private
  toggle instead. Delete uses a dialog (two-step, as before). The password form does not close itself after a
  success and its button is not disabled while the fields are incomplete (so the validation messages can show).
- Refine's sort value is kept across tabs; a sort a tab does not offer (e.g. "year" on tracks) falls back to name on the
  server and no option is highlighted.
- Cover art, generated gradients and font anti-aliasing differ by a few pixels; text is lowercase through the copied
  `body { text-transform: lowercase }`.
- Styles are a precompiled Tailwind v4 sheet (`assets/kima-base.css` -> `assets/kima.css`, regenerated by
  `tools/gen-kima-web-css.mjs`; **run it whenever classes in `src/` change; `--check` runs in the compare script, the
  e2e and CI (kima-web job, with a scratch install of the three pinned npm packages; Node is used only there)**), not `css!` macros. Icons are a fixed subset of lucide (`src/icons_data.rs`; add a name to
  `tools/gen-kima-web-icons.mjs`).
- The player queue is persisted through `/api/playback-state` (now including shuffle); repeat and the sleep timer are
  client-side only; the in-track position is stored in `localStorage` (`kima-web.position`).

Licences of the copied assets are in `assets/NOTICE.md` (Montserrat: OFL 1.1; icons: lucide ISC; logo: project GPL-3.0).

The e2e (`tools/verify-kima-web-e2e.mjs`) turns `text-transform` off in the page it drives so it can compare the
real text, and finds controls by `aria-label` (`Play`, `Pause`, `Next track`, `Sync library`, `Logout`).
