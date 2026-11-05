# M9: Rust/WASM Frontend, Part 1 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `kima-web` is served in production via server-side rendering, through a wasmtime runtime embedded
in-process inside `kima-server` (not a sidecar, not CSR-only) — a new user can open it, create the first admin
account, sign in, browse the library, search, play a track, see/edit a playlist, and change account settings —
all through types shared with the backend via `kima-api-types`. Next.js is fully retired from production; it
exists only as an opt-in `Dockerfile` `ARG`-gated developer build for local comparison during the M10 porting
work. There is no runtime switch and no dual-serve in production (revised 2026-11-05 per owner decision; see the
spec's header and §1/§6 for the full rationale).

**Architecture:** `crates/kima-api-types` (new, dependency-light, native + `wasm32-unknown-unknown`) holds the DTOs
for login/onboarding/library/search/playback-state/playlists/settings; `kima-api` route modules are migrated to use
it in place of locally-defined structs (no wire-format change). `crates/kima-web` (new) is a `vertigo`/`vertigo-cli`
crate: a hand-written route enum (`vertigo::router::Router`), one page module per §5 page, a thin fetch client
(`kima-web::api::client`) that bridges `serde_json` and vertigo's `RequestBuilder`/`RequestBody::Text`. `kima-web`
builds via `vertigo build` to a static `build/` directory. In production, `kima-server` spawns `vertigo_cli::
serve::run()` (verified callable as a library function, not just a CLI — spec §3) as a background `tokio::task`
bound to a loopback-only address, and reverse-proxies the `kima-web` page routes to it so every page's initial HTML
comes from real wasmtime-executed SSR; `/api/*` is untouched. Next.js is not part of production routing at all;
it is included in the image only behind the `KIMA_BUILD_LEGACY_FRONTEND` `Dockerfile` `ARG` and served, if present,
from an isolated `/__legacy/` mount for local dev comparison. Playwright gets a second config/spec-tree for
`kima-web`'s pages, which is the production-defining suite; the existing suite moves to running against the
legacy-flag build only.

**Tech stack:** Rust 2024 for every native crate; `kima-web` on `vertigo`/`vertigo-macro` 0.6.3, built with
`vertigo-cli` 0.6.3, target `wasm32-unknown-unknown` (toolchain requirement confirmed in Task 0, not assumed —
`vertigo`'s tutorial states nightly is required for trait aliases, current as of 0.6.3). Backend unchanged: axum
0.8, sqlx 0.8, tokio, Postgres 17. Frontend tooling unchanged for the existing suite: Next.js 16 static export,
vitest, Playwright; `wasm-opt` (binaryen) required for `vertigo-cli` production builds.

**Spec:** `docs/superpowers/specs/2026-11-04-m9-frontend-vertigo-design.md` (binding; this plan covers that spec in
full — there is no M9b). Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` (row M9). Style and
granularity reference: `docs/superpowers/plans/2026-10-27-m3b-autotagger.md`. Divergence log:
`docs/DIVERGENCES.md` (147 rows exist; D-134 is reserved by the spec §8 for this milestone and is written only by
the final task, once a page genuinely works end to end — check the file's tail before use in case another
in-flight milestone claimed it first).

**Decisions this plan treats as settled (from the spec; re-confirm only if a task finds the ground has shifted):**
1. Type sharing: new `kima-api-types` crate, plain `serde`, no OpenAPI (spec §4).
2. `kima-web` talks to vertigo's fetch layer via raw JSON text (`RequestBody::Text` + `serde_json`), not
   `AutoJsJson` (spec §4.2).
3. **Production serving is real SSR via an embedded wasmtime runtime (spec §3, §6.1)**: `kima-server` runs
   `vertigo_cli::serve::run()` in-process as a background `tokio::task` bound to a loopback-only address, and
   reverse-proxies the `kima-web` page routes to it. This supersedes the original CSR-only/`ServeDir` design;
   there was never a sidecar/separate-process in the shipped design, and vendoring vertigo-cli's private SSR
   internals for a literal single-`Router` embed was investigated and explicitly rejected for M9 (spec §3).
4. **No runtime switch.** Next.js is not served in production at all (no dual-serve). It is available only as an
   opt-in `Dockerfile` `ARG` (`KIMA_BUILD_LEGACY_FRONTEND`) for a local dev-time comparison build, served from an
   isolated `/__legacy/` mount, never overlapping `kima-web`'s routes and never present unless both the build
   `ARG` and a runtime env var are explicitly set (spec §6.2).
5. Playwright: `playwright.kima-web.config.ts` against a normal production-shaped build is the suite CI treats as
   gating production; the existing `kima-rs` suite keeps running but only against a `KIMA_BUILD_LEGACY_FRONTEND`
   build (spec §7).

## Global Constraints

- Everything in M0–M8's Global Constraints still holds for native crates: GPL-3.0-only, edition 2024,
  `rust-version = "1.85"` where applicable (this does not bind `kima-web`, which targets `wasm32-unknown-unknown`
  and may need nightly per Task 0's finding — document whichever toolchain it actually needs in that crate's
  `README.md`, do not silently change the workspace's default toolchain); camelCase JSON on `/api`, errors
  `{"error": "<message>"}` (unchanged by this milestone — `kima-api-types` mirrors it, never redefines it).
- **Before every commit touching a native crate:** `cargo fmt --all`, `cargo clippy --workspace --all-targets --
  -D warnings`, `KIMA_NET_DENY_REAL=1 cargo test --workspace` against the dev Postgres, all clean. Frontend tasks
  that touch `frontend/` run `npx tsc --noEmit`, `npm run test:unit`, `npm run build` in `frontend/` as before.
  Tasks that touch `kima-web` additionally run `cargo build -p kima-web --target wasm32-unknown-unknown` (or
  `vertigo build` once Task 2 wires it) and `cargo test -p kima-web` (native, for the component tests — vertigo's
  own suite runs natively per Task 0's finding).
- **sqlx:** unaffected by this milestone except where Task 1's DTO relocation changes a `query_as!` target type's
  import path (never its shape). Re-run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/` if
  any such touch changes a checked query's output type path. CI stays `SQLX_OFFLINE=true`.
- **Commits:** `dimmu <3417783+d1mmu-dev@users.noreply.github.com>`. **No `Co-Authored-By`, no AI or Claude
  attribution line of any kind.** Conventional Commits style. Do not push without the owner's explicit go-ahead
  (this plan's authoring task does not push either — see Task 0's note).
- **Divergence rows:** D-134 is written **once**, in Task 19 (the last task), after the embedded-SSR production
  path and at least one page are verified working end to end — not speculatively earlier. If any task discovers an
  incidental backend
  behaviour difference while relocating DTOs (Task 1) or while writing a page against the real API (Tasks 8–16),
  that gets its own new row (next free ID at the time, D-135 onward) in the commit that finds it; do not fold it
  into D-134.
- **No test reaches a real network.** Playwright runs with `KIMA_IDENTIFY=off` as the existing suite does.
- **Crate boundaries:** `kima-api-types` depends on nothing that fails to build for `wasm32-unknown-unknown`
  (`serde`, `serde_json` where unavoidable, `uuid` with the `serde` feature, whatever timestamp crate the backend
  already uses) — checked by Task 0's CI matrix addition, not by eyeballing `Cargo.toml`. `kima-web` never depends
  on `kima-api`, `kima-db`, `kima-core`, `sqlx`, `axum` or `tokio`. `kima-api` may depend on `kima-api-types` but
  `kima-api-types` never depends on `kima-api` (one-directional).
- **This is design/spec-and-plan work only where this plan itself is concerned** (it was authored with ZERO
  production `kima-web` code written) — but the plan's own tasks, once executed by a later worker, do write real
  code; that is expected and matches how M3a/M3b's plans were authored before being executed.

## Review Focus

1. **The DTO relocation (Task 1) changing wire behaviour by accident.** A moved struct silently gaining/losing a
   `#[serde(rename_all = "camelCase")]`, an `Option<T>` becoming required, or a `skip_serializing_if` being dropped
   would be a silent breaking change no existing test catches if the fixture tests (Task 1) are shallow. The
   round-trip fixtures must use real captured responses, not hand-typed "looks about right" JSON.
2. **The embedded-SSR reverse proxy (Task 3) serving the wrong thing, or serving nothing, for a route `kima-web`
   does not cover.** Every route outside §5 of the spec must get `kima-server`'s normal 404/existing routes, not an
   accidental proxy-through to the loopback SSR task; checked by a dedicated test (Task 3) before any page work
   starts, so later tasks cannot accidentally depend on a proxy allow-list that swallows unrelated routes.
3. **The embedded SSR task's liveness (Task 3).** If the loopback-bound `vertigo_cli::serve::run()` task dies after
   `kima-server` is already serving traffic, every `kima-web` page breaks (no CSR/static fallback exists under the
   new design) — Task 3's tests must cover at least one "the embedded task is down" case and assert `kima-server`
   degrades predictably (a clear 5xx for `kima-web` routes, `/api` and health checks unaffected), not silently
   hangs or panics the whole process.
4. **The player's `<audio>` element and playback-state persistence (Task 14) under the existing stream endpoint's
   range/transcode behaviour.** `kima-web` must not assume a different streaming contract than the Next.js player
   already relies on; Task 14's tests reuse the same stream endpoint behaviour M1 already established, not a new
   one.

## Tasks

- [ ] **Task 0: Toolchain and CI groundwork for a wasm32 crate**
  - Confirm the Rust toolchain `vertigo` 0.6.3 actually needs to build `wasm32-unknown-unknown` (nightly per the
    tutorial, or stable — test with the pinned workspace toolchain first, record the actual error if it fails,
    do not assume the tutorial is still accurate for every claim). Add `wasm32-unknown-unknown` to whatever
    `rustup target add` the dev environment and CI need. Install `wasm-opt` (binaryen) in the CI image.
  - Add a CI job (`.github/workflows/ci.yml`) that runs once `kima-web` exists (Task 2) but is written now as a
    placeholder with a comment pointing at the task that activates it, so the groundwork and the activation are
    reviewable separately.
  - Step 1 (test-first in spirit — this task is infrastructure, so "test" means a verification script): write
    `tools/verify-wasm-toolchain.sh` that runs `rustc --version --verbose`, confirms `wasm32-unknown-unknown` is an
    installed target, and confirms `wasm-opt --version` succeeds; run it and capture the output in the task's
    commit message body (not committed as a log file).
  - Step 2: document the confirmed toolchain requirement in a new `crates/kima-web/README.md` stub (one paragraph:
    what target, what nightly/stable requirement, how to install `wasm-opt`) — this file is the only `kima-web`
    content this task creates; the crate itself does not exist yet (Task 2).
  - **Acceptance criteria:** `tools/verify-wasm-toolchain.sh` runs clean on a fresh clone of the dev environment;
    the CI YAML diff is reviewable on its own (no behavioural change to existing jobs); the toolchain requirement
    is written down somewhere a later task can cite instead of re-discovering it.
  - Files: create `tools/verify-wasm-toolchain.sh`, `crates/kima-web/README.md`; modify `.github/workflows/ci.yml`.
  - Divergences: none (no behaviour change yet).
  - [ ] **Step 3: Commit** `chore(web): confirm wasm32 toolchain requirements for kima-web`.

- [ ] **Task 1: Extract `kima-api-types` — auth and onboarding DTOs first**
  - **Step 1 (test first):** add `crates/kima-api-types/tests/fixtures/{login_request.json,login_response.json,
    me_response.json,onboarding_credentials.json}` captured from the real running backend (not hand-typed), and a
    test `auth_types_round_trip` in `crates/kima-api-types/tests/auth.rs` asserting
    `serde_json::from_str::<Credentials>(fixture)` then `serde_json::to_string` reproduces the same parsed value
    (field-by-field, not byte-for-byte, since key order is not guaranteed) for each fixture. This test fails to
    compile until the types exist — expected red state.
  - **Step 2:** create `crates/kima-api-types` (workspace member; `Cargo.toml` deps: `serde` with `derive`, `uuid`
    with `serde`, plus whatever timestamp crate `kima-api::routes::auth` is found to use once inspected). Add
    `mod auth; mod onboarding; mod error;` with `Credentials`, the login response shape, `me` response shape, and
    `ApiErrorBody` moved verbatim (field names, `#[serde(rename_all = "camelCase")]`, optionality) from
    `kima-api::routes::auth`/`routes::onboarding`/`kima_api::error::ApiError`'s JSON shape.
  - **Step 3:** edit `kima-api::routes::auth` and `routes::onboarding` to `pub use kima_api_types::auth::Credentials`
    (etc.) instead of defining the struct locally; confirm every existing `kima-api` test (`tests/*.rs`) still
    passes unchanged — any failure here means the move was not behaviour-preserving and must be fixed before
    proceeding, not worked around.
  - **Acceptance criteria:** `cargo test -p kima-api-types` and `cargo test -p kima-api` both green; `cargo build
    -p kima-api-types --target wasm32-unknown-unknown` succeeds; no diff in any existing `kima-api` test's
    expected JSON.
  - Files: create `crates/kima-api-types/{Cargo.toml,src/{lib.rs,auth.rs,onboarding.rs,error.rs},tests/{auth.rs,
    fixtures/*.json}}`; modify `Cargo.toml` (workspace members), `crates/kima-api/Cargo.toml` (new dep),
    `crates/kima-api/src/routes/{auth.rs,onboarding.rs}`, `crates/kima-api/src/error.rs` (re-export shape).
  - Divergences: none (behaviour-preserving move).
  - [ ] **Step 4: Commit** `refactor(api): move auth/onboarding DTOs into kima-api-types`.

- [ ] **Task 2: Scaffold the `kima-web` crate and get `vertigo build` producing output**
  - **Step 1 (test first):** write `tools/verify-kima-web-build.sh` asserting `cargo run -p vertigo-cli -- build`
    (or the installed `vertigo` binary, whichever Task 0 settled on) run inside `crates/kima-web` produces a
    `build/index.html`, a hashed `.wasm`, and `wasm_run.js`; run it now against a not-yet-existing crate to confirm
    it fails for the right reason (red state), then implement.
  - **Step 2:** `cargo new --lib crates/kima-web` reshaped to vertigo's expected shape (`crate-type = ["cdylib",
    "rlib"]`, matching `examples/router/Cargo.toml`); minimal `#[main] fn app() -> DomNode` showing a static
    "kima-web" placeholder page (no real pages yet — this task proves the build pipeline only). Add
    `kima-api-types` as a dependency to prove the wasm-target constraint from Task 1 holds transitively.
  - **Step 3:** run `tools/verify-kima-web-build.sh` for real; fix until green.
  - **Acceptance criteria:** `vertigo build` run from `crates/kima-web` succeeds and produces the three artifacts
    above; `cargo clippy -p kima-web --target wasm32-unknown-unknown -- -D warnings` is clean; the crate is a
    workspace member.
  - Files: create `crates/kima-web/{Cargo.toml,src/lib.rs}`, `tools/verify-kima-web-build.sh`; modify root
    `Cargo.toml` (workspace members).
  - Divergences: none.
  - [ ] **Step 4: Commit** `feat(web): scaffold kima-web crate, verify vertigo build output`.

- [ ] **Task 3: Embed wasmtime SSR for `kima-web` in-process in `kima-server`, reverse-proxied over loopback**
  - **Step 1 (test first):** `crates/kima-server/tests/web_ssr_embed.rs` — three assertions written first (they
    fail, no embedding exists yet): (a) a request to one of `kima-web`'s routes (e.g. `/login`) returns HTML whose
    body was produced by the embedded wasmtime SSR task (assert on real rendered markup, not an empty shell —
    this is the test that actually distinguishes SSR from a CSR stub); (b) a request to a path outside §5 of the
    spec (e.g. `/podcasts`) gets `kima-server`'s normal 404/existing routing, never proxied to the SSR task; (c)
    with the embedded SSR task deliberately not started (simulated for the test), a request to a `kima-web` route
    returns a clear 5xx, while `/api/health` (or equivalent) still responds normally — proving the failure mode is
    contained, per Review Focus #3.
  - **Step 2:** add a `kima_web_ssr` module to `kima-server` that, at startup, constructs `vertigo_cli::serve::
    ServeOpts { common: CommonOpts { dest_dir: <kima-web's build/> }, inner: ServeOptsInner { host:
    "127.0.0.1".into(), port: <internal, configurable>, proxy: vec![], env: vec![] } }` by hand (no CLI parsing —
    confirmed constructible, spec §3) and spawns `vertigo_cli::serve::run(opts, None)` as a `tokio::task`, logging
    and (per spec §12 Q3-equivalent, now the embedded-SSR liveness question) deciding here whether a task panic
    crashes the whole process or is caught and surfaced as the contained 5xx from Step 1(c) — pick the contained
    failure mode unless this task finds a concrete reason the orchestrator-restart approach is simpler, and record
    which was chosen in the commit body.
  - **Step 3:** add a reverse-proxy layer to `kima_server::frontend` (or a new `kima_server::web_proxy` module) —
    for exactly the path allow-list §5 covers (a small named constant, extended page by page by Tasks 8–16, same
    shape the original switch design used for its allow-list), forward the incoming request's method/headers/body
    to the loopback address and stream the response back; everything else keeps `kima-server`'s existing routing
    unchanged.
  - **Step 4:** run the Task's own tests; fix until green. Run the full existing `kima-server` test suite to
    confirm nothing regressed. Confirm `cargo tree -p kima-server` shows `vertigo-cli`'s axum 0.6/poem/reqwest
    stack pulled in alongside `kima-server`'s axum 0.8 stack as two independent dependency subtrees (expected per
    spec §3 — note this in the commit body, do not treat a larger `Cargo.lock`/longer build as a bug to "fix").
  - **Acceptance criteria:** all Step 1 assertions pass; every pre-existing `kima-server` test still passes
    unchanged; the allow-list of `kima-web`-served paths is a single named constant a later task can extend page
    by page (Tasks 8–16 each add one entry, not re-architect the proxy); the embedded task's failure mode is
    explicit and tested, not assumed.
  - Files: create `crates/kima-server/tests/web_ssr_embed.rs`, `crates/kima-server/src/web_ssr.rs` (embedded task
    spawn), `crates/kima-server/src/web_proxy.rs` (reverse-proxy layer + allow-list constant); modify
    `crates/kima-core/src/config.rs` (internal SSR port + `build/` dir config), `crates/kima-server/src/lib.rs`
    (startup wiring), `crates/kima-server/Cargo.toml` (new `vertigo-cli` dependency), `docker-compose*.yml`/
    `Dockerfile` (copy `kima-web`'s `build/` output into the image; confirm exact path during this task).
  - Divergences: none yet (the embedded SSR path existing is not itself a divergence; D-134 waits for Task 19).
  - [ ] **Step 5: Commit** `feat(web): embed wasmtime SSR for kima-web in-process, reverse-proxied over loopback`.

- [ ] **Task 4: Routing skeleton in `kima-web`**
  - **Step 1 (test first):** `crates/kima-web/src/route.rs` tests (native `cargo test -p kima-web`, following
    `examples/router`'s `Route` pattern) asserting `Route::from(String)`/`.to_string()` round-trip for every path
    in §5 of the spec (`/login`, `/setup`, the browse root, `/search`, playlist list/detail, the settings slice),
    plus a `NotFound` fallback for anything else. Write the enum variants and the test table together but assert
    the round-trip, not just "it compiles".
  - **Step 2:** implement `Route` and wire `vertigo::router::Router::new_history_router()` into the root `#[main]`
    component, rendering a per-variant placeholder (`dom! { <div>"{variant debug}"</div> }`) — real pages arrive
    in Tasks 8–16, this task only proves routing dispatches correctly.
  - **Acceptance criteria:** the round-trip test passes for every path; manually loading each path in `vertigo
    watch` (dev-only SSR per spec §3) shows the right placeholder; no page has real content yet.
  - Files: create `crates/kima-web/src/route.rs`; modify `crates/kima-web/src/lib.rs`.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): routing skeleton for the seven M9 pages`.

- [ ] **Task 5: The fetch client bridging `serde_json` and vertigo's `RequestBuilder`**
  - **Step 1 (test first):** `crates/kima-web/src/api/client.rs` tests exercising the client against a tiny local
    mock (vertigo's test harness or a hand-rolled fake `Driver`/fetch stub — confirm what vertigo's own tests use
    for mocking fetch, in `crates/vertigo/src/tests/api_access/mod.rs`, and reuse that approach rather than
    inventing a second one) covering: a successful typed GET deserializes into a `kima-api-types` struct; a 4xx/5xx
    response deserializes into `ApiErrorBody` and the client surfaces it as `Resource::Error(String)`; a network
    failure (no response at all) also surfaces as `Resource::Error`; a bearer token, when set, is attached via
    `.bearer_auth`.
  - **Step 2:** implement `ApiClient` (wraps `RequestBuilder`, encodes request bodies with `serde_json::to_string`
    into `RequestBody::Text`, decodes responses with `serde_json::from_str`, maps non-2xx to `ApiErrorBody`),
    returning `Resource<T>` per call per spec §4.2 and §5 (shared loading/error presentation).
  - **Acceptance criteria:** all Step 1 cases pass; the client has no dependency on any specific page; it is
    exercised for real against the running backend for the first time in Task 7 (login), not mocked forever.
  - Files: create `crates/kima-web/src/api/{mod.rs,client.rs}`.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): typed fetch client bridging serde_json and vertigo's RequestBuilder`.

- [ ] **Task 6: `kima-api-types::library`, `search`, `playback_state`, `playlists`, `settings` DTOs**
  - **Step 1 (test first):** extend `crates/kima-api-types/tests/` with fixtures + round-trip tests for every DTO
    the browse/search/player/playlists/settings pages will need (artist/album/track list+detail shapes, search
    result shape, playback-state get/set shape, playlist list/detail/create/reorder shapes, the account-settings
    subset of `/system-settings` decided in the spec §5.7/§12 Q5). Capture fixtures from the real backend, as in
    Task 1 — this is the same pattern repeated for a wider surface, not a new technique.
  - **Step 2:** add the modules and types to `kima-api-types`; migrate the corresponding `kima-api` route modules
    (`routes::library::{artists,albums,tracks}`, `routes::search`, `routes::playback_state`, `routes::playlists`,
    `routes::settings`) to use them, exactly as Task 1 did for auth/onboarding.
  - **Step 3:** run every pre-existing `kima-api` test; any failure means the relocation changed behaviour and
    must be fixed, not special-cased.
  - **Acceptance criteria:** all new round-trip tests pass; `cargo build -p kima-api-types --target
    wasm32-unknown-unknown` still succeeds with the larger surface; no `kima-api` test regresses.
  - Files: extend `crates/kima-api-types/src/{library.rs,search.rs,playback_state.rs,playlists.rs,settings.rs}`
    and `tests/`; modify the corresponding `kima-api` route modules.
  - Divergences: none (behaviour-preserving).
  - [ ] **Step 4: Commit** `refactor(api): move library/search/playback/playlists/settings DTOs into kima-api-types`.

- [ ] **Task 7: Auth guard and session storage**
  - **Step 1 (test first):** component test (native, vertigo's DOM test harness) asserting: with no stored
    token, any guarded route renders the login page instead of its content; with a stored (fake/mocked) valid
    token, the guarded content renders; a 401 from any API call clears the stored token and redirects to login.
    Also a test for the spec §12 Q3/Review Focus #3 scenario: the storage key/shape chosen does not collide with
    or get confused by `frontend/lib/auth-context.tsx`'s storage during the side-by-side period (decide and record
    the owner's answer to Q3 here if not already settled; if unanswered, default to an isolated `kima-web`-only
    storage key so the two frontends never silently share or stomp a session, and note that choice in the spec's
    open-questions log as resolved).
  - **Step 2:** implement the auth guard component and the session store (`crates/kima-web/src/auth.rs`), wired
    through the Task 5 client.
  - **Acceptance criteria:** all Step 1 cases pass; manually verified in `vertigo watch` that visiting a guarded
    route with no session shows the login page.
  - Files: create `crates/kima-web/src/auth.rs`; modify `crates/kima-web/src/route.rs` (guard wiring).
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): auth guard and session storage for kima-web`.

- [ ] **Task 8: Login page**
  - **Step 1 (test first):** component test — submitting valid credentials calls `POST /auth/login` via the Task 5
    client and, on success, stores the session and navigates to the library root; on a 401/429 the backend's
    `ApiErrorBody.error` message is shown verbatim (including the 429 `Retry-After` case, D-104/D-105 — assert the
    shown message, not just "an error shows").
  - **Step 2:** implement `crates/kima-web/src/pages/login.rs`; add `/login` to Task 3's allow-list.
  - **Acceptance criteria:** Playwright (Task 17 adds the actual browser spec; this task's own acceptance is the
    component test plus a manual `vertigo watch` smoke check against the real dev backend: login with a real
    account succeeds and redirects).
  - Files: create `crates/kima-web/src/pages/login.rs`; modify `crates/kima-server/src/frontend.rs` (allow-list).
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): login page`.

