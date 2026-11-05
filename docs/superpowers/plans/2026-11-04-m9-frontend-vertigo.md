# M9: Rust/WASM Frontend, Part 1 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a new user can open `kima-web` (behind the `KIMA_WEB_FRONTEND` switch), create the first admin account,
sign in, browse the library, search, play a track, see/edit a playlist, and change account settings — all through
types shared with the backend via `kima-api-types`, while the Next.js frontend keeps serving every other route
unchanged and the existing Playwright suite against it stays green.

**Architecture:** `crates/kima-api-types` (new, dependency-light, native + `wasm32-unknown-unknown`) holds the DTOs
for login/onboarding/library/search/playback-state/playlists/settings; `kima-api` route modules are migrated to use
it in place of locally-defined structs (no wire-format change). `crates/kima-web` (new) is a `vertigo`/`vertigo-cli`
crate: a hand-written route enum (`vertigo::router::Router`), one page module per §5 page, a thin fetch client
(`kima-web::api::client`) that bridges `serde_json` and vertigo's `RequestBuilder`/`RequestBody::Text`. `kima-web`
builds via `vertigo build` to a static `build/` directory; `kima-server::frontend` gains a second `ServeDir` for it,
selected per-request by the switch. Playwright gets a second config/spec-tree for `kima-web`'s pages; the existing
suite is untouched.

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
3. Production serving is CSR-only (`vertigo build`'s static output through a `ServeDir`), not vertigo's
   wasmtime-based SSR server (spec §3, §6; owner question Q1 in spec §12 — if the owner answers "keep SSR", Task 3
   changes shape; do not start Task 3 before that answer exists in the spec's open-questions log or a recorded
   owner decision).
4. Switch: server-side, env var `KIMA_WEB_FRONTEND` plus a request-scoped override, both frontends served by
   `kima-server` at once (spec §6).
5. Playwright: a second config + spec tree for `kima-web`, existing suite untouched (spec §7).

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
- **Divergence rows:** D-134 is written **once**, in Task 18 (the last task), after the switch and at least one
  page are verified working end to end — not speculatively earlier. If any task discovers an incidental backend
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
2. **The switch (Task 3) serving the wrong frontend for a route `kima-web` does not cover.** Every route outside
   §5 of the spec must fall through to the Next.js export regardless of `KIMA_WEB_FRONTEND`, checked by a
   dedicated fallback test (Task 3) before any page work starts, so later tasks cannot accidentally depend on a
   switch that 404s half the app.
3. **Auth/session boundary between the two frontends during the side-by-side period (Task 7).** A user signed in
   on one frontend hitting a `kima-web` page (or vice versa) must not see a confusing half-logged-in state; the
   spec defers the exact storage mechanism to the owner (§12 Q3) but whichever answer lands, the test in Task 7
   must cover "signed in on Next.js, switch flips mid-session" at least at the level of "does not crash / shows
   the login page", since that is the realistic owner workflow while comparing the two frontends.
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

- [ ] **Task 3: Serve `kima-web`'s build from `kima-server`, behind the `KIMA_WEB_FRONTEND` switch**
  - **Step 1 (test first):** `crates/kima-server/tests/web_frontend_switch.rs` — with the switch off (default), a
    request to `/` and to a path outside §5 of the spec is served by the Next.js `ServeDir` (assert on a
    Next.js-export-only marker file/response, not on page content); with the switch on (env var set, or the
    request-scoped override set for the specific request only), a request to `/login` (one of `kima-web`'s routes)
    is served by `kima-web`'s `build/` output, while a path outside §5 (e.g. `/podcasts`) is **still** served by
    Next.js even with the switch on. Write these three assertions first; they fail (no switch exists yet).
  - **Step 2:** add `KIMA_WEB_FRONTEND` to `kima_core::config::Config` (default `off`, matching `KIMA_IDENTIFY`'s
    pattern — same file, same style); add the request-scoped override (decided here: a `?kima_web=1` query param
    checked before falling back to the env default — simplest to implement and to drive from Playwright per spec
    §6; revisit if the owner answers §12 Q2 with a stronger requirement). Add the `kima-web` `ServeDir` to
    `kima_server::frontend::routes`, routed only for the path set §5 actually covers (a small allow-list constant,
    mirroring `LEGACY_ROUTES`'s style in the same file), falling through to the existing Next.js `ServeDir` for
    everything else and for the switch-off case.
  - **Step 3:** run the Task's own tests; fix until green. Run the full existing `kima-server` test suite to
    confirm nothing about the Next.js serving path regressed.
  - **Acceptance criteria:** all three assertions from Step 1 pass; every pre-existing `kima-server` test still
    passes unchanged; the allow-list of `kima-web`-served paths is a single named constant a later task can extend
    page by page (Tasks 8–16 each add one entry, not re-architect the switch).
  - Files: create `crates/kima-server/tests/web_frontend_switch.rs`; modify `crates/kima-core/src/config.rs`,
    `crates/kima-server/src/frontend.rs`, `crates/kima-server/src/lib.rs` (config wiring), `docker-compose*.yml`/
    `Dockerfile` only if the build needs the `kima-web` `build/` output copied in (confirm during this task).
  - Divergences: none yet (the switch existing is not itself a divergence; D-134 waits for Task 18).
  - [ ] **Step 4: Commit** `feat(web): serve kima-web behind KIMA_WEB_FRONTEND switch, Next.js unaffected elsewhere`.

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
    §5.7/§12 Q5 (confirm the owner's answer to Q5 before adding anything beyond account/password — if unanswered,
    ship account-only and note the deferral in this task's commit body).
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
    independent of whether `kima-web`'s actual WASM renders correctly (that is Playwright's job, Task 17).
  - **Acceptance criteria:** the full flow passes against a fresh test database every time (no shared state
    between runs); this is the first test that exercises every DTO from Tasks 1 and 6 in one sequence.
  - Files: create `crates/kima-server/tests/web_flow_end_to_end.rs`.
  - Divergences: none.
  - [ ] **Step 2: Commit** `test(server): end-to-end flow test across kima-api-types' full M9 surface`.

- [ ] **Task 17: Playwright for `kima-web`**
  - **Step 1 (test first, in the sense that the spec is the "test"):** write `frontend/playwright.kima-web.
    config.ts` (sibling to the existing `playwright.kima-rs.config.ts`) pointed at the request-scoped switch
    override from Task 3, and `frontend/tests/e2e/kima-web/smoke.spec.ts` asserting the login page renders and a
    login with a seeded test account succeeds — written and run against the Task 8 login page first, confirming
    the harness itself works before the remaining specs are added.
  - **Step 2:** add `frontend/tests/e2e/kima-web/{onboarding,browse,search,playlists,player,settings}.spec.ts`,
    one per page, porting the same user-visible assertions the existing Next.js specs make for the overlapping
    behaviour (not copying selectors — vertigo's DOM output has different markup, per spec §7).
  - **Step 3:** add/extend the CI job so both `playwright.kima-rs.config.ts` and `playwright.kima-web.config.ts`
    run against one `kima-server` instance with `KIMA_WEB_FRONTEND` available, per spec §7; confirm the existing
    suite's job is untouched in behaviour (same specs, same config, same pass/fail criteria as before this plan).
  - **Acceptance criteria:** both Playwright configs green in CI; the pre-existing `kima-rs` suite's run time and
    assertions are unchanged (a diff of that job's config/specs should be empty).
  - Files: create `frontend/playwright.kima-web.config.ts`, `frontend/tests/e2e/kima-web/*.spec.ts`; modify
    `.github/workflows/ci.yml`.
  - Divergences: none.
  - [ ] **Step 4: Commit** `test(e2e): Playwright suite for kima-web's seven M9 pages`.

- [ ] **Task 18: Flip the switch on in dev/CI defaults where appropriate, and log the divergence**
  - Confirm with the owner (or per their earlier answer to spec §12 Q2) whether `KIMA_WEB_FRONTEND`'s **default**
    changes from `off` to something else anywhere (e.g. CI always exercises it via the override regardless of the
    default, which is likely sufficient — changing the production default is explicitly an M10 decision per the
    roadmap, not this plan's to make unilaterally).
  - Write `docs/DIVERGENCES.md` row **D-134** per spec §8, using the final, actually-shipped wording (not the
    spec's draft wording, if anything changed during implementation — e.g. if Q1/Q2/Q3 were answered differently
    than the spec's default assumption, the row must say what shipped).
  - Update `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` row M9 to point at this plan file and mark it
    done, following the exact pattern M1–M3's rows use (✅ marker, plan filename).
  - **Acceptance criteria:** `docs/DIVERGENCES.md` D-134 reads correctly against what actually shipped (re-read it
    once after Tasks 1–17 land, not drafted blind from the spec); the roadmap row is updated; every acceptance
    criterion of every earlier task in this plan is independently re-verified once more in one sitting (a final
    `cargo test --workspace`, the Task 0 verify script, both Playwright configs) before this commit.
  - Files: modify `docs/DIVERGENCES.md`, `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`.
  - Divergences: D-134 (written here).
  - [ ] **Step 1: Commit** `docs(web): log the kima-web frontend switch as D-134; mark M9 done in the roadmap`.
