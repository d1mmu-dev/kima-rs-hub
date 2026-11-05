# M9: Rust/WASM Frontend, Part 1 — Design

Date: 2026-11-04 (revised 2026-11-05 per owner decisions below)
Status: Revised — scope changed by explicit owner decision after the initial draft. Superseded: the original draft
assumed dual-serve (Next.js + `kima-web` side by side behind a runtime switch) and CSR-only production for
`kima-web`. The owner has since decided: (1) production SSR for `kima-web` via embedded wasmtime (§3, §6), not
CSR-only; (2) Next.js is fully retired from production — no dual-serve, no user-visible switch; (3) Next.js stays
available only as an opt-in dev-time build (`Dockerfile` `ARG`) for migration comparison, never shipped in the
production image by default; (4) Settings stays the minimal M9 placeholder (§5.7), unchanged, full page in M10.
Remaining open questions are in §12.
Parent spec: `docs/superpowers/specs/2026-10-16-kima-rust-design.md`
Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` (row M9, "Notes carried forward")
Builds on: M0–M8 as merged on `main` (HEAD `2014543`): crates `kima-core`, `kima-db`, `kima-jobs`, `kima-library`,
`kima-ident`, `kima-net`, `kima-api`, `kima-subsonic`, `kima-server`; the forked Next.js 16 frontend served as a
static export by `kima-server::frontend` (`tower_http::ServeDir` + legacy-route redirects, D-011).
Researched directly (read-only, this repo and `/home/dimmu/projects/vertigo` checked out locally, **not modified**):
`vertigo` 0.6.3 (`crates/vertigo/src/{router.rs, fetch/*, driver_module/*, lib.rs}`, `tutorial.md`) and `vertigo-cli`
0.6.3 (`crates/vertigo-cli/{README.md, Cargo.toml, src/main.rs, src/serve/{mod.rs, serve_run.rs, serve_opts.rs,
server_state.rs}, src/commons/models.rs}`), plus the `examples/router` and `examples/counter` crates. The second
round (2026-11-05) specifically re-read `vertigo-cli`'s `serve` module to answer whether its wasmtime-based SSR
server can run in-process inside `kima-server` (§3's new finding) rather than being assumed one way or the other.

Upstream kima-hub has no Rust/WASM frontend; this whole milestone is a kima-rs addition (D-134, §10).

## 1. Intent

M1–M8 gave kima-rs a complete backend and a working Next.js frontend (a fork of upstream's, exported statically and
served by `kima-server`). The owner wants the long-term frontend to be Rust compiled to WebAssembly, built on the
`vertigo` crate, so the project's UI layer matches its backend's language and toolchain and stops carrying a Node
build step. M9 is **part 1**: a new `kima-web` crate covers the pages a new user needs to get in and listen —
login, onboarding, library browse, search, player, playlists, settings.

**Revised scope (owner decision, 2026-11-05): `kima-web` is production's *only* frontend from the moment M9 ships
it, served with real server-side rendering through an embedded wasmtime runtime (§3, §6) — not a CSR-only static
export, and not side by side with Next.js.** There is no runtime switch, no user-visible toggle, and no dual-serve
in production: Next.js is dropped from the production image entirely. The only place Next.js still exists after
M9 is an **opt-in developer build** behind a `Dockerfile` `ARG` (`KIMA_BUILD_LEGACY_FRONTEND`, §6.2), used purely to
run the old frontend side by side *locally* for comparison while the remaining ~20 pages are ported in M10 — it is
never built into, or served from, the production image, and CI's production-defining build never sets the ARG.

A direct, owner-accepted consequence: in production, any page `kima-web` does not yet cover (§2's non-goals —
podcasts, autotag UI, organizer UI, sharing, stats, vibe search, etc.) is simply **not served** between M9 and M10,
since there is no fallback to Next.js in production anymore. This is called out again in §11 (Risks) as an accepted
interim gap, not an oversight.

Why this matters:
- Dropping Node from the *production* build and container image is now true as of M9, not deferred to M10 — the
  owner's stated end goal (roadmap M10 note) is reached for production immediately; the dev-only legacy build ARG
  exists solely to de-risk the M10 porting work, not to soften the production cut.
- M11 (casting) is built straight into `kima-web`'s player and gets no Next.js equivalent (roadmap M11 note), so
  the player page built here is load-bearing for a milestone after this one.
- Because there is no fallback frontend in production, `kima-web`'s SSR path and its seven pages are the only
  thing standing between a deployed M9 build and a broken UI for those pages — correctness and the embedded-SSR
  integration (§3, §6) carry more weight than they would under the old side-by-side plan.

Success: a fresh production deployment serves `kima-web` for `/`, login, onboarding, library browse, search,
player, playlists and the minimal Settings slice, each page's initial HTML coming from wasmtime-executed SSR (not
an empty CSR shell), using only data shapes shared with the backend, with no duplicated hand-maintained
TypeScript-equivalent-in-Rust guessing of the API. A developer who sets `KIMA_BUILD_LEGACY_FRONTEND=1` at image
build time can still reach the old Next.js pages locally for side-by-side comparison. The `kima-web` Playwright
suite (§7) is the production-defining suite and is green in CI; the legacy Next.js suite still exists and still
passes, but only under the dev-flag build, and is no longer what CI gates production correctness on.

## 2. Goals

From the roadmap M9 row, as revised by the owner's 2026-11-05 decisions: a new `kima-web` crate on `vertigo` +
`vertigo-cli`, sharing API types with the backend; pages for login, onboarding, library browse, search, player,
playlists, settings; **production serves `kima-web` exclusively, with SSR via an embedded wasmtime runtime (§3,
§6) — the Next.js export is not served in production at all**; a `Dockerfile` `ARG` optionally builds the legacy
Next.js frontend for local dev-time comparison only (§6.2); the `kima-web` Playwright suite is the production
regression net, the legacy suite runs only under the dev-flag build (§7); a `DIVERGENCES.md` entry when this
lands (§8).

Non-goals (left to M10 or later, explicitly out of scope here):
- Every other page (artist/album detail beyond what browse needs, podcasts, audiobooks, radio, mixes, discover,
  Subsonic token management, identification/autotag UI, organizer UI, 2FA/API keys/device link, sharing, imports,
  stats, vibe search). M10 ports these and deletes `frontend/`.
- Casting (M11): the player built here is the eventual host for it, but no Cast/AirPlay code lands in M9.
- Making `kima-web` the default (M10).
- Any change to upstream parity or backend business logic; this milestone is frontend-architecture and
  frontend-pages only. Backend route *shapes* may be refactored (moved into a shared types crate, §4) but their
  wire behaviour (status codes, JSON bodies, camelCase, error shape) must not change — that would need its own
  `DIVERGENCES.md`/spec treatment and is explicitly deferred.

## 3. What was verified in `vertigo`/`vertigo-cli` (not assumed)

Read directly from the checked-out `vertigo` repo (`/home/dimmu/projects/vertigo`, version 0.6.3; **read-only**,
used only to ground this design):

- **Component model.** A function decorated `#[component]` (or the bare `#[main]` entry point) becomes a struct
  with a `mount`/render method; a `dom! { ... }` macro builds a tree of `DomNode`s from JSX-like markup. State is
  plain Rust created once per mount (not per render), held in `Value<T>` (a reactive, `Rc`-backed cell). Conditional
  and list rendering go through `Value`/`Computed` methods (`render_value`, `render_list`), not a diffing re-run of
  the whole function (`crates/vertigo/src/tests/dom/component.rs`, `tutorial.md` §4–§10).
- **Reactive state.** `Value<T>` is the mutable cell; `Computed<T>` is a derived, memoized read (`.map()`); a
  dependency graph re-fires only the clients (mount closures) that actually read a changed value. `bind!`/`bind_rc`
  macros clone captured values into event-handler closures (`crates/vertigo/src/computed/{value.rs,to_computed.rs}`,
  tutorial §6, §9, §10). `transaction(|ctx| ...)` batches multiple `Value` writes into one graph recompute
  (tutorial §9).
- **Routing.** `vertigo::router::Router<T>` (`crates/vertigo/src/router.rs`) wraps a `Computed<T>` kept in sync with
  either the History API (`Router::new_history_router`) or the URL hash (`new_hash_router`); `T` must implement
  `Clone + ToString + From<String> + PartialEq`. There is no nested/typed route-table macro (no file-based routing
  like Next.js's `app/`): a route enum with a hand-written `from(String)`/`to_string()` pair (exactly the pattern in
  `examples/router/src/app.rs`) is the whole routing layer; matching on the enum picks which page component to
  render via `route.route.render_value(|r| match r { ... })`.
- **HTTP client / data fetching.** `vertigo::fetch::request_builder::RequestBuilder` (`.get(url)`/`.post(url)`,
  `.bearer_auth(token)`, `.body_json(impl JsJsonSerialize)`, `.call().await` returning a `RequestResponse`,
  `.into_data::<T: JsJsonDeserialize>()`). Types cross the fetch boundary through vertigo's own `JsJson`/
  `JsJsonSerialize`/`JsJsonDeserialize` traits (`vertigo_macro::AutoJsJson` derive), **not** `serde_json` directly —
  important for §4. `vertigo::fetch::resource::Resource<T>` (`Loading`/`Ready(T)`/`Error(String)`, with `?`-operator
  support via `Try`) and `LazyCache<T>` give a `Resource`-shaped loading state for a request, the idiom this crate
  uses in place of react-query (`crates/vertigo/src/fetch/{request_builder.rs,resource.rs,lazy_cache.rs}`).
- **SSR vs CSR, and how the WASM build is assembled.** `vertigo-cli` (`crates/vertigo-cli/Cargo.toml`) embeds
  **wasmtime 0.29** and its own HTTP server (`poem`/`axum`): `vertigo build` compiles the crate to
  `wasm32-unknown-unknown`, runs `wasm-opt`, and gathers `index.html` + `wasm_run.js` (vertigo's own JS glue, not
  wasm-bindgen) + the hashed `.wasm` into a `build/` directory. `vertigo serve`/`vertigo watch` start a built-in
  server that, for each request, **runs the same compiled `.wasm` inside wasmtime server-side** to produce the
  initial HTML (so a crawler or a no-JS browser gets real markup), then ships the same `.wasm` to the browser to
  hydrate and take over reactively (tutorial §3: "Vertigo uses a built-in web server which supports
  server-side rendering... simple pages work even if the browser have JavaScript turned off"). **Owner decision,
  2026-11-05: production must use real SSR, not CSR-only.** §6 below is the integration design for that.

- **Can `vertigo serve`'s wasmtime SSR run in-process inside `kima-server`, or does it need a sidecar?**
  Investigated directly against the real `vertigo-cli` 0.6.3 source (not assumed), because this is the question
  that decides whether real production SSR is compatible with D-006 ("one `kima` binary", `docs/DIVERGENCES.md`):
  - `vertigo-cli` *is* built as a library as well as a binary (`[lib] name = "vertigo_cli" path = "src/main.rs"`,
    `crates/vertigo-cli/Cargo.toml`), and its public surface re-exports a genuinely callable, non-CLI entry point:
    `vertigo_cli::serve::run(opts: ServeOpts, port_watch: Option<u16>) -> Result<(), ErrorCode>`
    (`src/serve/mod.rs`: `pub use serve_run::run;`). `ServeOpts`/`ServeOptsInner`/`CommonOpts` are plain `pub`
    structs with `pub` fields (`src/serve/serve_opts.rs`, `src/commons/models.rs`) — they derive `clap::Args` for
    CLI parsing but nothing stops constructing them by hand in Rust code, so `run()` can genuinely be called as a
    library function from inside another binary's `#[tokio::main]`, without shelling out to the `vertigo` CLI.
  - **But `run()` does not return a `Router` or a request handler — it owns the whole HTTP server lifecycle
    itself**, down to the socket: `src/serve/serve_run.rs` builds its own `axum::Router`, calls
    `axum::Server::bind(&addr).serve(...)` (old pre-`axum::serve` 0.6-era API) and blocks on that future until the
    process exits or the bind fails. There is no way to get a `Router`/`tower::Service` out of it to `.merge()` or
    `.nest()` into `kima-server`'s own axum app — the only public API is "run a complete standalone server on
    `host:port`".
  - **The actual SSR rendering logic is not exposed at all.** The wasmtime `Engine`/`Module` setup and the
    per-request render loop live in `ServerState` (`src/serve/server_state.rs`) and `WasmInstance`/`HtmlResponse`
    (`src/serve/wasm/*`, `src/serve/html/*`) — all declared with plain `mod` (not `pub mod`) inside `serve/mod.rs`,
    so they are private to the `vertigo-cli` crate and unreachable from `kima-server` even though `vertigo-cli` is
    a dependency. Getting a bare "render this URL to an HTML string" function would require **vendoring/forking**
    roughly ten private files with no semver/API stability guarantee across `vertigo-cli` upgrades.
  - **Dependency-stack mismatch:** `vertigo-cli` 0.6.3 pins `axum = "0.6.20"`, `poem = "1.3.59"`, `reqwest`,
    `tokio` with the `full` feature — a materially older/heavier HTTP stack than `kima-server`'s `axum` 0.8 +
    `tokio`. Two different `axum` major versions compile fine side by side in one binary (they're separate crates
    under the hood), but their `Router`/middleware types cannot interoperate, which independently rules out a
    literal single-`Router` merge even if the private internals were exposed.
  - **Conclusion — chosen design (§6): embed `vertigo_cli::serve::run()` as a background `tokio::task` inside
    `kima-server`'s own process, bound to a loopback-only port (e.g. `127.0.0.1:<internal-port>`, never exposed
    externally), and have `kima-server`'s real axum app reverse-proxy the `kima-web` page routes to it over that
    loopback hop.** This is **one binary, one OS process, one supervised unit, no sidecar container, no
    supervisord, no second deployed artifact** — the parts of D-006 ("Postgres + one `kima` binary", footprint)
    that actually matter operationally are satisfied. It is *not* a literal single `Router`/single listening
    socket (there are two sockets: the external one `kima-server` already has, and an internal loopback-only one
    for the embedded wasmtime SSR server), and it does carry `vertigo-cli`'s older axum/tokio/poem/reqwest stack as
    an extra, non-trivial dependency subtree. The rejected alternative — vendoring the private `ServerState`/
    `WasmInstance`/`HtmlResponse` internals to get a true single-`Router` embed — is **not** chosen for M9: it
    would mean maintaining a fork of unstable private API surface with every `vertigo-cli` upgrade, for a benefit
    (one fewer loopback hop, no duplicate HTTP-stack dependency) that does not outweigh that maintenance cost at
    this milestone's size. Revisit only if the loopback-hop design proves inadequate in practice (§11, §12 Q1 is
    now resolved by this finding, not left open).
  - This is **not** a classic sidecar (no second container, no second process to supervise, no external reverse
    proxy/infra change, no separate lifecycle/healthcheck) and is the design actually implemented — it does not by
    itself need a new `DIVERGENCES.md` row distinct from D-134 (§8), since D-006's own wording is about
    footprint/process count, which this preserves. If implementation reveals the loopback coupling is tighter than
    expected (e.g. startup-ordering fragility between `kima-server` and the embedded `run()` task), that gets its
    own `DIVERGENCES.md` row when it actually lands (Task-level decision, not speculative here).
- **No OpenAPI / codegen tooling in `vertigo`.** Nothing in the crate generates types from a schema; everything is
  hand-written Rust structs with the `AutoJsJson` derive (vertigo's own (de)serialization) or, when going through
  plain JSON text, `serde`. This matches `kima-api`, which also has no OpenAPI (`rg utoipa|openapi` in
  `crates/kima-api` returns nothing): backend DTOs are plain `#[derive(Serialize, Deserialize)]` structs defined
  inline in route modules (e.g. `routes::auth::Credentials`, re-exported from `routes::onboarding`). §4 decides how
  these meet vertigo's `AutoJsJson`.
- **Target constraints.** `vertigo`'s own dependency list is minimal (`log`, optional `chrono`) and wasm-safe;
  nothing in it needs `tokio`, `sqlx` or `axum`. `kima-api`, `kima-db`, `kima-core` as they stand **cannot** be
  pulled into a `wasm32-unknown-unknown` crate: they depend on `sqlx` (native libpq/Postgres driver), `tokio` with
  full features, `axum` extractors, `argon2`, etc. — none of which compile (or make sense) on `wasm32-unknown-unknown`.
  Any type sharing has to go through a crate that depends on **none** of those.

## 4. Decision: how `kima-web` shares API types with the backend

**Chosen approach: a new leaf crate, `kima-api-types`, holding plain DTOs; both `kima-api` and `kima-web` depend on
it.** Rejected alternatives and why:

- **OpenAPI generation** (e.g. `utoipa` on the backend, a generator emitting Rust client types): rejected for M9.
  `kima-api` has no OpenAPI today (verified, above); adding it is its own cross-cutting change touching every route
  module, orthogonal to standing up a frontend, and would make this milestone's diff unreviewable. It stays a real
  option for a later milestone once `kima-web` covers enough surface to make generated-client churn worth it
  (tracked as a non-blocking idea, not a task, in §12 Q4).
- **`kima-web` depending on `kima-api`/`kima-db` directly, feature-gated for wasm**: rejected. Too much of those
  crates' dependency trees (sqlx, tokio-full, axum, argon2, lofty, image codecs) would need `#[cfg(not(target_arch =
  "wasm32"))]` gating throughout crates that were never designed for it; the blast radius and review risk are the
  same as a rewrite, for a handful of DTOs.
- **Hand-duplicated structs in `kima-web`, kept in sync by convention** (what the Next.js frontend effectively does
  today in `frontend/lib/api.ts`): rejected as the long-term answer — it is exactly the drift risk the owner is
  trying to get away from by unifying the stack on Rust. Acceptable only as a stopgap per-field shim (§4.3) where a
  backend type still carries DB-only fields.

### 4.1 Crate shape

New crate `crates/kima-api-types` (workspace member, native — no wasm-specific code needed; it is `#![no_std]`-free
but dependency-light enough to build for every target including `wasm32-unknown-unknown`):

- Dependencies: `serde` (`derive`), `serde_json` only where a type is intentionally `serde_json::Value` (should be
  rare and reviewed), `uuid` (`serde` feature), `time` or `chrono` matching whatever `kima-db`/`kima-api` already
  use for timestamps (checked per type during Task 1, not guessed here), and nothing else. Specifically **no**
  `sqlx`, **no** `axum`, **no** `tokio`.
- One module per API surface this milestone touches: `auth` (login/refresh/logout/me — reused by onboarding's
  session bootstrap), `onboarding`, `library` (artists/albums/tracks list+detail shapes needed by browse/search/
  player), `search`, `playback_state`, `playlists`, `settings` (the subset of `/system-settings` the Settings page
  in scope covers — see §5.7), `error` (one shared `ApiErrorBody { error: String, details: Option<...> }` matching
  `kima_api::error::ApiError`'s JSON shape, so `kima-web` has one place that knows the `{"error": "..."}` envelope).
- Every type keeps `#[serde(rename_all = "camelCase")]` to match the wire format untouched (Global Constraints,
  unchanged by this milestone).
- `kima-api` route modules are edited to `use kima_api_types::auth::Credentials` (etc.) instead of defining the
  struct locally, **re-exporting** the type where other backend code already imports it by its old path, so this is
  a mechanical move, not a behavioural change. A type that currently mixes wire fields with DB-only concerns (e.g.
  something that also carries a `PgRow`-derived field) is split: the wire-facing subset moves to `kima-api-types`,
  the rest stays a backend-local type that `From`/`Into`s into it.

### 4.2 Bridging `serde` and vertigo's `AutoJsJson`

Vertigo's fetch layer speaks `JsJsonSerialize`/`JsJsonDeserialize` (its own `JsJson` enum), not `serde::Serialize`
directly (verified in `crates/vertigo/src/fetch/request_builder.rs` — `body_json(impl JsJsonSerialize)`,
`RequestBody::into<T: JsJsonDeserialize>`). `kima-api-types` cannot derive `AutoJsJson` itself without depending on
`vertigo_macro`, which would make a native-only crate (used by `kima-api`, compiled for the server) pull in a
wasm-frontend macro crate — harmless (macros aren't wasm-specific) but a layering smell worth avoiding. Two
options, decided here:

- **Chosen:** `kima-web` sends/receives raw JSON text over vertigo's fetch (`RequestBody::Text(String)` /
  `.body(RequestBody::Text(serde_json::to_string(&req)?))`) and parses responses with `serde_json::from_str` instead
  of going through `JsJson`/`AutoJsJson` at all. This keeps `kima-api-types` a plain `serde` crate usable unmodified
  by both sides, at the cost of a thin `kima-web`-local wrapper around `RequestBuilder` that does the `serde_json`
  encode/decode and maps a non-2xx response into the shared `ApiErrorBody`. That wrapper (`kima-web::api::client`)
  is a Task in the plan (Task 5) and is the only place vertigo's fetch API and `serde_json` meet.
- Rejected: deriving `AutoJsJson` on the shared types (would force `kima-api-types` to depend on `vertigo_macro`
  and, transitively on the backend side, pull a frontend macro crate into the server's dependency tree — avoidable
  for zero benefit, since `serde_json::from_str::<T>` is just as correct and the backend needs serde regardless).

### 4.3 What does not fit the shared-type model (named now, not discovered mid-implementation)

- **Timestamps:** confirm during Task 1 whether `kima-api`'s existing DTOs serialize `time::OffsetDateTime` as
  RFC3339 strings (consistent with `time::format_description::well_known::Rfc3339` seen in `routes/auth.rs`) or as
  something else; `kima-api-types` standardises on whatever the backend already emits, not a new format.
- **IDs:** `uuid::Uuid` with its `serde` feature, matching current behaviour; no change.
- **Error details:** `ApiError::InvalidWithDetails`'s `details: Vec<serde_json::Value>` stays loosely typed
  (`serde_json::Value`) in `kima-api-types::error` rather than being given a type per call site — that would be
  scope creep for M9.
- **Anything this milestone's pages don't call** stays wherever it is; `kima-api-types` only grows to cover login,
  onboarding, library browse/search, player/playback-state, playlists, and the Settings slice in §5.7. M10 extends
  it page by page, the same way M9 does.

## 5. Pages in scope

Each page is a `vertigo` component (`#[component]` function + a `render`) with its own module under
`crates/kima-web/src/pages/`. Routing is the hand-written enum from §3 (`crates/kima-web/src/route.rs`), matching
the paths the Next.js app already uses under `frontend/app/` so links, deep-link tests, and the owner's muscle
memory don't change:

1. **Login** (`/login`) — username/password form, calls `POST /auth/login`, stores the returned token (storage
   mechanism decided in Task 7, mirroring whatever `frontend/lib/auth-context.tsx` does today: survives reload,
   cleared by `/auth/logout` and on 401), redirects to the library root on success, shows the backend's error
   message verbatim (one more place the shared `ApiErrorBody` pays for itself) on failure, and reflects the login
   rate limiter's 429 (`Retry-After`, D-104/D-105) rather than a generic error.
2. **Onboarding** (`/setup`, matching the Next.js `FIXED_REDIRECTS` target `/login/` only insofar as `/setup` is a
   legacy alias — see `kima-server::frontend::FIXED_REDIRECTS`) — shown only while there is no admin user yet
   (`GET /auth/users` or whatever the backend exposes as "needs setup"; confirmed in Task 8, not assumed); creates
   the first admin account and signs in.
3. **Library browse** (`/collection/` equivalent — artists, albums, tracks lists with pagination) — reuses the
   list/pagination DTOs from `kima-api-types::library`; infinite scroll or paged nav decided in Task 10 to match
   whatever the current Next.js UX does closely enough that the Playwright port (§7) doesn't need new assertions.
4. **Search** (`/search` equivalent) — calls `GET /api/search` (or whatever `routes/search.rs` exposes), debounced
   input, results grouped by artist/album/track as the backend already groups them.
5. **Player** — not a route of its own but a persistent component (mirrors the Next.js app's global audio bar):
   transport controls, current track, queue view, progress scrubber, calls `playback_state` endpoints to persist
   position (matching M1's playback-state feature) and streams audio from the existing `/api/library/tracks/:id/
   stream` endpoint through a plain HTML `<audio>` element (`dom!`'s `<audio>` tag with `src`, matching the Next.js
   player's base case — no transcode-negotiation UI changes in scope).
6. **Playlists** (`/browse/playlists/` list + `/browse/playlists/view/` detail) — list, open, reorder/remove tracks,
   create; calls `kima-api-types::playlists`.
7. **Settings** — only the slice already needed to run the rest of M9's pages end to end: account (username/
   password change — reuses `auth::change_password`), and whatever subset of `/system-settings` the owner needs
   visible this early (confirmed in Task 16; defaults to nothing beyond account settings if nothing else is needed
   for an onboarded single-admin instance to be usable — identification/artwork/autotag/organizer settings stay on
   the Next.js side until M10, same as every other out-of-scope page).

Non-page concerns shared across all of the above: an auth guard component (redirect to `/login` on 401, matching
`frontend/lib/auth-context.tsx`'s behaviour), an SSE/event bridge is **not** in scope for M9 (none of these pages
require it to be minimally usable; polling is an acceptable M9 fallback where the Next.js page uses SSE, called out
per-page in the plan) and a shared `Resource<T>`-based loading/error presentation (one component, `§4.2`'s client
feeding it).

## 6. Production serving (SSR, embedded) and the dev-only legacy frontend

**Owner decision, 2026-11-05 — supersedes the original "switch" design below.** There is no runtime switch, no
dual-serve, and no user-visible toggle in production. `kima-web` is the only frontend `kima-server` serves to real
users, with the pages it covers rendered server-side via an embedded wasmtime runtime. Next.js exists only as an
opt-in local developer build (§6.2), never in the production image.

### 6.1 Production: `kima-web` only, served via embedded SSR

**Chosen (per §3's research): `kima-server` spawns `vertigo_cli::serve::run()` as a background `tokio::task` at
startup, bound to a loopback-only address (e.g. `127.0.0.1:<internal-port>`, configurable, never exposed on the
public listener), and its own axum app reverse-proxies requests for the paths `kima-web` covers (§5's allow-list —
login, onboarding, browse, search, player, playlists, the Settings slice) to that loopback address, returning the
wasmtime-rendered HTML (and, on the client, the same `.wasm`/`wasm_run.js` the embedded server serves) to the real
caller.**

Mechanics (detailed in the plan):
- `kima-web` builds (via `vertigo build`, §3) to a `build/` directory at image-build time, same as before.
- At `kima-server` startup, a background task calls `vertigo_cli::serve::run(ServeOpts { ... }, None)` pointed at
  that `build/` directory, bound to loopback only. This is real, unmodified `vertigo-cli` code — no fork, no
  vendoring (§3's conclusion).
- `kima-server`'s own axum router gets a reverse-proxy layer (a thin HTTP client forwarding method/headers/body,
  not unlike the `--proxy` flag `vertigo serve` already has for its own API calls, mirrored here in the other
  direction) for exactly the path set §5 covers; everything else is `kima-server`'s existing routes/404 — **no
  fallback to Next.js exists in production** (§1).
- `/api/*` is unaffected: `kima-web` calls the same backend either way, which is the entire point of §4.
- Startup ordering matters now in a way it did not under the old switch design: the embedded SSR task needs the
  `build/` output to exist before it binds, and `kima-server`'s readiness/healthcheck should not report ready
  until the embedded task has successfully bound its loopback port (plan Task-level detail, not decided here).

Rejected alternatives (updated for the new mandate):
- **CSR-only static `ServeDir`** (the original plan): rejected by the owner — production must have real SSR.
- **Vendoring vertigo-cli's private SSR internals for a literal single-`Router` embed**: rejected for M9 per §3's
  finding — unstable private API, high fork-maintenance cost, not worth it yet.
- **Sidecar as a second OS process/container**: rejected — the loopback-task design (§3, above) gets real SSR
  without paying that cost; a true external sidecar would need its own lifecycle/healthcheck and would be a
  starker break from D-006 than the chosen design.
- **Separate subdomain**: rejected — irrelevant now that there is only one frontend in production; no same-origin
  concern to solve.

### 6.2 Dev-only: optional legacy Next.js build via a `Dockerfile` `ARG`

**Chosen: a build-time `Dockerfile` `ARG` (`KIMA_BUILD_LEGACY_FRONTEND`, default unset/`0`) that, when set,
additionally runs the Next.js static export and bakes it into the image at a distinct, non-production path, purely
so a developer migrating the remaining M10 pages can run the old and new frontends side by side locally for visual
comparison.** This is *not* a runtime feature and is *not* wired into `kima-server`'s production routing:
- When the `ARG` is unset (the default, and always the case for any image CI builds as "production"), the
  Next.js build step is skipped entirely — no Node toolchain run, no `frontend/` assets baked in, matching the
  owner's goal of a Node-free production image starting at M9, not M10.
- When the `ARG` is set (local `docker build --build-arg KIMA_BUILD_LEGACY_FRONTEND=1 ...`), the image additionally
  contains the Next.js export, and `kima-server` serves it only when a corresponding runtime env var confirms the
  assets are present (e.g. `KIMA_LEGACY_FRONTEND_PATH` pointing at the baked-in directory) at a clearly separate,
  non-colliding mount point (e.g. `/__legacy/`) — never at `/`, never overlapping any `kima-web` route, and never
  reachable unless both the build-time `ARG` and the runtime env var line up (belt-and-suspenders against it ever
  leaking into a production deployment by accident).
- No runtime switch, no cookie/header override, no "flip back without a redeploy" mechanic — if the dev build
  wants to compare, it loads `/__legacy/...` and the real `kima-web` pages side by side in two browser tabs. This
  is deliberately simpler than the original §6 switch design because there is no production audience for it.

Rejected alternatives:
- **Keeping the full request-scoped runtime switch** (the original design): rejected — it solved a dual-serve
  production problem that no longer exists; keeping it would be unneeded complexity for a dev-only comparison
  need.
- **A separate `docker-compose` service for the legacy frontend**: rejected for the same reason the embedded-SSR
  design was chosen over a sidecar (§6.1) — a single image with an opt-in build flag is simpler to reason about
  and keeps the "one binary" property intact even for the dev-comparison build (the legacy assets are static files
  served by the same `kima-server` binary, not a second process).

## 7. Playwright: `kima-web` is the production suite; the legacy suite moves to the dev-flag build

**Chosen: the `kima-web` spec tree is the one CI treats as defining production correctness; the existing Next.js
suite keeps running, but only against a `KIMA_BUILD_LEGACY_FRONTEND`-enabled build, and is no longer what gates a
production release.**

- `frontend/tests/e2e/kima-rs/*.spec.ts` (the existing suite: `smoke`, `library`, `autotag`, `enrichment`,
  `subsonic`) is unchanged in content, but its CI job now builds the image with `KIMA_BUILD_LEGACY_FRONTEND=1` and
  points at the `/__legacy/` mount (§6.2) — it is a **dev-build regression net for pages not yet ported**, not a
  production gate. If `KIMA_BUILD_LEGACY_FRONTEND` is ever dropped from the dev build entirely (post-M10), this
  suite is deleted along with `frontend/`, same as the roadmap always intended.
- `frontend/playwright.kima-web.config.ts` (sibling to the existing `playwright.kima-rs.config.ts`) runs against a
  normal **production-shaped** `kima-server` build (no legacy ARG set), exercising `kima-web`'s pages exactly as a
  real deployment serves them — through the embedded-SSR reverse proxy (§6.1), not a CSR-only shortcut, so the
  suite actually exercises the SSR path it is meant to guard. `frontend/tests/e2e/kima-web/` holds the login/
  onboarding/browse/search/player/playlists/settings specs, written against `kima-web`'s markup (vertigo's DOM
  output will not have the same class names/structure as the React components, so these cannot be byte-identical
  copies — they port the same user-visible assertions, not the same selectors). `smoke.spec.ts`'s kima-web
  counterpart is the first one written (plan Task, template for the rest).
- CI (`.github/workflows/ci.yml`) gains two separate jobs with two separate image builds: one without the legacy
  ARG (runs `playwright.kima-web.config.ts` — the production gate) and one with it (runs the existing `kima-rs`
  suite against `/__legacy/` — the dev-build regression net). They do not share a running `kima-server` instance
  the way the original side-by-side design assumed, because production and dev-flag builds are now different
  images by design (§6.2). A `kima-web` spec that cannot yet pass because its page isn't ported stays absent from
  the suite rather than skipped-and-ignored (no `test.skip` debt).

## 8. Divergence log

Roadmap M9 note: "Log the frontend replacement in DIVERGENCES.md when it lands." Upstream kima-hub has no Rust/WASM
frontend at all, so this is a kima-rs-only addition, not a behavioural difference in any API response — it gets
**one** row, added once the switch is wired end to end and at least one page works (last task of the plan, not
earlier, so the row describes what actually landed instead of what was planned):

- **D-134** | Frontend | No Rust/WASM frontend; the Next.js app is the only UI | A second frontend, `kima-web`
  (vertigo + vertigo-cli, compiled to WASM, server-side rendered in production via an embedded wasmtime runtime
  inside `kima-server` — §3, §6.1), covers login/onboarding/library browse/search/player/playlists/settings and
  is the **only** frontend served in production; Next.js is fully retired from production (no dual-serve, no
  switch) and survives only as an opt-in `Dockerfile` `ARG` (`KIMA_BUILD_LEGACY_FRONTEND`, §6.2) for local
  dev-time comparison during the M10 porting work; any page `kima-web` does not yet cover is not served in
  production until M10 ports it | Owner request, revised 2026-11-05: replace the forked Next.js frontend with
  Rust/WASM outright for the pages M9 covers, not side-by-side (roadmap M9); both frontends call the same `/api`
  so there is no backend behaviour change to log separately | open

This ID and row text are a draft for the plan's final task to confirm/adjust against whatever actually shipped
(the same convention M3a/M3b used — see `docs/superpowers/plans/2026-10-27-m3b-autotagger.md`'s rule: "a difference
§23 does not list gets the next free ID" — here, D-134 is reserved by this spec itself since no earlier plan
claimed it; check the file's tail before use in case another in-flight milestone took it first).

## 9. Testing strategy

- **`kima-api-types`**: round-trip tests (`serde_json::to_string`/`from_str`) against fixed JSON fixtures captured
  from the current backend's real responses (one fixture per type, committed under `crates/kima-api-types/tests/
  fixtures/`), so a future accidental rename is caught without needing a running server.
- **`kima-web` unit-level**: vertigo component tests following `crates/vertigo/src/tests/dom/component.rs`'s
  pattern (vertigo ships a DOM-diffing test harness for exactly this) for anything with branching render logic
  (route matching, auth-guard redirect, Resource loading/error states) — these run under `cargo test`, no browser,
  no wasm target needed (vertigo's own test suite runs natively).
- **End-to-end**: Playwright, §7. This is the only place `kima-web`'s actual WASM build is exercised in a real
  browser, which matches how the Next.js frontend is tested today (no component-level browser tests there either).
- **No backend test changes** are expected from this milestone beyond the mechanical DTO relocation (§4.1); if a
  relocation surfaces a DTO that was accidentally diverging from its own OpenAPI-less "spec" (e.g. an undocumented
  optional field), that is logged as its own `DIVERGENCES.md` row when found, not silently fixed.

## 10. Global constraints (carried forward, unchanged)

GPL-3.0-only; edition 2024 for every native crate (`kima-api-types` included); `kima-web` targets
`wasm32-unknown-unknown` and therefore cannot use `rust-version`-gated native-only features — confirm the toolchain
`vertigo` 0.6.3 requires (tutorial.md §1: **nightly**, for trait aliases) does not regress kima-rs's pinned stable
toolchain for the native crates; `kima-web` may need its own `rust-toolchain.toml` override scoped to that crate's
directory if `vertigo` still requires nightly (confirmed in Task 0 against the installed `vertigo` version, not
assumed from the tutorial alone — the tutorial is marked "up to date with version 0.6.3", matching the vendored
checkout, so this is likely but must be checked against the real `cargo build` error, if any, before committing to
it). camelCase JSON on `/api`, errors `{"error": "<message>"}` (unchanged — `kima-api-types::error` mirrors this,
does not redefine it). Commits: `dimmu <3417783+d1mmu-dev@users.noreply.github.com>`, no AI/Claude attribution
trailers, Conventional Commits style. `docs/DIVERGENCES.md` row added in the same commit as the change it records
(§8 — here, the final integration task). No test reaches a real network; Playwright against `kima-web` runs with
`KIMA_IDENTIFY=off` like the existing suite.

## 11. Risks

- **vertigo is a small, single-maintainer-style crate** (two authors, 0.6.x) compared to Next.js/React's ecosystem;
  this milestone is also this project's first real exercise of it beyond the tutorial/examples, so velocity on
  later pages (M10) should be expected to be slower than it looks from M9's seven pages, which were picked to be
  the simplest complete slice (no SSE, minimal nested routing, no complex forms-with-many-fields yet).
  Settings is deliberately the shallowest possible slice (§5.7) to avoid importing that risk into M9 itself.
- **Embedded SSR via a loopback-proxied `vertigo_cli::serve::run()` task (§3, §6.1) is new integration surface**:
  it pulls `vertigo-cli`'s own axum 0.6/poem/reqwest/tokio-full dependency stack into `kima-server`'s binary
  alongside its axum 0.8 stack (bigger binary, longer build, two independent HTTP stacks in one process); the
  loopback hop adds a request-scoped dependency on an internal task's liveness that the original CSR-only design
  did not have (if the embedded task dies, every `kima-web` page in production breaks, not just degrades to a
  CSR shell) — the plan's SSR-embedding task needs its own liveness/health check and restart-on-crash story, not
  just a happy-path "spawn it once".
- **No Next.js fallback in production for unported pages (§1)** means every page outside M9's seven
  (podcasts, autotag UI, organizer UI, sharing, stats, vibe search, etc.) is simply unavailable in a deployed M9
  build until M10 ports it — an explicit, owner-accepted trade for dropping dual-serve complexity, but a real
  user-facing regression for the interim period that should be called out plainly in any release notes, not just
  in this spec.
- **DTO relocation (§4.1) touches many backend route files** even though it changes no behaviour; the plan isolates
  this into its own early, narrowly-reviewable task (Task 1) with the round-trip fixture tests as the safety net,
  specifically so it can be reviewed and landed independently of anything about vertigo.

## 12. Open questions for the owner

Resolved by the owner's 2026-11-05 decisions (kept here for the record, not left open):
- ~~Production SSR for `kima-web`~~ — **resolved: real SSR, embedded wasmtime in-process via a loopback-proxied
  background task (§3, §6.1)**, not CSR-only and not an external sidecar.
- ~~Switch mechanism exposure~~ — **resolved: no switch exists.** Next.js is dev-only behind a `Dockerfile` `ARG`
  (§6.2); nothing related is ever user-facing in production.
- ~~Settings scope~~ — **resolved: minimal placeholder (account/password only) for M9, confirmed as-is; full
  Settings page is M10's job, unchanged from the original plan.**

Still open:
1. **Token/session storage in `kima-web`.** §5.1 defers the exact mechanism to the plan's auth-guard task, matching
   whatever `auth-context.tsx` does today (likely `localStorage` or a cookie). With Next.js no longer running
   concurrently in production, the original "does flipping the switch mid-session log the user out" concern (old
   §12 Q3) is moot for production; it still applies narrowly to the dev-flag build (§6.2) where a developer might
   have both `kima-web` and the legacy `/__legacy/` Next.js open in adjacent tabs — confirm whether an isolated
   `kima-web`-only storage key (recommended default, since the two are genuinely separate apps now, not a single
   switchable session) is acceptable, or whether sharing the storage key/shape is still wanted for that dev
   comparison workflow.
2. **OpenAPI, for real, later.** §4 rejected OpenAPI generation for M9 as premature. Once `kima-web` covers more of
   the surface in M10, is generating `kima-api-types` from a `utoipa` spec (rather than hand-writing it) worth
   doing as a cleanup pass, or does the owner prefer hand-written DTOs to stay the house style indefinitely?
3. **Embedded-SSR liveness/restart policy (new, raised by §11's risk).** If the loopback-bound `vertigo_cli::
   serve::run()` task panics or its bind fails after `kima-server` has already reported ready, should
   `kima-server` crash-and-restart itself (simplest, relies on the container orchestrator's restart policy),
   retry the embedded task in place, or serve a plain error page for `kima-web`'s routes while keeping `/api`
   healthy? This changes the SSR-embedding task's shape materially and should be settled before that task starts
   implementation.