- [ ] **Task 9: Onboarding page**
  - **Step 1 (test first):** component test — the onboarding form is shown only when the backend reports no admin
    user exists yet (confirm the real signal from `kima-api-types::onboarding`/`routes::onboarding` in Task 1/6,
    not guessed); submitting valid admin credentials calls the onboarding endpoint and signs the new admin in
    (reusing Task 7's session store) and navigates to the library root; visiting `/setup` once an admin already
    exists redirects to `/login` instead of showing the form again.
  - **Step 2:** implement `crates/kima-web/src/pages/onboarding.rs`; add `/setup` to the allow-list.
  - **Acceptance criteria:** component tests pass; manual `vertigo watch` smoke check against a fresh dev database
    (no admin yet) completes onboarding and lands signed in.
  - Files: create `crates/kima-web/src/pages/onboarding.rs`; modify `crates/kima-server/src/frontend.rs`.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): onboarding page`.

- [ ] **Task 10: Library browse — artists list**
  - **Step 1 (test first):** component test — the page shows a `Resource::Loading` state, then renders the
    fetched artist list paginated/scrolled the way Task 6's DTO shape supports, then an error state on a failed
    fetch (reusing the shared `Resource`-presentation component from Task 5/spec §5's "non-page concerns").
  - **Step 2:** implement the artists list; add its route to the allow-list.
  - **Acceptance criteria:** component tests pass; manual check against the dev backend's real library shows real
    artists.
  - Files: create `crates/kima-web/src/pages/browse/artists.rs`; modify route/allow-list.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): library browse — artists list`.

- [ ] **Task 11: Library browse — albums and tracks lists**
  - Same TDD shape as Task 10, one component test per list before its implementation, covering albums (with
    artwork URL field from `kima-api-types::library`) and tracks (with duration/track-number fields).
  - **Acceptance criteria:** component tests pass for both lists; manual check against the dev backend.
  - Files: create `crates/kima-web/src/pages/browse/{albums.rs,tracks.rs}`; modify route/allow-list.
  - Divergences: none.
  - [ ] **Step N: Commit** `feat(web): library browse — albums and tracks lists`.

- [ ] **Task 12: Search page**
  - **Step 1 (test first):** component test — debounced input (confirm the debounce behaviour is testable
    synchronously in vertigo's harness, or test the non-debounced query-dispatch logic directly if not, documenting
    which); results grouped by artist/album/track matching the backend's grouping; an empty query shows no results
    and no request is made (mirrors the Next.js search page's behaviour — confirm by reading
    `frontend/app` 's search page before writing the assertion, not by assumption).
  - **Step 2:** implement `crates/kima-web/src/pages/search.rs`; add to allow-list.
  - **Acceptance criteria:** component tests pass; manual check with a real query against the dev backend.
  - Files: create `crates/kima-web/src/pages/search.rs`; modify route/allow-list.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): search page`.

- [ ] **Task 13: Playlists — list and detail**
  - **Step 1 (test first):** component tests — list page renders fetched playlists; detail page renders a
    playlist's tracks; creating a playlist calls the create endpoint and navigates to its detail page; removing a
    track from the detail view calls the backend and updates the rendered list optimistically or after refetch
    (decide which during implementation and assert the chosen behaviour, not both).
  - **Step 2:** implement `crates/kima-web/src/pages/playlists/{list.rs,detail.rs}`; add both routes to the
    allow-list.
  - **Acceptance criteria:** component tests pass; manual check creating/editing a real playlist against the dev
    backend.
  - Files: create `crates/kima-web/src/pages/playlists/{list.rs,detail.rs}`; modify route/allow-list.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): playlists — list and detail`.

- [ ] **Task 14: Player component**
  - **Step 1 (test first):** component test — given a "current track" state, the player renders an `<audio>` tag
    whose `src` points at the existing `/api/library/tracks/:id/stream` endpoint (no new streaming contract, per
    Review Focus #4); transport actions (play/pause/seek/next/prev over a queue) update the shared playback state
    and, on an interval or on pause/track-change, persist position through `kima-api-types::playback_state` via the
    Task 5 client; reloading the page restores position from the backend's last-saved state.
  - **Step 2:** implement `crates/kima-web/src/player.rs` as a persistent component mounted alongside every page
    (not a route) per spec §5.5.
  - **Acceptance criteria:** component tests pass; manual check playing a real track against the dev backend,
    confirming position survives a reload.
  - Files: create `crates/kima-web/src/player.rs`; modify `crates/kima-web/src/lib.rs` (mount point).
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): player component with stream playback and position persistence`.

- [ ] **Task 15: Settings — account slice**
  - **Step 1 (test first):** component test — the page shows the signed-in user's username; submitting a password
    change calls `auth::change_password` and shows success/failure from the backend's `ApiErrorBody` verbatim.
  - **Step 2:** implement `crates/kima-web/src/pages/settings.rs` covering only the account slice decided in spec
    §5.7 (minimal placeholder, confirmed by the owner as the settled scope for M9 — not reopened; the full
    Settings page is M10's job).
  - **Acceptance criteria:** component tests pass; manual password change against the dev backend succeeds and a
    re-login with the new password works.
  - Files: create `crates/kima-web/src/pages/settings.rs`; modify route/allow-list.
  - Divergences: none.
  - [ ] **Step 3: Commit** `feat(web): settings — account slice`.

- [ ] **Task 16: End-to-end wiring check across all seven pages**
  - **Step 1 (test first):** a `kima-server` integration test driving the full flow through the HTTP layer only
    (no browser): onboarding → login → browse → search → create a playlist → add a track → play (assert the
    stream endpoint is reachable with the session token) → change the account password — using `kima-api-types`
    directly to build/parse requests, proving the shared types work end to end against the real running server,
    independent of whether `kima-web`'s actual WASM renders correctly (that is Playwright's job, Task 18).
  - **Acceptance criteria:** the full flow passes against a fresh test database every time (no shared state
    between runs); this is the first test that exercises every DTO from Tasks 1 and 6 in one sequence.
  - Files: create `crates/kima-server/tests/web_flow_end_to_end.rs`.
  - Divergences: none.
  - [ ] **Step 2: Commit** `test(server): end-to-end flow test across kima-api-types' full M9 surface`.

- [ ] **Task 17: `Dockerfile` `ARG` for an optional dev-only legacy Next.js frontend build**
  - **Step 1 (test first):** `tools/verify-legacy-frontend-arg.sh` — asserts that `docker build .` (no `ARG` set)
    produces an image with no Node toolchain and no `frontend/` build artifacts baked in (verified by `docker run
    --rm <image> sh -c '! command -v node && ! test -d /app/legacy-frontend'` or equivalent), and that
    `docker build --build-arg KIMA_BUILD_LEGACY_FRONTEND=1 .` produces an image that *does* contain the Next.js
    static export at the path `kima-server` expects for `/__legacy/`. Run it against the current `Dockerfile`
    first to confirm it fails for the right reason (no `ARG` wired yet), then implement.
  - **Step 2:** add `ARG KIMA_BUILD_LEGACY_FRONTEND=0` to the `Dockerfile`; gate the existing Next.js build stage
    (`npm ci && npm run build` or equivalent) behind that `ARG` so it is skipped entirely by default (no Node
    invoked, no `frontend/` stage run) and only executed when the `ARG` is set to a truthy value; when skipped,
    the final image stage must not `COPY` anything from the skipped build stage (confirm the multi-stage Dockerfile
    doesn't unconditionally reference it elsewhere).
  - **Step 3:** add a runtime env var (e.g. `KIMA_LEGACY_FRONTEND_PATH`) to `kima_core::config::Config` (unset by
    default) that, only when both the build-time baked assets exist *and* the env var is set, makes `kima-server`
    serve them from `/__legacy/` via a `ServeDir` — never at `/`, never overlapping any path in the Task 3
    allow-list; add a `kima-server` test asserting `/__legacy/` 404s when the env var is unset even if assets
    happen to be present (belt-and-suspenders against accidental production exposure, per spec §6.2).
  - **Acceptance criteria:** `tools/verify-legacy-frontend-arg.sh` passes for both the default and `ARG`-set
    builds; a `kima-server` test confirms `/__legacy/` is inert unless explicitly enabled at both build and run
    time; no change to any existing route under the Task 3 allow-list or to `/api/*`.
  - Files: create `tools/verify-legacy-frontend-arg.sh`; modify `Dockerfile`, `crates/kima-core/src/config.rs`,
    `crates/kima-server/src/frontend.rs` (or `web_proxy.rs`) for the `/__legacy/` mount, and a new
    `crates/kima-server/tests/legacy_frontend_dev_flag.rs`.
  - Divergences: none (this is strictly additive dev tooling; it does not change production behaviour).
  - [ ] **Step 4: Commit** `chore(docker): add KIMA_BUILD_LEGACY_FRONTEND ARG for optional dev-only Next.js build`.

- [ ] **Task 18: Playwright for `kima-web`, the production-gating suite**
  - **Step 1 (test first, in the sense that the spec is the "test"):** write `frontend/playwright.kima-web.
    config.ts` (sibling to the existing `playwright.kima-rs.config.ts`) pointed at a normal production-shaped
    `kima-server` build (no `KIMA_BUILD_LEGACY_FRONTEND`, exercising the Task 3 embedded-SSR proxy for real), and
    `frontend/tests/e2e/kima-web/smoke.spec.ts` asserting the login page renders (with real SSR-produced markup
    present before any JS runs, not just after hydration — the assertion that actually tells SSR apart from CSR)
    and a login with a seeded test account succeeds — written and run against the Task 8 login page first,
    confirming the harness itself works before the remaining specs are added.
  - **Step 2:** add `frontend/tests/e2e/kima-web/{onboarding,browse,search,playlists,player,settings}.spec.ts`,
    one per page, porting the same user-visible assertions the existing Next.js specs make for the overlapping
    behaviour (not copying selectors — vertigo's DOM output has different markup, per spec §7).
  - **Step 3:** repoint the existing `frontend/tests/e2e/kima-rs/*.spec.ts` CI job to build the image with
    `KIMA_BUILD_LEGACY_FRONTEND=1` (Task 17) and run against `/__legacy/` instead of `/` — this job becomes the
    dev-build regression net for pages not yet ported, not a production gate (spec §7); add a second, separate CI
    job that builds the image without the `ARG` and runs `playwright.kima-web.config.ts` against it — this is the
    one CI treats as defining production correctness. The two jobs use two different image builds and do not
    share a running `kima-server` instance.
  - **Acceptance criteria:** both Playwright jobs green in CI; the pre-existing `kima-rs` suite's specs are
    unchanged in content (only its CI job's build/target changed); the `kima-web` job demonstrably exercises SSR,
    not a CSR shortcut (Step 1's markup-before-hydration assertion is the proof).
  - Files: create `frontend/playwright.kima-web.config.ts`, `frontend/tests/e2e/kima-web/*.spec.ts`; modify
    `.github/workflows/ci.yml` (two jobs: production-gate and legacy-dev-build).
  - Divergences: none.
  - [ ] **Step 4: Commit** `test(e2e): Playwright suite for kima-web's seven M9 pages, production-gating`.

- [ ] **Task 19: Confirm the production SSR path end to end, and log the divergence**
  - Re-verify, in one sitting, that a production-shaped build (no `KIMA_BUILD_LEGACY_FRONTEND`) serves every one
    of the seven M9 pages through the Task 3 embedded-SSR proxy with no Next.js involvement anywhere in that
    build — this is the acceptance bar for the whole milestone's revised scope (owner decisions, spec header), not
    just this task's own diff.
  - Write `docs/DIVERGENCES.md` row **D-134** per spec §8, using the final, actually-shipped wording (not the
    spec's draft wording, if anything changed during implementation — e.g. if the embedded-SSR liveness policy
    from Task 3 or the §12 open questions were answered differently than the spec's default assumption, the row
    must say what shipped).
  - Update `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` row M9 to point at this plan file and mark it
    done, following the exact pattern M1–M3's rows use (✅ marker, plan filename); note explicitly that Next.js is
    retired from production as of M9 (not M10 as the original roadmap note implied), so M10's own note can be
    updated to reflect "finish porting the remaining pages" rather than "flip the default".
  - **Acceptance criteria:** `docs/DIVERGENCES.md` D-134 reads correctly against what actually shipped (re-read it
    once after Tasks 1–18 land, not drafted blind from the spec); the roadmap row is updated; every acceptance
    criterion of every earlier task in this plan is independently re-verified once more in one sitting (a final
    `cargo test --workspace`, the Task 0 verify script, both Playwright jobs, both `Dockerfile` `ARG` states from
    Task 17) before this commit.
  - Files: modify `docs/DIVERGENCES.md`, `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`.
  - Divergences: D-134 (written here).
  - [ ] **Step 1: Commit** `docs(web): log kima-web as the sole production frontend as D-134; mark M9 done`.
