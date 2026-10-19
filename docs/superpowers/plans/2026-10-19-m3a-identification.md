# M3a Identification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Without the owner doing anything, every identifiable track carries its MusicBrainz recording id, artist pages show a bio, a photo and similar artists, the lyrics panel shows synced lyrics, Subsonic apps get genres and artist info, and the Library Enrichment panel shows real progress, failures and working pause/resume/stop. With `KIMA_IDENTIFY=off`, kima makes no outbound request at all.

**Architecture:** Two new crates. `kima-net` is the only outbound HTTP path: per-source limits, a Postgres response cache, persisted source pauses and upstream's `Outcome` type. `kima-ident` holds every music-domain decision: the source clients, chromaprint, upstream's text rules and identification ladder, the `track_stages` ledger, the enrichment coordinator, artist enrichment and lyrics. `track_stages` is itself the work queue; one `cycle` job on a new `enrich` queue drains it, with job leases kept alive by a new heartbeat. The scanner learns to read MusicBrainz ids, ISRC, genres and embedded lyrics. Settings move into an encrypted key-value store whose secrets are write-only.

**Tech Stack:** Rust 2024, axum 0.8, sqlx 0.8, tokio, Postgres 17, lofty 0.25.4, image 0.25. New: `reqwest` 0.12 with `rustls-tls`, `json` and `gzip` (in `kima-net` only), `rusty-chromaprint` 0.3, `regex` 1, `percent-encoding` 2, `md-5` 0.11 (already in the workspace; used for Wikimedia paths). The forked Next.js 16 frontend (static export), vitest and Playwright.

**Spec:** `docs/superpowers/specs/2026-10-19-m3-identification-design.md` (binding; this plan covers **M3a only**, spec §4). Parent spec: `docs/superpowers/specs/2026-10-16-kima-rust-design.md`. Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`. Divergence log: `docs/DIVERGENCES.md` (highest row today: D-105; the M3a rows D-106–D-118 and D-124 are drafted in spec §23).

**Upstream reference:** kima-hub @ `12faf1cd34ff53054221818b0ccde58b8e9b0020`, cloned read-only at `/tmp/kima-hub`. If it is missing: `git clone https://github.com/Chevron7Locked/kima-hub /tmp/kima-hub && git -C /tmp/kima-hub checkout 12faf1cd34ff53054221818b0ccde58b8e9b0020`. "Port X" means: read X under `backend/src/` and reproduce its cases as Rust tests with the same inputs and expected values, named after the upstream `it(...)` text in snake case.

**What M3b consumes from this plan** (spec §4): the MusicBrainz client (`kima_ident::musicbrainz`, Task 15), `tracks.recording_mbid`/`recording_source` (Task 17), `track_stages` and `kima_db::stages` (Tasks 10, 13), the job heartbeat, `JobCtx` cancel token and `JobError::Permanent` (Task 3), `enqueue_at` (Task 3), the scanner's MBID, total and ISRC columns (Tasks 8–10), and `kima_ident::text` (Task 12). M3b adds `ReadCause::TagWrite`, migrations 0021–0022 and D-119–D-123 (D-122 partly, see Task 30).

## Global Constraints

- Everything in M0's, M1's and M2's Global Constraints still holds: GPL-3.0-only, edition 2024, `rust-version = "1.85"`, every version pinned in the workspace `Cargo.toml`; camelCase JSON on `/api` with errors as `{"error": "<message>"}`; UUIDv7 ids; the database is never mocked (`#[sqlx::test(migrator = "kima_db::MIGRATOR")]`).
- **Before every commit:** `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` against the dev Postgres on `localhost:5456` (`DATABASE_URL=postgres://kima:kima@localhost:5456/kima`), all clean.
- **sqlx:** compile-checked `query!`/`query_as!`/`query_scalar!` for every new query. After any query change run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/` in the same commit. CI builds with `SQLX_OFFLINE=true`.
- **Commits:** author and committer `dimmu <3417783+d1mmu-dev@users.noreply.github.com>` (the repo's local git config). **No `Co-Authored-By` or any AI attribution line.** Conventional style (`feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `chore:`).
- **Divergence rows:** every difference from upstream goes into `docs/DIVERGENCES.md` in the same commit as the change that introduces it. Each task's **Divergences:** line names the rows its commit writes or extends. Copy ID, area and the upstream/kima-rs columns verbatim from spec §23; add a one-line "Why" from the spec section or controller's call the row cites, and status `open`. Where a row spans tasks, the first task writes the clauses its change introduces and later tasks extend the same row, so that after the last named task the row reads exactly as in §23. A difference §23 does not list gets the next free ID after D-124 (D-119–D-123 are reserved for M3b).
- **No test reaches the network.** Every outbound call goes through `kima-net`, whose base URLs are injected (`NetConfig`). Tests point them at `kima_net::testing::Replay`. CI sets `KIMA_NET_DENY_REAL=1` on the Rust job (Task 6), which makes `kima-net` panic on any host other than `127.0.0.1`, `localhost` or `::1`. Fixtures are recorded by hand with `tools/record-fixtures` (Task 7), never in CI.
- **ffmpeg is required in CI.** Every ffmpeg-dependent test starts with `if !kima_test_support::require_ffmpeg() { return; }` (Task 2): it skips locally without ffmpeg and panics under `CI`.
- **Playwright and Docker runs set `KIMA_IDENTIFY=off`** (CI e2e job from Task 18, the wrap-up's container run), so no browser test triggers a real lookup.
- **Crate boundaries:** `kima-net` depends on `kima-core` and `kima-db` only, and knows nothing about music. `kima-ident` depends on `kima-net`, `kima-db`, `kima-jobs`, `kima-library`, `kima-core`, never on axum. `kima-library` never depends on `kima-net` or `kima-ident`. `kima-api` may depend on `kima-net` and `kima-ident`; `kima-subsonic` may depend on `kima-ident`. `kima-db` gains a dependency on `kima-core` (Task 5) for the cipher.
- **Outbound identity:** User-Agent `kima-rs/<CARGO_PKG_VERSION> ( <KIMA_CONTACT> )` on every request, images included. No lookup takes a string id: MusicBrainz ids are `kima_ident::Mbid` (a parsed `uuid::Uuid`). Only answers (`Answered`) and absences (`Absent`) are cached; failures never.
- **Never echo a stored secret.** `acoustidApiKey` and `fanartApiKey` are write-only; no response, log line or `http_cache.request` value contains one.
- **Migrations** `0015`–`0020` are spec §6.1–§6.6 verbatim (Task 5: 0015, Task 6: 0016, Task 8: 0017–0020), plus the two genre views §6.5 abbreviates, written out.
- **New env vars** (`kima_core::config`, README in Task 32), exact names and defaults from spec §22: `KIMA_IDENTIFY` (`on`; `off`; anything else fails startup), `KIMA_CONTACT` (`https://github.com/d1mmu-dev/kima-rs-hub`), `MUSICBRAINZ_BASE_URL` (`https://musicbrainz.org/ws/2`), `ACOUSTID_CLIENT_KEY` (unset), `ACOUSTID_DISABLED` (`false`), `FANART_API_KEY` (unset), `KIMA_FINGERPRINT_WORKERS` (`max(1, available_parallelism / 2)`).
- **Stage constants** (spec §10, upstream names): claim lease 15 min, redelivery cap 3, retry cap 4, retry delays 1 min / 10 min / 1 h, stage timeout 30 s, `last_error` at most 2000 characters, batch 20 tracks / 10 artists.
- **AcoustID default key is the empty string** (spec §2, §27 Q1). Never copy upstream's key.

## Review Focus

1. **Identification switched off, by `KIMA_IDENTIFY=off` or by the "Auto enrich metadata" toggle.** A full enrichment cycle over tagged and untagged tracks, a lyrics request, a `test-fanart` press and a settings save must make zero outbound requests, while fingerprints are still computed. The test lives in Task 18 (`identify_off_makes_no_request`), with the per-route checks in Tasks 20 and 24.
2. **Tags whose MusicBrainz id fields hold something else:** a braced `{uuid}`, `urn:uuid:…`, 32 hex digits without hyphens, the nil UUID, two ids in one single-valued field, or an AcoustID id. None of them is stored or ever sent to a source; only a bare 36-character hyphenated UUID (any case) other than nil counts. The test lives in Task 9 (`mbid_junk_dropped`).
3. **Titles and artist names that break a Lucene query or a URL:** `Don't Stop "Live" (Remix)`, `AC/DC`, `Guns N' Roses & Friends?`, `#1 Crush`, `東京事変`. The search request must carry exactly the escaped, percent-encoded query, and the replay server must see one well-formed request per query. The test lives in Task 15 (`search_query_escapes_lucene_and_url`).
4. **A track purged, re-pointed or re-tagged while its stage is claimed.** The late report must write nothing (no `recording_mbid`, no genre rows, no stage row resurrected), raise no error, and let the batch go on with the next track. The test lives in Task 17 (`track_purged_mid_claim_writes_nothing`, `file_changed_mid_claim_is_dropped`).
5. **File names ffmpeg could misread:** a basename starting with `-`, one containing `:`, and (on Linux) one with non-UTF-8 bytes. Fingerprinting must decode them like any other file. ffmpeg always gets `file:` followed by the absolute path. The test lives in Task 11 (`odd_file_names_fingerprint`).

---

### Task 1: M2 leftovers: limiter fold keys and divergence wording

**Files:**
- Modify: `crates/kima-api/src/login_limit.rs`, `crates/kima-api/src/routes/auth.rs` (`login`), `crates/kima-subsonic/src/auth.rs` (password path), `crates/kima-api/tests/login_limit.rs`, `crates/kima-subsonic/tests/auth.rs`, `.github/workflows/ci.yml`, `docs/DIVERGENCES.md` (D-104, D-105)
- Test: unit tests in `login_limit.rs`, plus the two integration files above

**Divergences:** none new; D-104 and D-105 reworded (spec §25.3).

**Interfaces:**
- Consumes: `LoginLimiter::{check, begin}`, `Attempt::success`, `users::sign_in_lookup` (M2).
- Produces: `LoginLimiter::begin_all(&self, ip: IpAddr, names: &[&str], now: Instant) -> Result<Vec<Attempt<'_>>, Blocked>`. It checks every distinct key first. When any key is blocked it returns the longest `Blocked` and charges nothing; otherwise it charges each distinct key once.

- [ ] **Step 1: Failing unit tests** in `login_limit.rs`:
  - `begin_all_blocked_second_key_charges_nothing`: block `b` with 5 failures; call `begin_all(&["a","b"])` 10 times (all `Err`); then `a` takes 4 more failures and `check(a)` is still `Ok`.
  - `begin_all_blocked_first_key_charges_nothing`: the same with `["b","a"]`.
  - `begin_all_charges_each_distinct_key_once`: `begin_all(&["a","a"])` counts one failure on `a`.
- [ ] **Step 2: Rewrite the two fold tests** (`fold_variant_failures_answer_the_same_for_known_and_unknown_names`, `database_fold_key_is_charged_whether_or_not_the_account_exists`, in both crates) so they hold under any database locale: the final probe uses a **wrong** password and signs in as `db_fold(noisy)` (the database's own `lower()` of the noisy name). Assert, without any `if`: the known and unknown answers are equal, and both are 429 on `/api` (41 on `/rest`).
- [ ] **Step 3: Run** `cargo test -p kima-api --test login_limit && cargo test -p kima-subsonic --test auth`. Expected: the new unit tests fail to compile.
- [ ] **Step 4: Implement.** `login` and the Subsonic password path call `check(ip, typed)` first (no charge, before any database work), then `sign_in_lookup`, then `begin_all(ip, &[typed, fold])` (the fold only when it differs, as today). Both `Attempt`s get `success()` on a good password.
- [ ] **Step 5: CI under both locales.** Add a second service `postgres-c` (`pgvector/pgvector:pg17`, port 5433, `POSTGRES_INITDB_ARGS: "--locale=C --encoding=UTF8"`) to the `rust` job, and two steps after `cargo test --workspace`: `DATABASE_URL=postgres://kima:kima@localhost:5433/kima cargo test -p kima-api --test login_limit` and the same for `-p kima-subsonic --test auth`.
- [ ] **Step 6: Docs.** D-104: read `routes/auth.rs::login` and `kima-subsonic/src/auth.rs` and state exactly which paths trim the username. D-105: the strict list names bookmark `position` (`createBookmark`), not the play-queue `position` (read `routes/queue.rs::position` and state what it does), and the `size=10.5` sentence reads "falls back to the endpoint default (upstream reads 10)".
- [ ] **Step 7: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 8: Commit** with message `fix(auth): a blocked fold key charges nothing; fold tests hold under the C locale`. Stage `docs/DIVERGENCES.md`.

### Task 2: M2 leftovers: Subsonic minors and `kima-test-support`

**Files:**
- Create: `crates/kima-test-support/Cargo.toml`, `crates/kima-test-support/src/lib.rs`
- Modify: root `Cargo.toml` (workspace dep `kima-test-support = { path = "crates/kima-test-support" }`), the `[dev-dependencies]` of `kima-api`, `kima-library`, `kima-subsonic`; `crates/kima-api/tests/common/mod.rs`, `crates/kima-library/tests/common/mod.rs`, `crates/kima-subsonic/tests/common/audio.rs` (local `ffmpeg_guard`/`ffmpeg_available` removed, `require_ffmpeg` re-exported), `crates/kima-library/tests/tags.rs` (the two guard tests move), `crates/kima-db/src/stars.rs`, `crates/kima-db/src/subsonic.rs`, `crates/kima-db/src/users.rs`, `crates/kima-subsonic/src/routes/{stars.rs, bookmarks.rs, stubs.rs, users.rs}`
- Test: unit tests in `kima-test-support`, `crates/kima-subsonic/tests/{scrobble.rs, stars.rs, users.rs}`, `crates/kima-db/tests/subsonic.rs`

**Divergences:** none.

**Interfaces:**
- Produces:
  - `kima_test_support::{ffmpeg_available() -> bool, ffmpeg_guard(available: bool, ci: bool) -> bool, require_ffmpeg() -> bool}`, moved verbatim from `kima-library/tests/common`. The crate is `publish = false` and is only ever a dev-dependency.
  - `kima_db::stars::set` returns `Ok(false)` on a foreign-key violation (SQLSTATE `23503`: the item was purged between the check and the insert); the endpoint maps it to 70.
  - `kima_db::subsonic::kinds_of(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, StarKind>>`: one query over tracks, albums and artists. `star`/`unstar` use it instead of one `kind_of` per id.
  - `kima_db::subsonic::artist_exists(pool: &PgPool, id: Uuid) -> sqlx::Result<bool>`: `EXISTS`, with the visibility rule of `artist_with_albums` (the artist has a present track). `known_artist` uses it.
  - `kima_db::users::set_role_unless_last_admin_in(conn: &mut PgConnection, id: Uuid, role: &str) -> sqlx::Result<RoleChange>`; the pool version delegates to it.

- [ ] **Step 1: Failing tests:**
  - `ffmpeg_guard_panics_in_ci_without_ffmpeg` (`#[should_panic(expected = "ffmpeg is required in CI")]`) and `ffmpeg_guard_skips_outside_ci`, now unit tests in `kima-test-support`.
  - `now_playing_hides_missing_track` (scrobble.rs): `submission=false` for a track, mark it missing, `getNowPlaying` lists no entry for it.
  - `set_fk_violation_is_false` (db): `stars::set` for a random track id that no row has returns `Ok(false)`.
  - `kinds_of_mixed_ids`: one track, one album, one artist and one unknown id give three entries.
  - `star_many_ids_one_kind_query`: `star` with 3 ids of mixed kinds stars all three.
  - `update_user_role_and_password_is_one_transaction` (users.rs): as an admin with password auth, `updateUser` of the only admin with `adminRole=false&password=newpass1` answers 50 `Cannot remove the last admin`, and the target's password hash is unchanged.
- [ ] **Step 2: Run** `cargo test -p kima-test-support -p kima-db -p kima-subsonic`. Expected: fail.
- [ ] **Step 3: Implement.** `getBookmarks` builds a `HashMap<Uuid, SongItem>` from one `tracks_by_ids` call. `updateUser` runs the role change (`set_role_unless_last_admin_in`) and `set_password(&mut tx, …)` in one transaction and commits once.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** with message `refactor: M2 review minors; shared kima-test-support crate`.

### Task 3: Jobs: `JobError`, `JobCtx`, lease heartbeat, delayed enqueue

**Files:**
- Modify: `crates/kima-jobs/src/{queue.rs, worker.rs}`, `crates/kima-server/src/jobs.rs` (`LIBRARY_LEASE` deleted), `crates/kima-server/tests/library_jobs.rs`, `crates/kima-library/src/jobs.rs`, `crates/kima-library/src/scan.rs`, `crates/kima-library/src/organize/apply.rs`
- Test: `crates/kima-jobs/tests/{queue.rs, worker.rs}`, `crates/kima-library/tests/scan.rs`, `crates/kima-server/tests/library_jobs.rs`

**Divergences:** none.

**Interfaces:**
- Consumes: `queue::{claim, complete, fail, reap}`, `worker::spawn_worker` (M0).
- Produces:
  - `kima_jobs::worker::JobError { Retry(String), Permanent(String) }`, with `From<String>` (= `Retry`) and `Display`.
  - `kima_jobs::worker::JobCtx { pub cancel: CancellationToken }` (Clone). The token fires on shutdown and when the claim is lost.
  - `Handler` **changes** to `Arc<dyn Fn(Job, JobCtx) -> BoxFuture<'static, Result<(), JobError>> + Send + Sync>`. `worker::handler(f)` keeps its signature (`Fn(Job) -> Fut<Result<(), String>>`) through an adapter. New `worker::handler_ctx<F, Fut>(f: F) -> Handler` where `F: Fn(Job, JobCtx) -> Fut`, `Fut: Future<Output = Result<(), JobError>>`.
  - `WorkerConfig.heartbeat: Duration`, `lease / 4` in `new()`.
  - `queue::renew(pool, job: &Job, lease: Duration) -> sqlx::Result<bool>`: `UPDATE jobs SET locked_until = now() + lease WHERE id = $1 AND state = 'running' AND attempts = $2`.
  - `queue::fail_permanent(pool, job: &Job, error: &str) -> sqlx::Result<bool>`: `failed` at once, same guard as `fail`.
  - `queue::enqueue_at(pool, queue: &str, payload: Value, max_attempts: i32, run_at: OffsetDateTime) -> sqlx::Result<Uuid>`; `enqueue` delegates with `now()`. `pg_notify` only when `run_at <= now()`.
  - `queue::enqueue_at_unless_queued_with_key(pool, queue, payload, max_attempts, key: &str, run_at: OffsetDateTime) -> sqlx::Result<Option<Uuid>>`; `enqueue_unless_queued_with_key` delegates with `now()`.
  - `queue::reschedule_queued(pool, queue: &str, kind: &str, run_at: OffsetDateTime) -> sqlx::Result<bool>`: moves the oldest `queued` job whose payload `kind` matches to `run_at`; notifies when due. `false` when there is none.
  - `kima_library::scan::reconcile_cancellable(ctx: &LibraryCtx, cancel: &CancellationToken) -> Result<ScanReport, ScanError>`, checking `cancel` between folders; new `ScanError::Cancelled`. `reconcile(ctx)` delegates with a fresh token.
  - `kima_library::organize::apply::run_job_cancellable(ctx, run_id, action, cancel: &CancellationToken) -> Result<()>`: the runner's `checkpoint()` also returns `OrganizeError::Interrupted` once `cancel` fired. `run_job` delegates.
  - `kima_library::jobs::run(ctx: &LibraryCtx, payload: &Value, cancel: &CancellationToken) -> Result<bool, JobError>` **changes**: `Ok(true)` when the job added, updated, re-pointed, restored or marked missing any track. An unparsable payload is `Permanent`.
  - `kima_library::jobs::OnChange = Arc<dyn Fn(PgPool) -> BoxFuture<'static, ()> + Send + Sync>` and `jobs::handler_with(ctx: LibraryCtx, on_change: OnChange) -> Handler`, calling `on_change` after an `Ok(true)`. `jobs::handler(ctx)` = `handler_with(ctx, <no-op>)`. Task 18 passes the coordinator's kick.
  - `kima_server::jobs::LIBRARY_LEASE` is deleted; `register_library` uses the default lease.

- [ ] **Step 1: Failing tests** (spec §24.3 heartbeat cases):
  - `job_longer_than_three_leases_completes`: lease 1 s, heartbeat 250 ms, a handler that sleeps 3.5 s; a second worker on the same queue polls every 100 ms meanwhile. The job ends `done` with `attempts = 1`, and the handler ran once.
  - `lost_claim_cancels_token`: a handler awaiting `ctx.cancel.cancelled()`; the test sets the row to `state = 'queued'` by SQL. The handler returns within 2 heartbeats, and the row is not marked `done`.
  - `permanent_error_fails_at_once`: `max_attempts` 5; the handler returns `JobError::Permanent("nope")`; the row is `failed` with `attempts = 1` and `last_error = 'nope'`.
  - `string_error_still_retries` (through `worker::handler`).
  - `shutdown_cancels_job_token`.
  - `enqueue_at_waits_until_run_at`: `run_at = now + 2 s`; `claim` returns nothing at once and the job after 2.5 s.
  - `reschedule_queued_moves_to_now`, `reschedule_queued_without_job_is_false`.
  - `enqueue_at_unless_queued_with_key_dedupes_queued`.
  - `reconcile_cancellable_stops_between_folders` (scan.rs): a pre-cancelled token gives `ScanError::Cancelled` and nothing is marked missing.
  - `library_queue_uses_default_lease` (library_jobs.rs) replaces the two `LIBRARY_LEASE` assertions.
- [ ] **Step 2: Run** `cargo test -p kima-jobs -p kima-library -p kima-server`. Expected: compile failure.
- [ ] **Step 3: Implement.** `run_one` builds a child token of the shutdown token per job and runs a renewal loop (`tokio::time::interval(heartbeat)`) next to the handler future; `renew` returning `false` cancels the token and logs once at `warn`. `Ok` → `complete`, `Retry` → `fail`, `Permanent` → `fail_permanent`, panic → `fail`.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(jobs): lease heartbeat, cancel token, delayed enqueue, permanent errors`.

### Task 4: Jobs: reconnect sweep (M1 minor 3) and watcher test ratio (minor 4)

**Files:**
- Modify: `crates/kima-jobs/src/worker.rs`, `crates/kima-jobs/tests/worker.rs`, `crates/kima-library/tests/watch.rs`
- Test: the same

**Divergences:** none.

**Interfaces:**
- Produces: `pub type ListenConnect = Arc<dyn Fn(PgPool) -> BoxFuture<'static, sqlx::Result<PgListener>> + Send + Sync>` and `WorkerConfig.listen_connect: ListenConnect` (default: `PgListener::connect_with(&pool)` then `listen(NOTIFY_CHANNEL)`).

- [ ] **Step 1: Failing tests** (spec §11.4):
  - `reconnect_failure_uses_retry_arm`: `listen_connect` counts its calls and fails exactly on the 2nd; `poll_interval` 30 s, `listen_retry` 200 ms. Terminate the LISTEN backend; assert a 3rd call happens within 1 s; then enqueue, and the job runs within 1 s.
  - `jobs_enqueued_during_reconnect_run_promptly`: `poll_interval` 1 h. Terminate the LISTEN backend, enqueue at once, and the job runs within 2 s.
  - `listen_is_retried_after_failure` is deleted (it passed for the wrong reason).
- [ ] **Step 2: Run** `cargo test -p kima-jobs --test worker`. Expected: fail.
- [ ] **Step 3: Implement.** `notified` uses `PgListener::try_recv()`. `Ok(Some(_))` wakes the loop; `Ok(None)` (sqlx saw the connection drop) and `Err` both drop the listener and reconnect through `listen_connect` at once, falling back to the retry arm on failure. Every reconnect, successful or not, is followed by an immediate claim sweep, since the loop claims on its next pass.
- [ ] **Step 4: Minor 4.** In `file_written_slowly_indexed_once_complete`, the settle passed to `start` becomes 2 s against the 300 ms chunk gap; the post-write wait becomes 3 s.
- [ ] **Step 5: Run** `cargo test -p kima-jobs -p kima-library --test watch`. Expected: pass.
- [ ] **Step 6: Commit** with message `fix(jobs): reconnect LISTEN through the worker and sweep after it; steadier watcher test`.

### Task 5: Settings store, write-only secrets and the identification switch

**Files:**
- Create: `crates/kima-db/migrations/0015_settings_kv.sql` (spec §6.1 verbatim), `crates/kima-db/src/settings_kv.rs`, `crates/kima-core/src/identify.rs`, `crates/kima-db/tests/settings_kv.rs`
- Modify: `crates/kima-db/Cargo.toml` (adds `kima-core`, `tracing`), `crates/kima-db/src/lib.rs`, `crates/kima-core/src/{lib.rs, config.rs}`, `crates/kima-api/src/state.rs`, `crates/kima-api/src/routes/system_settings.rs`, `crates/kima-server/src/main.rs`, `crates/kima-server/tests/app.rs` (config helper)
- Test: `crates/kima-db/tests/settings_kv.rs`, `crates/kima-api/tests/settings.rs`, unit tests in `config.rs` and `identify.rs`

**Divergences:** D-110, every clause except `test-fanart` (Task 20 completes it); D-111 (in full).

**Interfaces:**
- Consumes: `kima_core::cipher::Cipher` (M2).
- Produces:
  - `kima_db::settings_kv::Key { AcoustidApiKey, FanartApiKey, FanartEnabled, AutoEnrichMetadata, EnrichmentConcurrency, AutotagAutoApply }` with `name(self) -> &'static str` (the camelCase key of spec §15.1) and `is_secret(self) -> bool`.
  - `AutoApply { Off, Db, Files }` (serde lowercase).
  - `KvSettings { acoustid_api_key: Option<String>, fanart_api_key: Option<String>, fanart_enabled: bool, auto_enrich_metadata: bool, enrichment_concurrency: u8, autotag_auto_apply: AutoApply }`; `Default` = `None, None, false, true, 1, Off`.
  - `load(pool, cipher: &Cipher) -> sqlx::Result<KvSettings>`. A secret that fails to decrypt reads as `None`, logged at most once an hour per key, naming the key only.
  - `set_plain(pool, key: Key, value: Option<&serde_json::Value>) -> sqlx::Result<()>` (`None` deletes the row).
  - `set_secret(pool, cipher: &Cipher, key: Key, value: Option<&str>) -> sqlx::Result<()>`; AAD = `b"settings_kv:"` followed by the key name.
  - `SettingsCache` (Clone): `new(pool: PgPool, cipher: Arc<Cipher>) -> Self`, `with_ttl(self, Duration) -> Self` (default 60 s), `get(&self) -> sqlx::Result<Arc<KvSettings>>`, `replace(&self, s: KvSettings)`.
  - `kima_core::identify::IdentifySwitch` (Clone, shared): `new(env_off: bool, setting: bool) -> Self`, `enabled(&self) -> bool` (= `setting && !env_off`), `set_setting(&self, on: bool)`, `locked(&self) -> bool` (= `env_off`).
  - `Config.identify_env_off: bool` from `KIMA_IDENTIFY` (`on`/`off`, case-insensitive, default `on`; anything else is `ConfigError::Invalid` naming the variable).
  - `AppState` gains `pub settings: SettingsCache` and `pub identify: IdentifySwitch`, with `with_settings` and `with_identify`. `with_cipher` rebuilds `settings` with the new cipher.
  - Every successful POST reloads `settings_kv::load` into `state.settings.replace(…)`, so this process sees the change at once.
  - `GET /api/system-settings` (admin) answers M1's keys plus `fanartEnabled`, `fanartApiKey: ""`, `hasFanartApiKey`, `acoustidApiKey: ""`, `hasAcoustidApiKey`, `autoEnrichMetadata` (the effective value), `autoEnrichMetadataLocked`, `enrichmentConcurrency`, `autotagAutoApply`.
  - `POST /api/system-settings` (admin) accepts any subset of those, as spec §15.2. For a secret: absent or `""` leaves it unchanged, `null` clears it, and any other string is trimmed and stored (more than 200 characters → 400). Other fields are type-checked; unknown fields are ignored. Errors are 400 `{"error":"Invalid settings","details":"<detail>"}`, where the detail is `<field> is too long`, `<field> must be a boolean`, `enrichmentConcurrency must be a whole number from 1 to 5`, `autotagAutoApply must be one of off, db, files` or `<field> must be a string or null`. The answer stays M1's body. A change to `autoEnrichMetadata` calls `identify.set_setting`.
  - Main builds `IdentifySwitch::new(config.identify_env_off, kv.auto_enrich_metadata)` from `settings_kv::load` at startup.

- [ ] **Step 1: Failing DB tests:**
  - `secret_encrypted_at_rest_with_key_aad`: after `set_secret(AcoustidApiKey, "k1")`, `secret_enc` decrypts with AAD `settings_kv:acoustidApiKey` and fails with `settings_kv:fanartApiKey`; `value` is NULL.
  - `absent_rows_are_defaults`
  - `key_check_rejects_bad_names` (raw insert of `bad-key`)
  - `undecryptable_secret_reads_as_unset`
  - `cache_serves_until_ttl_and_replace_is_immediate` (`with_ttl(100 ms)`)
- [ ] **Step 2: Failing API tests** (settings.rs; the `systemSettings.acoustid.test.ts` port, inverted):
  - `acoustid_key_encrypted_at_rest_never_returned`: POST `{"acoustidApiKey":"abc"}`; GET answers `acoustidApiKey: ""` and `hasAcoustidApiKey: true`; the row's bytes do not contain `abc`.
  - `post_empty_string_keeps_secret`, `post_null_clears_secret`, `post_trims_secret`
  - `post_too_long_400`: 201 characters gives exactly `{"error":"Invalid settings","details":"fanartApiKey is too long"}`.
  - `post_wrong_types_400`: one case per detail string above.
  - `post_unknown_fields_ignored`, `post_answers_m1_body`, `non_admin_403`
  - `identify_env_off_locks`: with `IdentifySwitch::new(true, true)`, GET gives `autoEnrichMetadata: false, autoEnrichMetadataLocked: true`; a POST of `true` is stored and the switch stays disabled.
  - `toggle_flips_switch`
  - Config: `identify_default_on_and_off`, `identify_invalid_fails`.
- [ ] **Step 3: Run** `cargo test -p kima-db --test settings_kv && cargo test -p kima-api --test settings`. Expected: fail.
- [ ] **Step 4: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 5: Run** `cargo test --workspace`. Expected: pass, with the M1 settings tests unchanged.
- [ ] **Step 6: Commit** with message `feat(settings): encrypted key-value settings, write-only secrets, identification switch`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 6: `kima-net`: client, `Outcome`, per-source limits, pauses, network guard, replay server

**Files:**
- Create: `crates/kima-net/Cargo.toml` (feature `testing` pulls in `axum`; `reqwest = { workspace = true, features = ["rustls-tls", "json", "gzip"] }`), `crates/kima-net/src/{lib.rs, source.rs, outcome.rs, req.rs, client.rs, gate.rs, guard.rs, testing.rs}`, `crates/kima-db/migrations/0016_http_cache.sql` (spec §6.2 verbatim), `crates/kima-db/src/http_cache.rs` (pause rows), `crates/kima-net/tests/{client.rs, gate.rs, replay.rs}`, `crates/kima-net/tests/fixtures/lidarr-pipeline.json` (converted from upstream, see Step 4)
- Modify: root `Cargo.toml` (workspace deps `kima-net`, `percent-encoding = "2"`), `crates/kima-core/src/config.rs` (`contact`, `musicbrainz_base_url`), `crates/kima-api/Cargo.toml`, `crates/kima-api/src/state.rs`, `crates/kima-api/src/routes/system_settings.rs`, `crates/kima-server/src/main.rs`, `.github/workflows/ci.yml` (`KIMA_NET_DENY_REAL: "1"` in the `rust` job env)
- Test: the files above, plus config unit tests

**Divergences:** D-109: the client, per-source limits, pauses persisted in `source_pauses`, the UA, and transient errors retried by the ledger only (Task 7 completes the cache clauses).

**Interfaces:**
- Consumes: `IdentifySwitch` (Task 5).
- Produces:
  - `kima_net::Source { MusicBrainz, AcoustId, Wikidata, Fanart, Deezer, ListenBrainz, Lrclib, CoverArtArchive, Images }`, `as_str()` = `musicbrainz`, `acoustid`, `wikidata`, `fanart`, `deezer`, `listenbrainz`, `lrclib`, `coverartarchive`, `images`; `limits(self) -> Limits`.
  - `Limits { period: Duration, burst: u32, concurrency: u32, timeout: Duration, rate_limit_retries: u32 }`, with the spec §7.3 table as values, written `period/burst, concurrency, timeout, retries`: musicbrainz `1100 ms/1, 1, 10 s, 3`; acoustid `1 s/3, 1, 10 s, 1`; wikidata `1 s/5, 3, 10 s, 2`; fanart `1 s/2, 1, 10 s, 1`; deezer `5 s/25, 5, 10 s, 1`; listenbrainz `1100 ms/1, 1, 10 s, 1`; lrclib `3 s/1, 1, 5 s, 1`; coverartarchive `1 s/2, 2, 15 s, 1`; images `1 s/4, 2, 30 s, 0`.
  - `kima_net::Host { MusicBrainz, AcoustId, WikidataSparql, Wikidata, Wikipedia, Fanart, Deezer, ListenBrainz, Lrclib, CoverArtArchive }`, with `source(self) -> Source`.
  - `Bases { musicbrainz, acoustid, wikidata_sparql, wikidata, wikipedia, fanart, deezer, listenbrainz, lrclib, coverartarchive: String }`. `Bases::production(musicbrainz: &str)` uses `https://api.acoustid.org/v2`, `https://query.wikidata.org`, `https://www.wikidata.org`, `https://en.wikipedia.org`, `https://webservice.fanart.tv/v3`, `https://api.deezer.com`, `https://labs.api.listenbrainz.org`, `https://lrclib.net/api` and `https://coverartarchive.org`. `Bases::all(url: &str)` points every host at one URL (tests).
  - `NetConfig { user_agent: String, bases: Bases, deny_real: bool, allow_private_images: bool }`, `NetConfig::production(version: &str, contact: &str, musicbrainz_base: &str)` (`deny_real` from `KIMA_NET_DENY_REAL=1`), `NetConfig::for_tests(bases: Bases)` (`deny_real` and `allow_private_images` true). `user_agent(version, contact) -> String` gives `kima-rs/{version} ( {contact} )`.
  - `Outcome<T> { Answered(T), Absent(Evidence), RateLimited { until: OffsetDateTime }, Transient(Failure), Permanent(Failure) }` (spec §7.2), `Failure { pub code: String, pub message: String }`, `Evidence(pub serde_json::Value)`, `Outcome::map`.
  - `Req::get(host: Host, path: &str) -> Req` and `Req::post_form(host: Host, path: &str, body: String) -> Req`; builders `query(k: &str, v: impl Into<String>)`, `absent_on_404()`, `absent_when(fn(&serde_json::Value) -> bool)`, `budget(Duration)` (a deadline that starts once a limiter cell is held; blown → `Transient("TIMEOUT")`), `timeout(Duration)` (overrides the source's), `max_wait(Duration)` (longest wait for a cell; exceeded → `Transient("BUSY")`), `no_cache()`.
  - `Net` (Send + Sync): `new(cfg: NetConfig, pool: PgPool, switch: IdentifySwitch) -> Net`, `disabled() -> Net` (every call `Transient(DISABLED)`, no pool use), `load_pauses(&self) -> sqlx::Result<()>` (startup), `get_json<T: DeserializeOwned>(&self, req: Req) -> Outcome<T>`, `post_form_json<T: DeserializeOwned>(&self, req: Req) -> Outcome<T>`, `set_concurrency(&self, multiplier: u8)`, `paused_until(&self, source: Source) -> Option<OffsetDateTime>`.
  - `kima_net::gate::Gate`: a token bucket (GCRA) on `tokio::time::Instant` plus a resizable concurrency cap. It replaces the `governor` crate that spec §7.3 names, because governor's clock ignores tokio's paused time and spec §24.3 asks for paused-clock limiter tests; the behaviour is the same. `acquire(&self, max_wait: Option<Duration>) -> Result<GatePermit, Busy>`.
  - `kima_net::retry_delay(attempt: u32, retry_after: Option<&str>, jitter: Duration) -> Duration`: a positive whole number of seconds in `Retry-After` is honoured (capped 60 s); else `2 s × 2^attempt + jitter`, capped 60 s.
  - `kima_db::http_cache::{record_pause(pool, source: &str, until: OffsetDateTime, reason: &str) -> sqlx::Result<i32>` (upsert, returns `consecutive`), `clear_pause(pool, source: &str) -> sqlx::Result<()>`, `pauses(pool) -> sqlx::Result<Vec<(String, OffsetDateTime, i32)>>}`.
  - `kima_net::testing` (feature `testing`): `Entry { method: String, url: String, status: u16, headers: BTreeMap<String, String>, body: serde_json::Value, body_base64: Option<String> }`, `Recording(Vec<Entry>)` with `load(path) -> Recording` that also accepts upstream's `{source, recordedAt, replies}` shape (each reply becomes a GET entry), and `Replay::start(rec: Recording) -> Replay` with `url(&self) -> String`, `asked(&self) -> Vec<String>`, `user_agents(&self) -> Vec<String>`. Requests are matched on method, path, the sorted percent-decoded query pairs and, for POST, the SHA-256 of the body. Anything unrecorded answers 599 naming the request.
  - `Config` gains `contact: String` (`KIMA_CONTACT`) and `musicbrainz_base_url: String` (`MUSICBRAINZ_BASE_URL`, trailing `/` stripped, must start with `http://` or `https://`).
  - `AppState` gains `pub net: Arc<Net>` (default `Net::disabled()`) and `with_net`. Main builds one `Net`, calls `load_pauses` and `set_concurrency(kv.enrichment_concurrency)`. A POST that changes `enrichmentConcurrency` calls `state.net.set_concurrency`.

- [ ] **Step 1: Failing unit tests:**
  - `classify_statuses` (table, through `Replay`): 200 gives `Answered`; 404 with `absent_on_404` gives `Absent`; 404 without it gives `Permanent("HTTP_404")`; 400 gives `Permanent("HTTP_400")`; 408 gives `Transient("HTTP_408")`; 429 gives `RateLimited`; 503 from `Host::MusicBrainz` gives `RateLimited`, 503 from `Host::Deezer` `Transient("HTTP_503")`; 500 with body `rate limit exceeded` gives `RateLimited`; unparsable JSON gives `Transient("BAD_RESPONSE")`; a body over 4 MiB gives `Transient("BODY_TOO_LARGE")`.
  - `gate_spaces_musicbrainz_1100ms` (paused clock: 3 acquires take ≥ 2.2 s), `gate_deezer_bursts_25`, `gate_max_wait_busy`.
  - `concurrency_multiplier_capped_by_burst`: wikidata × 2 allows 5 in flight, not 6; musicbrainz × 5 stays 1.
  - `rate_limiter_backoff` (port `rateLimiterBackoff.test.ts`): `Retry-After: 0` falls back to the exponential delay; `Retry-After: 20` waits 20 s.
  - `user_agent_format`; config tests `contact_default`, `musicbrainz_base_trailing_slash_stripped`.
- [ ] **Step 2: Failing integration tests** (client.rs, `#[sqlx::test]`):
  - `retries_exhausted_pause_persisted`: a replay that always answers 429 for MusicBrainz. One call gives `RateLimited`, after 1 + 3 asks; a `source_pauses` row exists; a second call returns at once with no new ask; a new `Net` on the same pool, after `load_pauses`, also refuses without asking.
  - `pause_doubles_after_five_consecutive_capped_60s`: the sixth consecutive pause lasts 60 s, the fifth 30 s.
  - `answer_clears_pause_row`
  - `switch_off_never_touches_network`: a switch off gives `Transient("DISABLED")` for every host, and `asked()` stays empty.
  - `deny_real_panics_on_real_host` (`#[should_panic(expected = "kima-net refused a real host")]`).
  - `user_agent_sent_on_every_request`
  - `redirects_capped_at_three`; `https_to_http_redirect_refused`
  - `replay_unknown_request_599`; `replay_serves_upstream_recording` (the converted `lidarr-pipeline.json`: one release-group lookup answers its recorded body).
- [ ] **Step 3: Run** `cargo test -p kima-net --features testing`. Expected: fail.
- [ ] **Step 4: Implement.**
  - `get_json` checks the switch, then the in-memory pause mirror, then takes the semaphore, then a gate cell, then sends. Rate-limit outcomes are retried inside the client up to `rate_limit_retries` using `retry_delay`, then `record_pause` with `min(30 s × 2^max(0, consecutive − 5), 60 s)`. Transient errors are never retried here.
  - Redirects are followed by hand (reqwest `redirect::Policy::none()`), at most 3, same scheme or http→https only.
  - Convert upstream `__tests__/fixtures/musicbrainz/lidarr-pipeline.json` by loading it through `Recording::load` in the test; commit a copy under `tests/fixtures/`.
- [ ] **Step 5: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 6: Commit** with message `feat(net): outbound HTTP with per-source limits, persisted pauses and a replay server`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 7: `kima-net`: response cache, image downloads, fixture recorder

**Files:**
- Create: `crates/kima-net/src/{cache.rs, images.rs, ssrf.rs}`, `crates/kima-net/tests/{cache.rs, images.rs}`, `tools/record-fixtures/Cargo.toml`, `tools/record-fixtures/src/main.rs`
- Modify: root `Cargo.toml` (`members = ["crates/*", "tools/record-fixtures"]`), `crates/kima-net/src/{client.rs, lib.rs}`, `crates/kima-db/src/http_cache.rs`
- Test: the files above

**Divergences:** D-109 completed (Postgres `http_cache`, answers 30 d and absences 7 d with upstream's shorter TTLs, AcoustID answers cached).

**Interfaces:**
- Consumes: `Net`, `Req`, `Outcome` (Task 6).
- Produces:
  - `cache::canonical(req: &Req, base: &str) -> String`: method, then the URL with query pairs sorted, then for POST a newline and the hex SHA-256 of the body. `api_key` and `client` are removed from the query and from a form body first. `cache::key(source: Source, canonical: &str) -> String` gives `<source>:` + hex SHA-256.
  - `Source::ttl(self) -> Option<(Duration, Duration)>` (answered, absent), from spec §7.4: musicbrainz, acoustid, wikidata and coverartarchive 30 d / 7 d; fanart 7 d / 7 d; deezer 24 h / 24 h; listenbrainz 7 d / 1 d; lrclib and images `None`.
  - `kima_db::http_cache::{CacheOutcome { Answered, Absent }, get(pool, key: &str) -> sqlx::Result<Option<(CacheOutcome, serde_json::Value)>>` (unexpired only), `put(pool, key, source: &str, request: &str, outcome: CacheOutcome, body: &serde_json::Value, ttl: Duration) -> sqlx::Result<()>` (`ON CONFLICT (key) DO UPDATE`), `prune_expired(pool) -> sqlx::Result<u64>}`.
  - `Net::get_bytes(&self, url: &str) -> Outcome<Vec<u8>>` (source `Images`): http/https only; every address the host resolves to must pass `ssrf::is_public` unless `allow_private_images`; the connection goes to the checked address; each redirect is re-checked; `Content-Type: image/*`; 1000 bytes ≤ body ≤ 15 MiB. Refusals are `Permanent` with codes `NOT_PUBLIC`, `BAD_SCHEME`, `NOT_AN_IMAGE`, `TOO_SMALL`, `TOO_LARGE`. A 404 is `Absent`.
  - `ssrf::is_public(ip: IpAddr) -> bool`: false for loopback, private, link-local, CGNAT `100.64.0.0/10`, multicast, broadcast, unspecified, IPv6 ULA `fc00::/7`, IPv6 link-local, and IPv4-mapped forms of those.
  - `NetConfig.record: Option<Arc<Mutex<Vec<Entry>>>>`: when set, each raw answer is appended as an `Entry`.
  - `tools/record-fixtures` (binary `record-fixtures`): `record-fixtures <requests.json> <out.json>`, where `requests.json` is `[{"host":"musicbrainz","method":"GET","path":"/isrc/USRC17607839","query":[["inc","artist-credits"],["fmt","json"]],"body":null}]`. It writes a `Recording` with secrets replaced by `REDACTED`, and refuses to run when `KIMA_NET_DENY_REAL` is set.

- [ ] **Step 1: Failing tests:**
  - `cache_hit_costs_no_cell_and_no_request`: the second identical call is answered from `http_cache`; `asked()` grows by one only, and no gate cell is taken (paused clock: no 1.1 s wait).
  - `absent_cached_with_absent_ttl`: `expires_at − fetched_at` is 7 days for a MusicBrainz 404 with `absent_on_404`.
  - `absent_when_marks_empty_search_absent`
  - `failures_never_cached`: 500 then 200 asks twice.
  - `secrets_stripped_from_key_and_request`: two fanart calls with different `api_key` share one key, and the `request` column lacks both keys.
  - `post_body_in_key_without_client`: two AcoustID bodies differing only in `client` share a key; differing in `fingerprint` do not.
  - `no_cache_skips_cache`; `deezer_ttl_24h`; `listenbrainz_empty_list_absent_one_day`
  - `prune_deletes_expired_only`
  - `is_public_table`: `127.0.0.1`, `10.1.2.3`, `172.16.0.1`, `192.168.1.1`, `169.254.1.1`, `100.64.0.1`, `224.0.0.1`, `::1`, `fc00::1`, `fe80::1`, `::ffff:127.0.0.1` are false; `1.1.1.1` and `2606:4700::1` are true.
  - `get_bytes_refuses_loopback_without_flag`, `non_image_content_type_refused`, `too_small_refused`, `redirect_to_private_refused`, `ftp_scheme_refused`
  - `recorder_refuses_under_deny_real` (unit test on the binary's `check_allowed(env: Option<&str>) -> Result<(), String>`).
- [ ] **Step 2: Run** `cargo test -p kima-net --features testing`. Expected: fail.
- [ ] **Step 3: Implement.** Order inside `get_json`: the switch first (off answers `DISABLED` without reading the cache, so behaviour never depends on what is cached), then the cache, then pauses, then the semaphore and the gate.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(net): Postgres response cache, safe image downloads, fixture recorder`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 8: Migrations 0017–0020

**Files:**
- Create: `crates/kima-db/migrations/0017_identity.sql`, `0018_track_stages.sql`, `0019_genres.sql`, `0020_lyrics.sql` (spec §6.3–§6.6 verbatim), `crates/kima-db/tests/identity_schema.rs`
- Test: `crates/kima-db/tests/identity_schema.rs`

**Divergences:** none (the rows land with the behaviour, Tasks 10 onward).

**Interfaces:**
- Produces the tables and columns of spec §6.3–§6.6. Additions the spec leaves implicit:
  - 0017 ends with `UPDATE artists SET mbid = '89ad4ac3-39f7-470e-963a-56509c546377', enrichment_status = 'completed' WHERE name_key = 'various artists';` (`names::key(VARIOUS_ARTISTS)`; `mbid_source` stays NULL).
  - 0019 writes out `album_genres_effective` and `artist_genres_effective` in the same shape as `track_genres_effective` (`album_id|artist_id, genre_id, rank`).
  - `albums.cover_path` is dropped; no Rust code reads it (checked at HEAD).

- [ ] **Step 1: Failing tests:**
  - `va_artist_gets_mbid_and_completed`: insert the VA artist, re-run the 0017 `UPDATE` statement through `sqlx::raw_sql`, check the row.
  - `source_checks_reject_unknown_values`: `artists.mbid_source = 'x'`, `tracks.recording_source = 'x'`, `track_stages.status = 'x'`, `track_lyrics.source = 'x'` each fail.
  - `isrc_check_rejects_lowercase_and_short`
  - `enrichment_state_has_one_row` (a second insert fails; the seeded row is `idle`).
  - `effective_genres_prefer_musicbrainz`: a track with `file` and `musicbrainz` rows shows only the latter in `track_genres_effective`; one with only `file` rows shows those; the same for albums and artists.
  - `deleting_track_cascades`: `track_stages`, `track_genres`, `track_lyrics`, `track_acoustid` rows go with the track.
- [ ] **Step 2: Run** `cargo test -p kima-db --test identity_schema`. Expected: fail.
- [ ] **Step 3: Implement** the four files.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass, with every M1/M2 test unchanged.
- [ ] **Step 5: Commit** with message `feat(db): identity, stage, genre and lyrics schema (0017–0020)`.

### Task 9: Tag reader: MusicBrainz ids, ISRC, totals, genres, lyrics

**Files:**
- Modify: `crates/kima-library/src/tags.rs`, `crates/kima-test-support/{Cargo.toml, src/lib.rs}` (adds `lofty`, `uuid`; `Tags` and `make_audio` move here from `kima-library/tests/common`, which re-exports them, so `kima-ident`'s tests can make tagged audio too)
- Test: `crates/kima-library/tests/tags.rs`, unit tests in `tags.rs`

**Divergences:** D-107: the reading clauses ("also reads release, release-track and album-artist MBIDs, disc/track totals and genres").

**Interfaces:**
- Consumes: `tags::read` and `RawTags` (M1); `kima_test_support::require_ffmpeg` (Task 2), the M1 `make_audio` helper.
- Produces:
  - `RawTags` gains `recording_mbid`, `release_track_mbid`, `release_mbid`, `release_group_mbid: Option<Uuid>`, `artist_mbids: Vec<Uuid>`, `album_artist_mbids: Vec<Uuid>`, `isrc: Option<String>`, `genres: Vec<String>`, `disc_total: Option<u32>`, `track_total: Option<u32>`, `lyrics: Option<EmbeddedLyrics>`, read with the lofty 0.25.4 `ItemKey`s of spec §8.1.
  - `EmbeddedLyrics { plain: Option<String>, synced: Option<String> }`.
  - `tags::parse_mbid(s: &str) -> Option<Uuid>`: the trimmed value must be the 36-character hyphenated form (any case) and not nil. A single-valued field holding anything else is dropped whole. Multi-valued artist id fields are split per value, and each value also on `;` and `/`.
  - `tags::normalize_isrc(s: &str) -> Option<String>`: the first value split on `;`/`,`, spaces and hyphens removed, upper-cased, `^[A-Z]{2}[A-Z0-9]{3}\d{7}$`.
  - `tags::sylt_to_lrc(frames: &[(u32 /*ms*/, String)]) -> String`: lines `[mm:ss.cc] text`.
  - Genres: every `Genre` value, each split on `;`, trimmed, empties and names starting with `_` dropped, deduplicated case-insensitively keeping the first spelling.
  - `kima_test_support::Picard { recording, release_track, release, release_group: Option<&str>, artists, album_artists: Vec<&str>, isrc: Option<&str>, genres: Vec<&str>, disc: Option<(u32, u32)>, track: Option<(u32, u32)>, uslt: Option<&str>, sylt: Vec<(u32, &str)>, acoustid_id: Option<&str> }` (Default) and `tag_picard(path: &Path, p: &Picard)`, which writes with lofty's concrete tag types (UFID, TXXX, SYLT, USLT for MP3; Vorbis comments; MP4 freeform atoms).

- [ ] **Step 1: Failing tests** (each ffmpeg-gated, on a fresh `make_audio` file):
  - `reads_picard_ids_mp3`, `_flac`, `_ogg`, `_opus`, `_m4a`: every `RawTags` field above equals what `tag_picard` wrote.
  - **Review Focus 2,** `mbid_junk_dropped`: a recording id of `{a8b4…}`, `urn:uuid:a8b4…`, the 32-hex form, `00000000-0000-0000-0000-000000000000`, and `id1; id2` in the single-valued field each give `None`; an upper-case hyphenated id parses.
  - `acoustid_id_never_read_as_recording`: only `acoustid_id` is set; `recording_mbid` is `None`.
  - `isrc_first_value_normalised`: `us-rc1-76-07839; GBAYE0601498` gives `USRC17607839`; `PMEDIA-01` gives `None` (port of the `normalizeIsrc` cases in `recordingMatch.test.ts`).
  - `genres_split_and_deduped`: frames `Rock; Indie` and `rock`, `Pop`, `_hidden` give `["Rock","Indie","Pop"]`.
  - `totals_from_n_of_t` (MP3 `TRCK 3/12`, `TPOS 1/2`) and `vorbis_totals` (`TRACKTOTAL`, and `TOTALTRACKS` when `TRACKTOTAL` is absent).
  - `sylt_becomes_lrc`: `(62500, "line")` gives `[01:02.50] line`.
  - `uslt_with_timestamps_is_synced`, `vorbis_plain_lyrics`, `m4a_lyr_read`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test tags`. Expected: fail.
- [ ] **Step 3: Implement.** ID3 `SYLT` comes from `Id3v2Tag` frames (lofty's generic `Tag` does not carry it), so `read` opens MP3 files' ID3v2 tag concretely for lyrics.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(library): read MusicBrainz ids, ISRC, totals, genres and lyrics from tags`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 10: Scanner stores identity tags, file genres and embedded lyrics; reopens stages

**Files:**
- Create: `crates/kima-db/src/{genres.rs, lyrics.rs, stages.rs}`
- Modify: `crates/kima-db/src/{lib.rs, library.rs}`, `crates/kima-library/src/{resolve.rs, scan.rs}`
- Test: `crates/kima-library/tests/scan.rs`, `crates/kima-db/tests/library.rs`

**Divergences:** D-107 completed (album MBIDs are the mode; file genres a separate source below MusicBrainz); D-115, the clause "a re-read file without lyrics deletes its `embedded` row"; D-117, the clauses "same tag-only album identity, plus `release_mbid` stored; album MBIDs carry a source with precedence user > autotag > tag; MBID columns not unique".

**Interfaces:**
- Consumes: `RawTags` (Task 9), the 0017–0020 schema (Task 8).
- Produces:
  - `kima_library::resolve::ArtistFrom { AlbumArtistTag, ArtistTag, Path, Unknown }` and `Resolved.album_artist_from: ArtistFrom`.
  - `kima_db::library::TagIds { recording, release_track, release, release_group: Option<Uuid>, artists: Vec<Uuid>, album_artists: Vec<Uuid> }`. `ResolvedTrack` gains `ids: TagIds`, `isrc: Option<String>`, `disc_total: Option<i32>`, `track_total: Option<i32>`, `artist_mbid_candidate: Option<Uuid>`. The scanner fills the candidate by spec §8.1's artist rule: `album_artists[0]` when the album artist came from the AlbumArtist tag and exactly one id is present; `artists[0]` when it came from the Artist tag and `artists.len() == 1 + featured.len()`.
  - `kima_db::library::upsert_track` **changes** to return `sqlx::Result<Upserted { track: Uuid, album: Uuid, artist: Uuid, content_changed: bool }>`, where `content_changed` means the stored `fingerprint` differed. It writes the tag columns and totals. A file ISRC is stored with `isrc_source = 'id3'`; a file without one clears an `id3` value and leaves a `musicbrainz` one. A new artist whose `name_key` is `various artists` is created with the VA MBID and `enrichment_status = 'completed'`.
  - `library::refresh_album_mbids(conn, album: Uuid) -> sqlx::Result<()>`: the mode of `tag_release_mbid` and of `tag_release_group_mbid` over the album's present tracks, ties to the smallest UUID; each written with source `tag` only when the stored source is NULL or `tag` (R2).
  - `library::set_artist_mbid_if_unset(conn, artist: Uuid, mbid: Uuid) -> sqlx::Result<bool>`: sets `mbid`, `mbid_source = 'tag'` only while `mbid IS NULL`. `process_folder` passes the mode of the candidates it read per artist. Spec §8.1 says "mode over the artist's tracks"; the schema keeps no per-track candidate, so the first pass that finds one sets it.
  - `kima_db::genres::{GenreSource { MusicBrainz, File }, genre_key(name: &str) -> String` (lower-case, whitespace collapsed), `replace_track_genres(conn, track: Uuid, source: GenreSource, names: &[String]) -> sqlx::Result<()>` (names starting with `_` skipped; rank = position), `replace_album_genres(conn, album, source, names)`, `replace_artist_genres(conn, artist, source, names)`, `recompute_file_genres(conn, albums: &[Uuid], artists: &[Uuid]) -> sqlx::Result<()>` (top 5 by number of present tracks carrying them, ties by name), `prune_unused(pool) -> sqlx::Result<u64>}`.
  - `kima_db::lyrics::{upsert_embedded(conn, track: Uuid, plain: Option<&str>, synced: Option<&str>) -> sqlx::Result<()>` (replaces any source), `delete_embedded(conn, track: Uuid) -> sqlx::Result<()>` (only rows whose source is `embedded`)}.
  - `kima_db::stages::{reopen_for_changed_file(conn, track: Uuid) -> sqlx::Result<()>` (every row → `pending`, `attempts = 0`, `redelivered = 0`, `next_attempt_at`/errors NULL, `evidence = {"reason":"file changed"}`; deletes `track_acoustid`), `reopen_file_missing(conn, track: Uuid) -> sqlx::Result<()>` (rows with class `FILE_MISSING` → `pending`)}.
  - `store` (scan.rs) runs, in the track's transaction: `upsert_track`, `replace_track_genres(File)`, lyrics upsert or delete, and `reopen_for_changed_file` when `content_changed`. Re-pointed tracks and `clear_missing` restores call `reopen_file_missing`. After a folder, `process_folder` calls `refresh_album_mbids`, `set_artist_mbid_if_unset` and `recompute_file_genres` for the albums and artists it touched.

- [ ] **Step 1: Failing tests** (scan.rs, ffmpeg-gated, files tagged with `tag_picard`):
  - `picard_tagged_track_stores_ids`: every spec §8.1 column on the row; `isrc_source = 'id3'`.
  - `album_mbids_are_mode_ties_smallest`: 3 tracks with release A, A, B give A; 2 with A, B give the smaller.
  - `rescan_keeps_user_and_autotag_album_mbids` (R2): set `release_group_mbid_source = 'user'` by SQL; a rescan with different tags leaves it; clearing value and source then rescanning fills it from tags.
  - `artist_mbid_from_album_artist_tag`, `artist_mbid_from_artist_tag_with_featured` (`A feat. B` with 2 artist ids gives the first), `artist_mbid_skipped_when_counts_disagree`, `artist_mbid_never_replaces_user`.
  - `file_isrc_wins_over_musicbrainz`: a row with `isrc_source = 'musicbrainz'` gets the file's ISRC and `id3`.
  - `file_genres_replaced_wholesale`; `album_and_artist_file_genres_top5_by_track_count`
  - `embedded_lyrics_upserted_then_removed`: lyrics removed from the file delete the row (D-115); an `lrclib` row is replaced when the file gains lyrics.
  - `changed_file_reopens_stages_and_drops_fingerprint`; `sibling_reread_does_not_reopen` (an album-year change re-reads a sibling with unchanged content: stages untouched); `restore_reopens_file_missing`.
  - `va_artist_created_with_mbid`
- [ ] **Step 2: Run** `cargo test -p kima-library --test scan`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass, with every M1 scan test unchanged.
- [ ] **Step 5: Commit** with message `feat(library): store identity tags, file genres and embedded lyrics; reopen stages on change`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 11: Fingerprinting: chromaprint from ffmpeg, known-answer test, `fpcalc` feature

**Files:**
- Create: `crates/kima-ident/Cargo.toml`, `crates/kima-ident/src/{lib.rs, chromaprint.rs}`, `tools/make-fingerprint-fixture.sh`, `tools/fixtures/chromaprint/tone.wav`, `tools/fixtures/chromaprint/tone.fpcalc.json`, `crates/kima-ident/tests/chromaprint.rs`
- Modify: root `Cargo.toml` (workspace deps `kima-ident`, `rusty-chromaprint = "0.3"`), `crates/kima-core/src/config.rs`, `crates/kima-api/src/state.rs`, `crates/kima-api/src/routes/system.rs`, `crates/kima-server/src/main.rs`
- Test: `crates/kima-ident/tests/chromaprint.rs`, `crates/kima-api/tests/foundation.rs` (features), config unit tests

**Divergences:** D-106: the fingerprint clauses (in-process `rusty-chromaprint` on ffmpeg's 11025 Hz mono s16le output, first 120 s; no `fpcalc` in the image; the tag fingerprint is not read; `fpcalc` true when ffmpeg is present and `ACOUSTID_DISABLED` is not set). Task 12 completes the row.

**Interfaces:**
- Produces:
  - `kima_ident::chromaprint::Fingerprint { raw: Vec<u32>, encoded: String }`.
  - `DecodeError { Missing(std::io::Error), Timeout, NoAudio, Ffmpeg { code: Option<i32>, stderr: String } }`; `message(&self) -> String` is the text the ledger classifies (`incomplete decoder output` for `NoAudio`).
  - `fingerprint_file(path: &Path, timeout: Duration) -> impl Future<Output = Result<Fingerprint, DecodeError>>`: spawns `ffmpeg -nostdin -v error -i file:<abs path> -map 0:a:0 -t 120 -ac 1 -ar 11025 -f s16le -`, keeps the first 2000 bytes of stderr, feeds stdout in 64 KiB chunks to `rusty_chromaprint::Fingerprinter` with `Configuration::preset_test2()`, kills ffmpeg on timeout, and never parses stdout after a non-zero exit.
  - `compress(raw: &[u32]) -> Vec<u8>` (chromaprint's `FingerprintCompressor` format, algorithm id 1 = TEST2) and `encode(raw: &[u32]) -> String` (base64 URL-safe, no padding).
  - `ffmpeg_available() -> impl Future<Output = bool>`: `ffmpeg -version`, 5 s timeout.
  - `Config` gains `fingerprint_workers: usize` (`KIMA_FINGERPRINT_WORKERS`, ≥ 1; 0 or non-numeric is invalid) and `acoustid_disabled: bool` (`ACOUSTID_DISABLED`, `true` case-insensitive).
  - `AppState` gains `pub fpcalc: bool` and `with_fpcalc`; `GET /api/system/features` answers it. Main sets it to `ffmpeg_available().await && !config.acoustid_disabled`.

- [ ] **Step 1: Check the crate** (spec §27 Q2). Read `rusty-chromaprint` 0.3's docs. If it exposes `FingerprintCompressor` (algorithm 2 output equal to chromaprint's) and `Configuration::preset_test2()`, `compress` wraps it. Otherwise port chromaprint's `fingerprint_compressor.cpp` into `chromaprint.rs` with its upstream test vectors as unit tests (`compress_matches_chromaprint_vectors`).
- [ ] **Step 2: Make the fixture.** `tools/make-fingerprint-fixture.sh` writes `tone.wav`: 30 s, 11025 Hz mono s16, from ffmpeg `aevalsrc` with a fixed multi-tone expression (in the script). Run `fpcalc -json tone.wav > tone.fpcalc.json` with chromaprint 1.5.x (`pacman -S chromaprint` on the dev machine), and add `"_fpcalc": "<fpcalc -version output>"` to the JSON. The script's header comment names the version too.
- [ ] **Step 3: Failing tests** (ffmpeg-gated):
  - `known_answer_matches_fpcalc`: `fingerprint_file(tone.wav)?.encoded == tone.fpcalc.json["fingerprint"]`.
  - `flac_44k_bit_error_under_5_percent`: the same signal as 44.1 kHz FLAC (made in the test); the raw fingerprints differ in fewer than 5 % of bits over the shorter length.
  - `ffmpeg_failure_reports_code_and_stderr`: a text file named `x.flac` gives `Ffmpeg { code: Some(_), stderr }` with non-empty stderr ≤ 2000 bytes.
  - `missing_file_is_missing`; `no_audio_stream_is_no_audio_or_ffmpeg` (a PNG named `.flac` with `-map 0:a:0`); `timeout_kills_ffmpeg` (1 ms gives `Timeout`, and no ffmpeg child outlives the call).
  - **Review Focus 5,** `odd_file_names_fingerprint`: files named `-01 intro.flac`, `a:b.flac`, and on Linux a name with byte `0xFF` (`OsStr::from_bytes`), each fingerprinted to the same string as a copy named `plain.flac`.
  - `features_fpcalc_reflects_state` (foundation.rs); config `fingerprint_workers_default_half_cores`, `fingerprint_workers_zero_invalid`, `acoustid_disabled_parses`.
- [ ] **Step 4: Run** `cargo test -p kima-ident --test chromaprint`. Expected: fail.
- [ ] **Step 5: Implement.**
- [ ] **Step 6: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 7: Commit** with message `feat(ident): in-process chromaprint fingerprints from ffmpeg; fpcalc feature`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 12: `kima-ident` text rules

**Files:**
- Create: `crates/kima-ident/src/{text.rs, mbid.rs}`, `crates/kima-ident/tests/text.rs`
- Modify: `crates/kima-ident/Cargo.toml` (`regex = "1"`, `unicode-normalization`), `crates/kima-ident/src/lib.rs`
- Test: `crates/kima-ident/tests/text.rs`

**Divergences:** D-106 completed (text folding is NFKD plus a 20-letter table, tested with upstream's cases).

**Interfaces:**
- Consumes: `kima_library::resolve::parse_credit` (M1; spec §8.2 calls it `names::parse_credit`).
- Produces:
  - `kima_ident::Mbid(pub Uuid)` (Copy, Eq, Hash, Serialize/Deserialize as the hyphenated lower-case string), `Mbid::parse(s: &str) -> Option<Mbid>` (same rule as `tags::parse_mbid`), `Display`.
  - `text::{fold_identity, normalize_artist_name, artist_identity_key, fold_title, normalize_for_search, escape_lucene, normalize_string}(s: &str) -> String`; `titles_agree(a: &str, b: &str) -> bool`; `version_of(title: &str) -> BTreeSet<&'static str>`; `lengths_agree(a: Option<i32>, b: Option<i32>) -> Option<bool>`; `artists_agree(local: &[String], credits: &[String]) -> bool`; `is_generic_album_title(s: &str) -> bool`; `strip_artist_suffixes(s: &str) -> String`; `names_agree(found: &str, wanted: &str) -> bool`.
  - `text::Expected { title: String, artists: Vec<String>, length_secs: Option<i32>, isrc: Option<String> }` and `text::Candidate { id: Mbid, title: String, length_secs: Option<i32>, score: Option<u32>, credit_names: Vec<String>, isrcs: Vec<String> }`; `recording_agrees(c: &Candidate, e: &Expected) -> bool`; `pick_recording<'a>(cs: &'a [Candidate], e: &Expected) -> Option<&'a Candidate>`.
  - `text::local_names(track_artist: &str, album_artist: &str) -> Vec<String>`: `parse_credit(track_artist)` primary plus featured, then the album artist, deduplicated.
  - Every function follows spec §8.2 with its upstream source line. `is_generic_album_title` ports the list at `albumIdentity.ts:19-36` verbatim; `strip_artist_suffixes` ports `artistCorrection.ts`; `normalize_string` ports `utils/normalization.ts:normalizeString`; `names_agree` ports `deezer.ts:namesAgree`.

- [ ] **Step 1: Failing tests,** ports with upstream case names:
  - `recordingMatch.test.ts`, all cases (e.g. `rejects_a_different_song_that_shares_the_isrc`, `picks_the_agreeing_candidate_with_a_known_length_over_the_top_scored_one`).
  - `titlesAgree.test.ts`, all 10 (Japanese, dakuten, Cyrillic, Korean, Devanagari, Latin accent, deluxe, fold-to-nothing, punctuation-only).
  - `albumIdentity.test.ts`: the `isGenericAlbumTitle` cases and the identity-key cases that apply to `fold_identity`.
  - `artistIdentity.test.ts` and `artistIdentityAmpersand.test.ts` for `normalize_artist_name` and `artist_identity_key` (`Dead Mau5` → `deadmau5`); their `parseCredit` cases go to `kima-library`'s `resolve` tests.
  - `fold_identity_table`: each of the 20 letters of spec §8.2.
  - `escape_lucene_every_special`
  - `mbid_parse_rules`
- [ ] **Step 2: Run** `cargo test -p kima-ident --test text`. Expected: fail.
- [ ] **Step 3: Implement.** Compile each regex once (`std::sync::LazyLock`).
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): upstream text rules for matching, with their tests`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 13: Stage ledger: claims, reports, retry classes

**Files:**
- Create: `crates/kima-ident/src/stages/mod.rs`, `crates/kima-db/tests/stages.rs`, `crates/kima-ident/tests/stages.rs`
- Modify: `crates/kima-db/src/stages.rs`
- Test: the two test files

**Divergences:** D-108: the ledger clauses (`track_stages` is the work queue; no `blocked`, `skipped` always false; retry due and terminal told apart by `next_attempt_at`; when fingerprinting is unavailable the gate counts as settled).

**Interfaces:**
- Consumes: `kima_db::stages` (Task 10), `kima_net::Failure`, `Outcome` (Task 6).
- Produces:
  - `kima_db::stages::Stage { Fingerprint, Recording }` with `as_str`; `Claim { track_id: Uuid, stage: Stage, started_at: OffsetDateTime, attempts: i16, redelivered: i16 }`.
  - `claim(pool, stage: Stage, n: i64, fingerprinting: bool) -> sqlx::Result<Vec<Claim>>`: the spec §10.3 statement; for `Recording`, the prerequisite is `NOT $fingerprinting OR` a fingerprint row that is `done` or `failed` with `next_attempt_at IS NULL`.
  - `report_done(conn: &mut PgConnection, claim: &Claim, source: &str, evidence: &serde_json::Value) -> sqlx::Result<bool>` (guarded by `status = 'working' AND started_at = $3`).
  - `FailureRecord { pending_until: Option<OffsetDateTime>, retry_at: Option<OffsetDateTime>, count_attempt: bool, code: String, class: Option<String>, message: String }` and `report_failure(pool, claim: &Claim, rec: &FailureRecord) -> sqlx::Result<bool>`. `pending_until` set → `pending`; else `failed` with `next_attempt_at = retry_at`.
  - `kima_ident::stages::{CLAIM_LEASE, REDELIVERY_CAP, RETRY_CAP, RETRY_DELAYS, STAGE_TIMEOUT, MAX_ERROR_CHARS}` (Global Constraints values).
  - `ErrorClass { Damaged, Unsupported, Multicannel, Inconclusive, Permanent, FileMissing, DependencyClosed }` with `as_str` (`DAMAGED`, …, `MULTICANNEL`); `classify(message: &str) -> ErrorClass` (spec §10.4 patterns, case-insensitive, precedence MULTICANNEL > UNSUPPORTED > DAMAGED > INCONCLUSIVE).
  - `StageFailure { RateLimited { until: OffsetDateTime }, Permanent(Failure), Transient(Failure), FileMissing(String) }`; `StageFailure::from_outcome<T>(o: Outcome<T>) -> Option<StageFailure>` (`None` for `Answered`/`Absent`).
  - `record_for(claim: &Claim, f: &StageFailure, now: OffsetDateTime) -> FailureRecord`: rate limited → pending until, no attempt, code `RATE_LIMITED`; permanent → terminal, class `PERMANENT`; file missing → terminal, `FILE_MISSING`; transient → `classify`, retry only when `INCONCLUSIVE` and `attempts + 1 < RETRY_CAP`, at `RETRY_DELAYS[attempts]`. Messages cut to 2000 characters.
  - `lost_worker(claim) -> FailureRecord` (terminal, code `LOST_WORKER`, class `INCONCLUSIVE`, attempt counted).

- [ ] **Step 1: Failing tests** (ports of `getNextWork`, `stageGating`, `unifiedEnrichment.fingerprintGate`, `enrichment-stale-reports`, `enrichment-retry-cap`, `enrichment-per-source-pause`, `enrichment-inconclusive-errors`):
  - `no_row_is_pending_newest_mtime_first`; `missing_tracks_not_claimed`
  - `recording_waits_for_settled_fingerprint`: no fingerprint row, `pending`, and `failed` with a retry due all block; `done` and terminal `failed` both settle.
  - `fingerprinting_unavailable_counts_as_settled`
  - `concurrent_claims_split`; `claim_creates_working_rows_with_lease`
  - `lapsed_claim_is_redelivered`; `redelivery_cap_closes_lost_worker`
  - `stale_report_dropped`: claim, lapse the lease by SQL, re-claim, then the first claim's `report_done` returns `false` and the row is unchanged.
  - `rate_limited_parks_without_attempt`; `permanent_is_terminal`
  - `inconclusive_retries_1m_10m_1h_then_terminal`; `damaged_terminal_at_once`
  - `classify_table` (one sample per pattern, plus `multichannel stream truncated` → `MULTICANNEL`)
  - `file_missing_terminal`; `last_error_cut_at_2000`; `done_clears_errors_sets_finished_at`
- [ ] **Step 2: Run** `cargo test -p kima-db --test stages && cargo test -p kima-ident --test stages`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): track_stages ledger with claims, stale-report guard and retry classes`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 14: Fingerprint stage and `IdentCtx`

**Files:**
- Create: `crates/kima-ident/src/{ctx.rs, stages/fingerprint.rs}`, `crates/kima-db/src/identity.rs`, `crates/kima-ident/tests/fingerprint_stage.rs`, `crates/kima-ident/tests/common/mod.rs`
- Modify: `crates/kima-db/src/lib.rs`, `crates/kima-library/src/scan.rs` (`library_path`)
- Test: `crates/kima-ident/tests/fingerprint_stage.rs`

**Divergences:** none.

**Interfaces:**
- Consumes: `fingerprint_file` (Task 11), `stages::{claim, report_done, report_failure}` and `record_for` (Task 13), `SettingsCache` (Task 5), `Net` (Task 6), `LibraryCtx` (M1).
- Produces:
  - `kima_ident::IdentCtx { pub pool: PgPool, pub net: Arc<Net>, pub cipher: Arc<Cipher>, pub events: EventBus, pub library: LibraryCtx, pub settings: SettingsCache, pub config: IdentConfig }` (Clone).
  - `IdentConfig { pub fingerprinting: bool, pub fingerprint_workers: usize, pub acoustid_disabled: bool }` (Task 16 adds `acoustid_env_key`, Task 20 `fanart_env_key`; both `Option<String>`, default `None`).
  - `kima_library::scan::library_path(root: &Path, rel: &str) -> Option<PathBuf>`: `Some` only for paths made of `Component::Normal` parts (the M1 rule).
  - `kima_db::identity::{store_fingerprint(conn, track: Uuid, fingerprint: &str, duration_secs: i32) -> sqlx::Result<()>`, `fingerprint_of(pool, track: Uuid) -> sqlx::Result<Option<(String, i32)>>`, `track_path_and_duration(pool, track: Uuid) -> sqlx::Result<Option<(String, i32)>>}`.
  - `kima_ident::stages::BatchReport { claimed: usize, done: usize, failed: usize }`.
  - `stages::fingerprint::run_batch(ctx: &IdentCtx, cancel: &CancellationToken) -> sqlx::Result<BatchReport>`: claims 20, runs at most `fingerprint_workers` decodes at once, closes over-cap redeliveries with `lost_worker`, reuses a stored fingerprint (`done`, evidence `{"reason":"fingerprint already stored","characters":<len>}`), closes `duration_secs ≤ 0` as `PERMANENT`/`NO_DURATION`, maps a path outside the root or `ENOENT` to `FileMissing`, and stores the result plus `report_done` (source `ffmpeg`) in one transaction.
  - Test helper `tests/common::ident_ctx(pool: &PgPool, root: &Path, bases: Bases) -> IdentCtx` (a `Net` from `NetConfig::for_tests`, switch on).

- [ ] **Step 1: Failing tests** (ffmpeg-gated where they decode):
  - `stored_fingerprint_reused_without_decoding` (the file is deleted first; still `done`).
  - `new_track_fingerprinted_and_stored`: `duration_secs` equals `tracks.duration_secs`; source `ffmpeg`.
  - `zero_duration_is_no_duration`; `missing_file_is_file_missing`; `escaping_path_is_file_missing` (a row with path `../x.flac`).
  - `decode_failure_classified`: a truncated FLAC closes with the class `classify` gives its stderr.
  - `stale_claim_report_dropped`
- [ ] **Step 2: Run** `cargo test -p kima-ident --test fingerprint_stage`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): fingerprint stage`.

### Task 15: MusicBrainz client

**Files:**
- Create: `crates/kima-ident/src/musicbrainz.rs`, `crates/kima-ident/tests/musicbrainz.rs`, `crates/kima-ident/tests/fixtures/musicbrainz/*.json`
- Test: `crates/kima-ident/tests/musicbrainz.rs`

**Divergences:** none.

**Interfaces:**
- Consumes: `Net`, `Req`, `Host::MusicBrainz` (Tasks 6–7); `text::{normalize_for_search, escape_lucene, pick_recording, Candidate, Expected}` and `Mbid` (Task 12).
- Produces (`kima_ident::musicbrainz`; every request adds `fmt=json`; entity lookups use `absent_on_404`):
  - Types (serde, MusicBrainz's kebab-case field names): `Credit { name, joinphrase, artist: CreditArtist { id: Mbid, name, sort_name } }`; `Recording { id: Mbid, title, length: Option<u32> /*ms*/, score: Option<u32>, artist_credit: Vec<Credit>, isrcs: Vec<String>, genres: Vec<Genre>, tags: Vec<Genre> }`; `Genre { name, count: u32 }`; `ReleaseSummary { id: Mbid, status: Option<String>, date: Option<String> }`; `ReleaseGroup { id: Mbid, title, primary_type: Option<String>, first_release_date: Option<String>, releases: Vec<ReleaseSummary>, genres: Vec<Genre> }`; `Release { id: Mbid, title, date: Option<String>, label_info: Vec<LabelInfo>, media: Vec<Medium>, genres: Vec<Genre>, artist_credit: Vec<Credit> }`; `Medium { position: u32, track_count: u32, tracks: Vec<Track> }`; `Track { id: Mbid, position: u32, title, length: Option<u32>, recording: Recording }`; `Artist { id: Mbid, name, sort_name, aliases: Vec<Alias>, genres: Vec<Genre> }`; `Catalogue { release_group: ReleaseGroup, release: Release }`.
  - `Recording::candidate(&self) -> text::Candidate` (credit names via `credit_names`).
  - `choose_release(rs: &[ReleaseSummary]) -> Option<&ReleaseSummary>`: official first, then the earliest `date` padded with `-99-99` (none = `9999`), then id ascending.
  - `catalogue(net: &Net, rg: Mbid, tagged_release: Option<Mbid>) -> Outcome<Catalogue>`: `GET /release-group/{rg}?inc=releases+genres`, then `GET /release/{id}?inc=recordings+isrcs+labels+genres+artist-credits` for `tagged_release` or `choose_release`.
  - `isrc_recordings(net, isrc: &str) -> Outcome<Vec<Recording>>`: `GET /isrc/{code}?inc=artist-credits`; a non-ISRC asks nothing and is `Absent`; 400, 404 and zero recordings are `Absent`.
  - `search_recordings(net, title: &str, artist: &str) -> Outcome<Vec<Recording>>`: query 1 `recording:"<t>" AND artist:"<a>"`, query 2 `<t> AND artist:<a>` (both escaped), `GET /recording?query=…&limit=50`; a 400 tries the next; an empty normalised title asks nothing (`Absent`, reason `nothing to search by`); zero hits across both is `Absent`.
  - `identify_recording(net, e: &Expected) -> Outcome<Recording>`: search then `pick_recording`.
  - `recording_isrcs_and_genres(net, id: Mbid) -> Outcome<Recording>`: `GET /recording/{id}?inc=isrcs+genres+tags`.
  - `artist(net, id: Mbid, inc: &str) -> Outcome<Artist>` (`inc` = `aliases` or `genres`); `search_artists(net, query: &str) -> Outcome<Vec<Artist>>` (`GET /artist?query=<q>&limit=10`); `artist_release_groups(net, artist: Mbid, offset: u32) -> Outcome<(Vec<ReleaseGroup>, u32 /*release-group-count*/)>` (`GET /release-group?artist=<id>&type=album|ep|single&limit=100&offset=<n>`).
  - `credit_names(credits: &[Credit]) -> Vec<String>`: each credit's `name`, `artist.name` and `artist.sort-name`, plus the joined credit line.
  - `top_genres(gs: &[Genre], n: usize) -> Vec<String>`: by `count` descending, ties by name.

- [ ] **Step 1: Fixtures.** Copy upstream `services/__tests__/fixtures/mb/{rg-discovery.json, release-discovery-early.json}` bodies into replay entries keyed by kima-rs's request shape. Record the identification cases with `tools/record-fixtures` (requests listed in `tests/fixtures/musicbrainz/requests.json`, committed); where the network is unavailable, hand-write the entries from the bodies in upstream `musicbrainz.identification.test.ts`.
- [ ] **Step 2: Failing tests** (ports of `musicbrainz.identification.test.ts` (10) and `musicbrainz.albumCatalogue.test.ts` (6), through `Replay`):
  - `never_sends_a_code_that_is_not_an_isrc`, `treats_a_code_musicbrainz_refuses_as_no_answer`, `returns_every_recording_with_its_length_and_credits`, `takes_the_candidate_that_is_the_track_not_the_top_hit`, `answers_absent_when_no_candidate_is_the_track`, `treats_a_refused_query_as_no_answer_so_the_next_is_asked`, `asks_nothing_for_a_title_all_in_brackets`, `skips_another_artists_song_or_another_versions_length`, `passes_over_a_recording_marked_live`, `pages_past_the_first_100_release_groups`, `isrcs_and_genres_come_from_one_request_then_cache`.
  - `extracts_the_earliest_release_year_label_genres_and_tracklist`, `keeps_every_name_a_track_is_credited_to`, `asks_two_requests_with_exactly_the_needed_increments` (the `asked()` list exactly), `picks_the_earliest_official_release_whatever_the_order`, `answers_absent_for_a_made_up_release_group`, `serves_a_second_call_from_cache_without_http`.
  - `tagged_release_fetched_directly` (no `choose_release`).
  - **Review Focus 3,** `search_query_escapes_lucene_and_url`: for each title/artist pair in Review Focus 3, `asked()` holds exactly the expected `query=` value (Lucene-escaped, then percent-encoded) for query 1.
- [ ] **Step 3: Run** `cargo test -p kima-ident --test musicbrainz`. Expected: fail.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run.** Expected: pass.
- [ ] **Step 6: Commit** with message `feat(ident): MusicBrainz client with upstream's queries`.

### Task 16: AcoustID lookup and key order

**Files:**
- Create: `crates/kima-ident/src/acoustid.rs`, `crates/kima-ident/tests/acoustid.rs`, `crates/kima-ident/tests/fixtures/acoustid/*.json`
- Modify: `crates/kima-core/src/config.rs` (`acoustid_client_key`), `crates/kima-ident/src/ctx.rs` (`acoustid_env_key`)
- Test: `crates/kima-ident/tests/acoustid.rs`

**Divergences:** D-124 (in full).

**Interfaces:**
- Consumes: `Net` (`Req::post_form`, `Host::AcoustId`), `KvSettings` (Task 5), `text::{titles_agree, version_of, lengths_agree}`, `Mbid`.
- Produces:
  - `pub const DEFAULT_CLIENT_KEY: &str = "";`
  - `client_key(kv: &KvSettings, env: Option<&str>) -> Option<String>`: the setting, then env, then the default; empty means none.
  - `AcoustidResult { id: String, score: f64, recordings: Option<Vec<AcoustidRecording { id: Mbid, title: Option<String>, duration: Option<f64> }>> }`.
  - `lookup(net: &Net, key: &str, fingerprint: &str, duration_secs: i32) -> Outcome<Vec<AcoustidResult>>`: `POST /lookup` with body exactly `client=<pct key>&duration=<secs>&fingerprint=<pct fp>&meta=recordings%20releasegroups` and `Content-Type: application/x-www-form-urlencoded`. An empty key is `Permanent("NO_CLIENT_KEY")`; an empty fingerprint or `duration_secs ≤ 0` is `Permanent("NO_FINGERPRINT")` with no request; `status != "ok"` is `Transient("BAD_RESPONSE")`; `results: []` is `Absent`.
  - `Picked { recording: Mbid, score: f64, acoustid_id: String }` and `pick_fingerprint_match(results: &[AcoustidResult], duration_secs: i32, title: Option<&str>) -> Result<Picked, Evidence>` (spec §8.4; `MIN_SCORE = 0.5`; evidence `{askedDuration, topScore, reason}`).
  - `identify(net, key, fingerprint, duration_secs, title) -> Outcome<Picked>`.
  - `Config.acoustid_client_key: Option<String>` (`ACOUSTID_CLIENT_KEY`); `IdentConfig.acoustid_env_key`.

- [ ] **Step 1: Fixtures.** Hand-write replay entries from the bodies in upstream `acoustidService.test.ts` (no kima-rs key exists to record with, spec §27 Q1).
- [ ] **Step 2: Failing tests** (port of `acoustidService.test.ts`, 16 cases, names from its `it(...)` text, e.g. `skips_results_below_the_score_floor_and_takes_the_next`, `sends_the_live_verified_request_shape`), with these kima-rs readings of the key cases: `sends_the_stored_settings_key`, `env_key_when_no_setting`, `empty_default_means_no_key`, `settings_read_failure_falls_back_to_env`. Plus `no_key_is_permanent_no_client_key_without_request`.
- [ ] **Step 3: Run** `cargo test -p kima-ident --test acoustid`. Expected: fail.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run.** Expected: pass.
- [ ] **Step 6: Commit** with message `feat(ident): AcoustID lookup, fingerprint pick and key order`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 17: Recording stage: the identification ladder

**Files:**
- Create: `crates/kima-ident/src/{matching.rs, stages/recording.rs}`, `crates/kima-ident/tests/{ladder.rs, recording_stage.rs}`
- Modify: `crates/kima-db/src/identity.rs`
- Test: the two test files

**Divergences:** D-117: the clauses "a tagged release is used directly for the tracklist" and "album year is never filled from MusicBrainz".

**Interfaces:**
- Consumes: `musicbrainz::*` (Task 15), `acoustid::{client_key, identify}` (Task 16), `stages::*` (Task 13), `identity::fingerprint_of` (Task 14), `genres::{replace_track_genres, replace_album_genres}` (Task 10), `text::local_names` (Task 12).
- Produces:
  - `kima_db::identity::LadderTrack { id, title, artist_name, duration_secs, isrc: Option<String>, isrc_source: Option<String>, tag_recording_mbid: Option<Uuid>, album_id, album_title, album_artist, release_mbid: Option<Uuid>, release_group_mbid: Option<Uuid>, album_label: Option<String>, album_has_mb_genres: bool }` and `ladder_track(pool, id: Uuid) -> sqlx::Result<Option<LadderTrack>>`.
  - `kima_db::identity::IsrcWrite { Keep, Set(String), Clear }` and `store_recording_answer(conn, track: Uuid, recording: Uuid, source: &str, isrc: IsrcWrite) -> sqlx::Result<()>` (an `IsrcWrite::Set` writes source `musicbrainz`); `fill_album_blank(conn, album: Uuid, label: Option<&str>) -> sqlx::Result<()>`.
  - `matching::Rung { FileTag, Tracklist, Isrc, Search, AcoustId }` (`as_str` = `file_tag`, `tracklist`, `isrc`, `search`, `acoustid`).
  - `matching::SkipReason { NoKey, NoFingerprint, FingerprintingUnavailable, Disabled }`, `as_str` = `no AcoustID key`, `no fingerprint`, `fingerprinting unavailable`, `acoustid disabled`; `code` = `no_key`, `no_fingerprint`, `fingerprinting_unavailable`, `disabled`.
  - `matching::AcoustidGate { Ask { key: String, fingerprint: String, duration_secs: i32 }, Skip(SkipReason), Paused { until: OffsetDateTime } }` and `acoustid_gate(ctx: &IdentCtx, track: Uuid) -> sqlx::Result<AcoustidGate>`.
  - `matching::Answer { recording: Mbid, rung: Rung, isrc: IsrcWrite, track_genres: Vec<String>, album_label: Option<String>, album_genres: Vec<String>, evidence: serde_json::Value }`.
  - `matching::LadderResult { Matched(Answer), NoMatch { asked: Vec<&'static str>, skipped: Option<SkipReason> }, Park { until: OffsetDateTime }, Fail(StageFailure) }`.
  - `matching::identify(ctx: &IdentCtx, t: &LadderTrack, gate: AcoustidGate) -> LadderResult`: spec §8.3 rungs in order, every request with `budget(STAGE_TIMEOUT)`; any failure stops the ladder (`Fail`), only `Absent` falls through; ISRC and genres per §8.5 (top 5 curated `genres`; `tags` into evidence only).
  - `stages::recording::run_batch(ctx: &IdentCtx, cancel: &CancellationToken) -> sqlx::Result<BatchReport>`: claims 20, at most `enrichmentConcurrency` items in flight. A match writes, in one transaction, `report_done` (source = rung) and then `store_recording_answer`, the track's MusicBrainz genres, and `fill_album_blank` plus album genres when the album has none; a `false` from `report_done` rolls back. No match is `done` with evidence `{"reason":"no match","asked":[…]}` plus `"acoustid":"skipped","acoustidReason":<code>` when skipped. `Park` reports rate-limited, without an attempt.

- [ ] **Step 1: Failing ladder tests** (ports of `acoustid-fingerprint.test.ts`, `unifiedEnrichment.acoustidOrder.test.ts`, and the applicable `acoustidIdentification.test.ts` cases; Replay for MusicBrainz and AcoustID):
  - `file_tag_rung_decides_without_identification_requests`: a tagged track whose album has no release group: the only request is `/recording/<id>?inc=isrcs+genres+tags` (for ISRC and genres), and the source is `file_tag`.
  - `file_tag_with_release_group_reads_the_catalogue_entry`
  - `tracklist_rung_by_isrc_then_pick_then_vouched`; `vouched_pass_needs_exactly_one_row`
  - `failed_catalogue_defers_without_falling_through`
  - `isrc_rung_before_search`; `search_rung`; `acoustid_rung_last`
  - `mb_isrc_replaces_only_null_or_musicbrainz`; `isrc_rung_keeps_the_file_isrc`
  - `acoustid_skipped_without_key_has_evidence`; `acoustid_skipped_when_fingerprint_terminal`; `acoustid_paused_parks_without_attempt`
  - `no_rung_answers_closes_done_no_match`
  - `acoustid_id_tag_is_never_a_recording_id`
- [ ] **Step 2: Failing stage tests:**
  - `match_written_in_one_transaction_with_report`
  - **Review Focus 4,** `track_purged_mid_claim_writes_nothing`: claim two tracks, delete the first before its report; the run gives no error, writes no row for it, and identifies the second.
  - **Review Focus 4,** `file_changed_mid_claim_is_dropped`: `reopen_for_changed_file` runs between the claim and the report; the report returns `false`, `recording_mbid` stays NULL, the row stays `pending`.
  - `album_year_never_filled`; `album_label_and_genres_only_when_blank`; `track_genres_are_top5_curated_not_tags`
  - `never_writes_album_identity_or_artist_mbid`
- [ ] **Step 3: Run** `cargo test -p kima-ident --test ladder --test recording_stage`. Expected: fail.
- [ ] **Step 4: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 5: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 6: Commit** with message `feat(ident): recording stage with upstream's identification ladder`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 18: Coordinator: the `enrich` queue, the cycle, kick, scheduler

**Files:**
- Create: `crates/kima-ident/src/coordinator/mod.rs`, `crates/kima-db/src/enrichment.rs`, `crates/kima-ident/tests/coordinator.rs`
- Modify: `crates/kima-db/src/lib.rs`, `crates/kima-ident/src/ctx.rs`, `crates/kima-server/src/{jobs.rs, main.rs}`, `crates/kima-api/src/routes/system_settings.rs` (kick when the switch turns on), `.github/workflows/ci.yml` (`KIMA_IDENTIFY=off` for the e2e job's kima)
- Test: `crates/kima-ident/tests/coordinator.rs`, `crates/kima-server/tests/library_jobs.rs`, `crates/kima-api/tests/settings.rs`

**Divergences:** D-108 completed (one coordinator job; state in Postgres; no notifications until M8).

**Interfaces:**
- Consumes: `stages::fingerprint::run_batch` (Task 14), `stages::recording::run_batch` (Task 17), `queue::{enqueue_at_unless_queued_with_key, reschedule_queued, requeue_running}`, `worker::{handler_ctx, JobCtx, JobError}`, `library::jobs::{handler_with, OnChange}` (Task 3), `Net::paused_until` (Task 6), `http_cache::prune_expired` (Task 7), `genres::prune_unused` (Task 10).
- Produces:
  - `kima_db::enrichment::{Status { Idle, Running, Paused, Stopping }, Phase { Artists, Tracks, Fingerprint }, State { status, halted: bool, current_phase: Option<Phase>, started_at, paused_at, stopped_at, last_error: Option<String>, last_activity: OffsetDateTime }, state(pool) -> sqlx::Result<State>, begin_cycle(pool) -> sqlx::Result<State>` (`running`, `started_at` when idle), `set_phase(pool, phase: Option<Phase>, activity: bool) -> sqlx::Result<()>`, `end_cycle(pool, idle: bool) -> sqlx::Result<()>` (`stopping` → `idle`)}.
  - `coordinator::{QUEUE = "enrich", CYCLE_KIND = "cycle", MAX_ATTEMPTS = 3, MIN_INTERVAL = 10 s}`.
  - `coordinator::kick(pool: &PgPool) -> sqlx::Result<()>`: `reschedule_queued(now)`, else `enqueue_at_unless_queued_with_key({"kind":"cycle"}, key "kind", now)`.
  - `coordinator::run_cycle(ctx: &IdentCtx, cancel: &CancellationToken) -> Result<CycleReport, JobError>`: spec §10.6 steps 1–7. Fingerprint batches run only when `ctx.config.fingerprinting` (spec §9.4), concurrently with the network phases, which run only when the identification switch is on. The tracks phase is skipped while `musicbrainz` is paused. State is re-read at most every 2 s between items. The next cycle is queued at `now + 5 s` when any item was processed, else `now + 60 s` with status `idle`, through `enqueue_at_unless_queued_with_key` so a queued kick wins. A cycle starting less than `MIN_INTERVAL` after the previous one sleeps the rest (cancellable).
  - `IdentCtx` gains `last_cycle: Arc<Mutex<Option<Instant>>>`.
  - `coordinator::handler(ctx: IdentCtx) -> Handler` and `coordinator::prepare(pool) -> sqlx::Result<()>` (requeue running `enrich` jobs; `UPDATE artists SET enrichment_status = 'pending' WHERE enrichment_status = 'enriching'`; kick).
  - `kima_server::jobs::Registry::register_enrich(&mut self, ctx: IdentCtx)` (concurrency 1, default lease); `register_library` passes `handler_with(ctx, <kick>)`.
  - The scheduler is always spawned, with entries `library-sync` (when `KIMA_LIBRARY_SYNC_CRON` is not `off`), `http-cache-prune` (`17 3 * * *`: `prune_expired` then `prune_unused`) and `enrich-kick` (`*/10 * * * *`: `kick`). `kima_server::schedule_entries(config: &Config, sync: Option<Cron>) -> Vec<(&'static str, Cron, EnqueueFn)>` builds them.
  - A settings POST that turns the effective switch on calls `kick`.

- [ ] **Step 1: Failing tests:**
  - `paused_or_halted_cycle_does_nothing` (and enqueues nothing); `stopping_cycle_goes_idle`
  - `halt_survives_kick_and_scan`: with `halted`, `kick` and a library reconcile with changes run no work.
  - `cycle_fingerprints_and_identifies` (ffmpeg-gated; replay MusicBrainz).
  - **Review Focus 1,** `identify_off_makes_no_request`: every `Bases` field points at one `Replay` with an empty recording; tagged and untagged tracks; switch off; two cycles. `asked()` is empty, every track has a `track_acoustid` row, no recording row is `done`.
  - `paused_musicbrainz_skips_tracks_phase`
  - `next_cycle_in_5s_after_work_and_60s_when_idle` (reads `jobs.run_at`)
  - `kick_moves_queued_cycle_to_now`; `kick_enqueues_when_none_queued`; `concurrent_kicks_queue_one`
  - `min_interval_between_cycles`
  - `prepare_resets_enriching_artists`
  - `reconcile_with_changes_kicks` and `unchanged_reconcile_does_not_kick` (library_jobs.rs)
  - `schedule_has_prune_and_kick_always` (sync off still gives two entries)
  - `turning_switch_on_kicks` (settings.rs)
- [ ] **Step 2: Run** `cargo test -p kima-ident --test coordinator && cargo test -p kima-server`. Expected: fail.
- [ ] **Step 3: Implement and wire.** Main builds `IdentCtx` from the `AppState` pieces and the `LibraryCtx`, calls `coordinator::prepare` next to `prepare_library_queue`, and registers the `enrich` queue.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): enrichment coordinator on the enrich queue; scheduler always runs`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 19: Operator controls, progress, status and failures

**Files:**
- Create: `crates/kima-ident/src/coordinator/control.rs`, `crates/kima-ident/tests/control.rs`, `crates/kima-db/tests/enrichment.rs`
- Modify: `crates/kima-db/src/enrichment.rs`, `crates/kima-ident/src/coordinator/mod.rs`
- Test: the two test files

**Divergences:** none (D-108 is complete).

**Interfaces:**
- Consumes: `coordinator::kick`, `enrichment::State` (Task 18), `KvSettings`, `Net::set_concurrency`, `acoustid::client_key`.
- Produces:
  - `control::ControlError::WrongState(String)`, messages `Cannot pause enrichment in <status> state` and `Cannot resume enrichment in <status> state`.
  - `control::pause(pool) -> Result<State, ControlError>` (only from `running`), `resume(pool) -> Result<State, ControlError>` (from `paused`; `running` is a no-op; kicks), `stop(pool) -> sqlx::Result<State>` (`idle` is a no-op; else `stopping`, `halted`, `stopped_at`), `sync(pool) -> sqlx::Result<SyncCounts { artists: i64, tracks: i64 }>` (clears `halted` and `paused`, kicks).
  - `control::retry_failed_and_blocked(ctx: &IdentCtx) -> sqlx::Result<u64>`: first reopens `recording` rows `done` with `evidence->>'acoustid' = 'skipped'` whose `acoustidReason` no longer holds (`no_key` with a key now resolvable, `no_fingerprint` with a `track_acoustid` row now, `fingerprinting_unavailable` with fingerprinting now available, `disabled` with `ACOUSTID_DISABLED` now off); then rows `failed` with `next_attempt_at IS NOT NULL` → `pending` now; kicks.
  - `control::redecide_everything(ctx) -> sqlx::Result<u64>` (spec §10.7; `recording` rows of tracks with `recording_mbid` kept), `wipe_and_rerun(pool) -> sqlx::Result<WipeCounts { artists: i64, tracks: i64 }>` (spec §10.7 in one transaction; kicks after commit), `set_concurrency(ctx, c: i64) -> sqlx::Result<u8>` (clamped 1..5, stored, applied to `Net`).
  - `coordinator::hung(s: &State, now: OffsetDateTime) -> bool` (`running` and `last_activity < now − 15 min`).
  - `kima_db::enrichment::Progress` with the spec §10.8 shape (serde camelCase, `source: "TrackStage"`, zeroed `audioAnalysis`/`clapEmbeddings`/`similarity`) and `progress(pool, fingerprinting: bool) -> sqlx::Result<Progress>`; `StatusCounts` and `status_counts(pool)`.
  - `FailureFilter { entity_type: Option<String>, limit: i64, offset: i64 }`, `FailureRow` (spec §10.9 fields, serde camelCase) and `failures(pool, f: &FailureFilter) -> sqlx::Result<(Vec<FailureRow>, i64)>`; `FailureCounts { artist, track, audio, vibe, scan, podcast, total }` and `failure_counts(pool)`.

- [ ] **Step 1: Failing tests** (ports of `enrichmentStateMachine`, `enrichmentState.heartbeat`, `unifiedEnrichment.{redecideEverything, retryFailedAndBlocked, noWipe, missingSkipped, progressReadsTrackStage}`, `enrichmentRestart.integration` (11), `enrichmentFailureService(.trackStages)`, `enrichment.failuresTrackStages`):
  - state: `pause_only_from_running`, `resume_from_paused_noop_when_running`, `stop_idempotent_when_idle`, `sync_clears_halt_and_counts`, `hung_after_15_minutes`, `activity_stamped_on_count_change_only`.
  - `retry_reopens_skipped_acoustid_once_a_key_exists`, `retry_reopens_only_due_failures`, `retry_never_touches_done_or_terminal`.
  - `redecide_resets_counters_keeps_matched_recordings`.
  - wipe: `wipe_keeps_mbid_image_and_fingerprints`, `wipe_clears_bio_similar_and_mb_genres`, `wipe_reopens_done_and_working_recordings_only`, `wipe_clears_only_musicbrainz_isrc`, `wipe_never_touches_user_overrides`, plus the remaining `enrichmentRestart.integration` cases by name.
  - progress: `no_row_counts_as_pending`, `waiting_is_behind_unsettled_fingerprint`, `waiting_missing_counts_missing_tracks`, `va_excluded_from_artists`, `core_and_fully_complete_flags`.
  - failures: `track_row_id_is_track_stage`, `scan_entity_for_fingerprint_stage`, `file_missing_left_out`, `artist_failures_listed`, `entity_type_filters`, `ordered_last_failed_desc_with_total`, `counts_shape`.
  - `concurrency_clamped_and_applied`
- [ ] **Step 2: Run** `cargo test -p kima-ident --test control && cargo test -p kima-db --test enrichment`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): enrichment controls, progress, status and failures`.

### Task 20: Source clients: Wikidata, fanart.tv, Deezer, ListenBrainz, Cover Art Archive; `test-fanart`

**Files:**
- Create: `crates/kima-ident/src/{wikidata.rs, fanart.rs, deezer.rs, listenbrainz.rs, coverartarchive.rs}`, `crates/kima-ident/tests/sources.rs`, `crates/kima-ident/tests/fixtures/{wikidata,fanart,deezer,listenbrainz,coverartarchive}/*.json`
- Modify: `crates/kima-core/src/config.rs` (`fanart_api_key`), `crates/kima-ident/src/ctx.rs` (`fanart_env_key`), `crates/kima-api/Cargo.toml` (adds `kima-ident`), `crates/kima-api/src/routes/system_settings.rs`
- Test: `crates/kima-ident/tests/sources.rs`, `crates/kima-api/tests/settings.rs`

**Divergences:** D-110 completed (the `test-fanart` clause).

**Interfaces:**
- Consumes: `Net`, `Req`, `Host::*` (Tasks 6–7), `text::{normalize_string, names_agree}`, `Mbid` (Task 12), `KvSettings`.
- Produces:
  - `wikidata::WikiInfo { bio: Option<String>, image_url: Option<String> }` and `wikidata::lookup(net, mbid: Mbid) -> Outcome<WikiInfo>` (spec §12.2; no entity → `Answered(WikiInfo::default())`; a summary 404 → no bio); `wikidata::commons_url(file: &str) -> String`.
  - `fanart::key(kv: &KvSettings, env: Option<&str>) -> Option<String>` (the stored key only when `fanart_enabled`, else env); `fanart::artist_image(net, key: &str, mbid: Mbid) -> Outcome<Option<String>>` (first of `artistbackground`, `artistthumb`, `hdmusiclogo`; a bare file name is prefixed `https://assets.fanart.tv/fanart/music/{mbid}/{key}/`); `fanart::album_cover(net, key, rg: Mbid) -> Outcome<Option<String>>` (`albums.{rg}.albumcover[0]`, else `cdart[0]`); `fanart::TestResult { Ok, Rejected, NoAnswer }` and `fanart::test_key(net, key: &str) -> TestResult` (`GET /music/b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d`, 5 s timeout, `no_cache`).
  - `deezer::DeezerArtist { id: u64, name: String, picture: Option<String> }`; `deezer::find_artist(net, name: &str) -> Outcome<Option<DeezerArtist>>` (`/search/artist?q=<name>&limit=1`, verified by `normalize_string` equality; `picture_xl || picture_big || picture_medium`); `deezer::related(net, id: u64) -> Outcome<Vec<String>>` (`/artist/{id}/related?limit=30`); `deezer::album_cover(net, artist: &str, title: &str) -> Outcome<Option<String>>` (spec §12.7 step 1).
  - `listenbrainz::Similar { name: String, mbid: Option<Mbid>, score: f64 }` and `listenbrainz::similar(net, mbid: Mbid) -> Outcome<Vec<Similar>>` (spec §12.5 algorithm string; filtered, self excluded, sorted, top 30).
  - `coverartarchive::front(net, rg: Mbid) -> Outcome<Option<String>>` (`thumbnails.1200`, else `image`).
  - `Config.fanart_api_key: Option<String>` (`FANART_API_KEY`); `IdentConfig.fanart_env_key`; `AppState` gains `fanart_env_key: Option<String>`.
  - `POST /api/system-settings/test-fanart` (admin), spec §15.2: a body key, else the stored key (even when `fanartEnabled` is false), else 400 `{"error":"API key is required"}`; switch off → 409 `{"error":"Outbound lookups are turned off"}`; `Ok` → 200 `{"success":true,"message":"Fanart.tv connection successful"}`; `Rejected` (401/403) → 400 `{"error":"Fanart.tv rejected the API key"}`; `NoAnswer` → 502 `{"error":"Fanart.tv did not answer"}`.

- [ ] **Step 1: Fixtures.** Record one artist each (The Beatles, `b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d`) with `tools/record-fixtures` (fanart with a throwaway key; the recorder redacts it), plus hand-written 404 and 429 entries.
- [ ] **Step 2: Failing tests:**
  - `wikidata.outcome.test.ts` (5) port; `commons_url_uses_md5_path`; `p18_prefers_preferred_then_normal_never_deprecated`; `summary_404_is_no_bio_not_failure`.
  - `deezerVerifiedMatch.test.ts` and `deezerRelatedArtists.test.ts` ports; `deezer_album_cover_needs_artist_and_title_agree`.
  - `listenbrainz_filters_sorts_and_excludes_self`; `listenbrainz_empty_is_absent`.
  - `caa_front_prefers_1200_thumbnail`; `fanart_first_of_three_kinds_and_bare_name_prefixed`; `fanart_404_is_none`.
  - API: `test_fanart_uses_stored_key`, `test_fanart_body_key_wins`, `test_fanart_no_key_400`, `test_fanart_identify_off_409_no_request`, `test_fanart_rejected_400`, `test_fanart_no_answer_502`, `test_fanart_ok`, `test_fanart_not_cached` (two presses ask twice), `test_fanart_non_admin_403`.
- [ ] **Step 3: Run** `cargo test -p kima-ident --test sources && cargo test -p kima-api --test settings`. Expected: fail.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 6: Commit** with message `feat(ident): Wikidata, fanart.tv, Deezer, ListenBrainz and Cover Art Archive clients; test-fanart`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 21: Artist enrichment: MBID, bio, image, genres, similar artists

**Files:**
- Create: `crates/kima-ident/src/{artist.rs, similar.rs}`, `crates/kima-ident/tests/artist.rs`
- Modify: `crates/kima-db/src/{identity.rs, library.rs}` (`prune_empty`), `crates/kima-library/src/{artwork.rs, scan.rs}`, `crates/kima-ident/src/coordinator/mod.rs` (artists phase)
- Test: `crates/kima-ident/tests/artist.rs`, `crates/kima-library/tests/{artwork.rs, scan.rs}`

**Divergences:** D-113: the clauses "images downloaded into `<data>/artist-images`" and "no relation table until M5. Amends D-043, D-048" (add the amendment notes to D-043 and D-048).

**Interfaces:**
- Consumes: `musicbrainz::{artist, search_artists, artist_release_groups, top_genres}` (Task 15), Task 20's clients, `Net::get_bytes` (Task 7), `genres::replace_artist_genres` (Task 10), `text::*` (Task 12), the coordinator (Task 18).
- Produces:
  - `kima_library::artwork::{artist_image_path(data_dir: &Path, artist: Uuid) -> PathBuf` (`<data_dir>/artist-images/<id>.jpg`), `normalize_image(bytes: &[u8]) -> Result<Vec<u8>, ArtworkError>` (decode with `image`, at most 1500 px on the long side, JPEG q90), `find_local_artist_image(root: &Path, album_dirs: &[PathBuf]) -> Option<PathBuf>` (spec §12.3 step 2: nearest folder above the album folders up to the root; `artist.` or `folder.` `jpg|png|webp`; > 1000 bytes; not a symlink)}.
  - `kima_db::library::prune_empty` **changes** to return `sqlx::Result<Pruned { albums: Vec<Uuid>, artists: Vec<Uuid> }>`; `scan::prune_empty` also deletes each pruned artist's image file.
  - `kima_db::identity::{ArtistForEnrichment { id, name, mbid: Option<Uuid>, mbid_source: Option<String>, image_source: Option<String>, enrichment_failures: i16, album_titles: Vec<String>, album_dirs: Vec<String> }, claim_artists(pool, n: i64) -> sqlx::Result<Vec<ArtistForEnrichment>>` (spec §10.5 pick and claim; VA excluded), `save_artist_outcome(pool, id, out: &ArtistWrite) -> sqlx::Result<()>}` with `ArtistWrite` carrying the new status, failure count, `last_error`/`last_error_code`, and optional `mbid`/`mbid_source`, `bio`, `image_path`/`image_source`, `similar`.
  - `artist::resolve_mbid(net, name: &str, album_titles: &[String]) -> Outcome<Mbid>` (spec §12.1, `MAX_CATALOGUE_CHECKS = 3`) and `artist::mbid_belongs(net, mbid: Mbid, names: &[String]) -> Outcome<bool>` (the guard).
  - `similar::Entry { name: String, mbid: Option<Mbid>, r#match: f64 }` (serialised `{name, mbid, match}`) and `similar::merge(lb: &[listenbrainz::Similar], deezer: &[String]) -> Vec<Entry>` (spec §12.5 match formulas, LB ≥ 30 → LB only, dedupe by lower-cased name with LB first, cut at 30).
  - `artist::ArtistResult { Completed, Pending, Failed { code: String, message: String }, Unresolvable }` and `artist::enrich_one(ctx: &IdentCtx, a: &ArtistForEnrichment) -> ArtistResult` (spec §12.1–§12.5 in upstream's order; a failed source never records "none"; a lower-precedence image is replaced only by a higher one).
  - `artist::run_batch(ctx, cancel) -> sqlx::Result<BatchReport>`: claims 10, up to `enrichmentConcurrency` at once; at `enrichment_failures ≥ 4` → `unresolvable`.
  - The coordinator runs the artists phase before the tracks phase (skipped while `musicbrainz` is paused).

- [ ] **Step 1: Failing tests:**
  - `resolveArtistMbid.test.ts` (10) port (the `getArtistCorrection` cases dropped).
  - `artistFailureCap.test.ts` port: the 4th failure gives `unresolvable`; only rate-limited gives `pending`.
  - `artistEnrichmentArtwork.test.ts` port, including `drops_another_artists_mbid` ("Alexis Ffrench holding Sam Cooke's"): MBID, bio, MusicBrainz genres and similar are dropped, a local image kept.
  - `artwork.test.ts` (selected) port: `local_image_beats_wikidata`, `failed_source_never_records_none`, `higher_source_replaces_lower`, `user_image_never_replaced`.
  - `artistSimilarity.tiers.test.ts` port: LB ≥ 30 → LB only; merge and dedupe; both empty → `[]`; a failed source keeps the old value and fails the step.
  - `artist_genres_top5_only_when_non_empty`; `va_never_looked_up`; `claim_order_and_unresolvable_retry_after_7_days`.
  - `imageStorage.test.ts` (5) port for `artist_image_path` and `find_local_artist_image` (path traversal, symlinks, small files).
  - `stored_image_is_jpeg_at_most_1500px`; `prune_deletes_artist_image` (scan.rs).
- [ ] **Step 2: Run** `cargo test -p kima-ident --test artist`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): artist enrichment: MBID, bio, image, genres, similar artists`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 22: Album cover fallback and image serving by id

**Files:**
- Create: `crates/kima-ident/src/album_covers.rs`, `crates/kima-ident/tests/album_covers.rs`
- Modify: `crates/kima-ident/src/artist.rs`, `crates/kima-db/src/identity.rs`, `crates/kima-library/src/{artwork.rs, scan.rs}`, `crates/kima-api/src/media/cover.rs`, `crates/kima-api/src/routes/cover_art.rs`
- Test: `crates/kima-ident/tests/album_covers.rs`, `crates/kima-api/tests/cover_art.rs`

**Divergences:** D-113: the clause "served by `GET /library/cover-art/<artistId>`".

**Interfaces:**
- Consumes: `deezer::album_cover`, `coverartarchive::front`, `fanart::album_cover` (Task 20), `normalize_image` (Task 21), `media::cover` (M2).
- Produces:
  - `kima_library::artwork::album_image_path(data_dir: &Path, album: Uuid) -> PathBuf` (`<data_dir>/album-images/<id>.jpg`); `scan::prune_empty` deletes pruned albums' files.
  - `kima_db::identity::{albums_without_cover(pool, artist: Uuid) -> sqlx::Result<Vec<AlbumForCover { id, title, artist_name, release_group_mbid: Option<Uuid> }>>` (no `image_path`, and no embedded or folder cover: checked by the caller with `artwork::find_cover` on one track path), `set_album_image(pool, album: Uuid, path: &str, source: &str) -> sqlx::Result<()>}`.
  - `album_covers::fill_for_artist(ctx: &IdentCtx, artist: Uuid) -> Outcome<usize>`: batches of 3, deezer → coverartarchive → fanart (spec §12.7); called by `artist::enrich_one` after the image step.
  - `media::cover(state, id, size)` **extends**: a user album image (`image_source = 'user'`) first; then embedded and folder art (M1); then the album fallback file; and when `id` is no album, the artist image. Variants of data-dir images are cached under `<data_dir>/artwork/<id>/` and rebuilt when the source file's mtime is newer than the variant. Size snapping and headers as M2.

- [ ] **Step 1: Failing tests:**
  - `deezer_then_caa_then_fanart_order`; `album_with_embedded_cover_untouched`; `failed_source_records_nothing`
  - `cover_route_serves_artist_id_with_size_snapping`; `cover_route_unknown_id_404`
  - `fallback_served_after_embedded_and_folder`; `user_album_image_first`
  - `variant_rebuilt_when_image_replaced`
  - `prune_deletes_album_image`
- [ ] **Step 2: Run** `cargo test -p kima-ident --test album_covers && cargo test -p kima-api --test cover_art`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass, with the M1/M2 cover tests unchanged.
- [ ] **Step 5: Commit** with message `feat: album cover fallback; cover route serves artist images`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 23: Genres: effective genres and `/api/library/genres`

**Files:**
- Create: `crates/kima-api/src/routes/library/genres.rs`, `crates/kima-db/tests/genres.rs`, `crates/kima-api/tests/genres.rs`
- Modify: `crates/kima-db/src/genres.rs`, `crates/kima-api/src/routes/library/mod.rs`
- Test: the two test files

**Divergences:** D-114: the clauses "track, album and artist genres each from MusicBrainz, else file tags" and "`/library/genres` keeps upstream's heuristics over artist genres".

**Interfaces:**
- Consumes: the 0019 views (Task 8), `genres` module (Task 10), `text::normalize_artist_name` (Task 12).
- Produces:
  - `kima_db::genres::{effective_for_tracks(pool, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, Vec<String>>>`, `effective_for_albums`, `effective_for_artists` (same shape, ordered by rank), `musicbrainz_for_albums`, `musicbrainz_for_artists` (the `tags` alias), `merged(user: &[String], genres: &[String]) -> Vec<String>` (user first, unique case-insensitively), `library_genres(pool) -> sqlx::Result<Vec<(String, i64)>>` (spec §13.2 query: effective artist genres, distinct present tracks of the artist's albums, `HAVING count >= 15`, count desc, `LIMIT 20`)}.
  - `GET /api/library/genres` (user): `library_genres`, then drops genres equal to some artist's name under `normalize_artist_name` (candidates by `lower(unaccent(name))`, confirmed in Rust); answers `{"genres":[{"genre":<lowercase>,"count":n}]}`.

- [ ] **Step 1: Failing tests:**
  - `track_effective_musicbrainz_then_file`, `album_and_artist_effective_same_rule`, `underscore_genres_never_stored`, `merged_puts_user_first_unique`
  - `library_genres_threshold_15_and_limit_20`, `library_genres_drops_artist_named_genres`, `library_genres_lowercase`, `library_genres_needs_auth`
- [ ] **Step 2: Run** `cargo test -p kima-db --test genres && cargo test -p kima-api --test genres`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat: effective genres and /api/library/genres`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 24: Lyrics: LRCLIB and `/api/library/tracks/:id/lyrics`

**Files:**
- Create: `crates/kima-ident/src/{lrclib.rs, lyrics.rs}`, `crates/kima-api/src/routes/library/lyrics.rs`, `crates/kima-ident/tests/lyrics.rs`, `crates/kima-api/tests/lyrics.rs`, `crates/kima-ident/tests/fixtures/lrclib/*.json`
- Modify: `crates/kima-db/src/lyrics.rs`, `crates/kima-api/src/routes/library/mod.rs`
- Test: the two test files

**Divergences:** D-115 completed ("with lookups off or a limiter wait over 5 s the route answers from storage").

**Interfaces:**
- Consumes: `Net` (`Host::Lrclib`, `Req::max_wait`), `IdentifySwitch`, `kima_db::lyrics` (Task 10).
- Produces:
  - `kima_db::lyrics::{Lyrics { plain: Option<String>, synced: Option<String>, source: String, fetched_at: OffsetDateTime }, get(pool, track: Uuid) -> sqlx::Result<Option<Lyrics>>`, `upsert_lrclib(pool, track, plain, synced, lrclib_id: i32)`, `upsert_none(pool, track)`, `lookup_fields(pool, track) -> sqlx::Result<Option<(String /*title*/, String /*album artist*/, String /*album*/, i32 /*duration*/)>>}`.
  - `lrclib::Answer { id: i32, plain_lyrics: Option<String>, synced_lyrics: Option<String> }` and `lrclib::get(net, title, artist, album, duration_secs: i32) -> Outcome<Answer>` (spec §14.1; 404 → `Absent`; `max_wait(5 s)`; no cache).
  - `lyrics::LyricsAnswer { plain: Option<String>, synced: Option<String>, source: String }` and `lyrics::for_track(pool: &PgPool, net: &Net, identify: &IdentifySwitch, track: Uuid) -> sqlx::Result<Option<LyricsAnswer>>` (`None` = no such track), following spec §14.2 steps 2–5.
  - `GET /api/library/tracks/{id}/lyrics` (user): unknown or non-UUID → 404 `{"error":"Track not found"}`; else `{plainLyrics, syncedLyrics, source}`.

- [ ] **Step 1: Fixtures.** Record two public tracks with `tools/record-fixtures` (one with synced lyrics, one 404); add a hand-written 500.
- [ ] **Step 2: Failing tests:**
  - `unknown_and_non_uuid_404`; `embedded_row_returned_without_request`; `none_younger_than_30_days_asks_nothing`; `none_older_than_30_days_asks_again`
  - `hit_upserts_lrclib`; `lrclib_404_stores_none`; `failure_answers_existing_values_writes_nothing`
  - `identify_off_answers_from_storage_no_request`
  - `limiter_wait_over_5s_answers_from_storage`: three concurrent requests for different tracks with no stored rows; at 1 per 3 s the third would wait 6 s, so it answers nulls without a request and writes nothing (paused clock).
  - `missing_track_answers_like_present`
- [ ] **Step 3: Run** `cargo test -p kima-ident --test lyrics && cargo test -p kima-api --test lyrics`. Expected: fail.
- [ ] **Step 4: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 5: Run.** Expected: pass. Also run `npm run test:unit` in `frontend/`: `hooks/__tests__/useLyricsSync.test.tsx` passes unchanged.
- [ ] **Step 6: Commit** with message `feat: lyrics from tags and LRCLIB with a 30-day none cache`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 25: `/api/enrichment` routes

**Files:**
- Create: `crates/kima-api/src/routes/enrichment.rs`, `crates/kima-api/tests/enrichment.rs`
- Modify: `crates/kima-api/src/routes/mod.rs`, `crates/kima-api/src/state.rs` (progress memo)
- Test: `crates/kima-api/tests/enrichment.rs`

**Divergences:** D-112: the clause "those routes are not ported (the frontend never calls them)".

**Interfaces:**
- Consumes: `coordinator::control::*`, `coordinator::hung`, `enrichment::{progress, status_counts, failures, failure_counts, state}` (Tasks 18–19).
- Produces: the spec §16.2 table except the four metadata/reset routes (Task 27), with upstream's shapes:
  - `GET /enrichment/progress` memoised 5 s (`AppState.progress_memo: Arc<Mutex<Option<(Instant, Value)>>>`).
  - `GET /enrichment/status` (idle answers `{status:"idle", currentPhase:null, hung:false, …}`).
  - `POST /enrichment/{pause,resume,stop}` → `{message, state}`, with messages `Enrichment paused`, `Enrichment resumed`, `Enrichment stopping...`; `WrongState` → 400 `{error}`.
  - `POST /enrichment/sync` → `{message:"Incremental sync started", description, result:{artists, tracks, audioQueued:0}}`.
  - `POST /enrichment/retry-failed-and-blocked` → `{message, description, queued}`; `POST /enrichment/re-decide-everything` → `{message, description, reopened}`; `POST /enrichment/wipe-and-rerun` → `{artists, tracks, vectors:0}`. `message`/`description` texts are copied from upstream `routes/enrichment.ts`.
  - `GET /enrichment/failures?entityType&includeSkipped&includeResolved&limit=100&offset=0` → `{failures, total}`; `GET /enrichment/failures/counts`.
  - `GET /enrichment/concurrency` and `PUT /enrichment/concurrency {concurrency}` → `{concurrency, estimatedSpeed:"~{10c} artists/min, ~{60c} tracks/min", artistsPerMin:10c, tracksPerMin:60c}`; a missing or non-numeric value → 400 `{"error":"Missing or invalid 'concurrency' parameter"}`.
  - Reads are user, writes admin (403 `{"error":"Admin access required"}`).

- [ ] **Step 1: Failing tests** (plus `enrichmentStatus.route.test.ts` port: `status_reports_hung`):
  - `progress_shape_exact` (every key of spec §10.8); `progress_memoised_5s`
  - `status_idle_shape`
  - `pause_wrong_state_400_message`; `pause_resume_stop_answers_state`
  - `sync_answer_shape_and_counts`; `retry_answer_queued`; `redecide_answer_reopened`; `wipe_answer_shape`
  - `failures_paging_and_filter`; `failure_counts_shape`
  - `concurrency_put_clamps_and_estimates`; `concurrency_put_invalid_400`
  - `writes_need_admin_403`; `reads_need_auth_401`
- [ ] **Step 2: Run** `cargo test -p kima-api --test enrichment`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(api): /api/enrichment controls, progress, status, failures and concurrency`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 26: Library detail JSON: artists, albums, tracks

**Files:**
- Modify: `crates/kima-api/src/routes/library/{artists.rs, albums.rs, tracks.rs, mod.rs}`, `crates/kima-db/src/browse.rs`, `crates/kima-ident/src/similar.rs`
- Test: `crates/kima-api/tests/library.rs`

**Divergences:** D-113: the clauses "page views never call out; similar artists outside the library have no image".

**Interfaces:**
- Consumes: `genres::{effective_for_*, musicbrainz_for_*, merged}` (Task 23), the artist columns written by Task 21, `artwork::{artist_image_path, album_image_path}`.
- Produces:
  - `browse::ArtistRow` gains `mbid: Option<Uuid>`, `bio`, `user_bio`, `display_name: Option<String>`, `has_image: bool`, `user_genres: Vec<String>`, `similar: Option<serde_json::Value>`; `browse::AlbumRow` gains `release_mbid`, `release_group_mbid: Option<Uuid>`, `label`, `display_title: Option<String>`, `display_year: Option<i32>`, `user_genres: Vec<String>`; `browse::TrackRow` gains `recording_mbid: Option<Uuid>`, `isrc: Option<String>`.
  - `similar::ResolvedSimilar { id: String, name, mbid: Option<String>, cover_art: Option<Uuid>, album_count: i64 /*0*/, owned_album_count: i64, weight: f64, in_library: bool }` and `similar::resolve(pool, entries: &[similar::Entry], limit: usize) -> sqlx::Result<Vec<ResolvedSimilar>>`: by MBID, then by `normalize_artist_name` (candidates by `lower(unaccent(name))`, confirmed in Rust); first `limit`.
  - JSON per spec §16.1: artist detail `mbid`, `bio` (= `userSummary ?? summary`), `summary`, `userSummary`, `coverArt`/`image`/`heroUrl` (the artist id when it has an image, else null), `userHeroUrl: null`, `genres` (merged), `userGenres`, `tags`, `displayName`, `hasUserOverrides`, `similarArtists` (first 10); the artists list's `coverArt`/`heroUrl` the same way; album detail `mbid`, `rgMbid`, `genres`, `userGenres`, `label`, `displayTitle`, `displayYear`, `userCoverUrl: null`; track JSON `recordingMbid`, `isrc`, `genres` (effective).

- [ ] **Step 1: Failing tests:**
  - `artist_detail_filled`, `artist_bio_prefers_user_summary`, `artist_cover_art_is_id_only_with_image`, `artists_list_cover_art`
  - `similar_resolved_by_mbid_then_name` (`Beyonce` stored, `Beyoncé` similar entry matches); `similar_outside_library_has_no_image`; `similar_first_10`
  - `page_view_makes_no_request`: an artist with no similar or image, `AppState` with a `Net` pointed at a replay server; `asked()` stays empty.
  - `album_detail_filled`; `track_detail_has_recording_isrc_genres`
  - The M1 library tests pass unchanged.
- [ ] **Step 2: Run** `cargo test -p kima-api --test library`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(api): artist, album and track pages carry identity, genres, images and similar artists`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 27: MetadataEditor overrides: PUT metadata and reset

**Files:**
- Modify: `crates/kima-api/src/routes/enrichment.rs`, `crates/kima-db/src/identity.rs`, `crates/kima-db/src/stages.rs`
- Test: `crates/kima-api/tests/metadata_editor.rs` (new)

**Divergences:** D-118 (in full); D-117 completed (the editor still answers 409 for an MBID another row holds).

**Interfaces:**
- Consumes: `Net::get_bytes` (Task 7), `normalize_image`, `artist_image_path` (Task 21), `album_image_path` (Task 22), the detail JSON builders (Task 26), `coordinator::kick`.
- Produces:
  - `kima_db::identity::{ArtistOverrides { display_name, user_bio: Option<Option<String>>, user_genres: Option<Vec<String>>, mbid: Option<Uuid> }`, `apply_artist_overrides(pool, id, o: &ArtistOverrides) -> sqlx::Result<Applied { NotFound, MbidTaken, Done }>`, the album equivalent (`display_title`, `display_year`, `user_genres`, `release_group_mbid`), `reset_artist(pool, id)`, `reset_album(pool, id)`}.
  - `kima_db::stages::reopen_album_recordings(conn, album: Uuid) -> sqlx::Result<u64>`: the album's tracks' `recording` rows that are `done` with a source other than `file_tag` → `pending`.
  - Routes (admin), spec §16.2–§16.3: `PUT /enrichment/artists/{id}/metadata`, `PUT /enrichment/albums/{id}/metadata`, `POST /enrichment/artists/{id}/reset`, `POST /enrichment/albums/{id}/reset`. Errors: 404 `{"error":"Artist not found"}`/`{"error":"Album not found"}`; 400 `{"error":"year must be a number"}`; 400 `{"error":"Invalid MusicBrainz ID"}`; 409 `{"error":"MBID <x> is already in use by another artist"}` (album: `…by another album`); 400 `{"error":"Could not fetch that image"}` (15 s budget). An artist MBID change sets `enrichment_status = 'pending'` and kicks; an `rgMbid` change calls `reopen_album_recordings` and kicks.

- [ ] **Step 1: Failing tests:**
  - `name_bio_genres_set_and_cleared` (empty string and null clear); `album_title_year_set_and_cleared`; `year_not_a_number_400`
  - `invalid_mbid_400`; `mbid_in_use_409`; `empty_mbid_unchanged`; `artist_mbid_change_marks_pending`; `rg_mbid_change_reopens_non_tag_recordings`
  - `hero_url_downloaded_and_served` (replay image); `hero_url_failure_400`
  - `sort_name_follows_real_name` (inverted `enrichmentSortName.route.test.ts`)
  - `reset_clears_overrides_user_mbids_and_images` (R2); `reset_needs_admin_403`; `reset_unknown_404`
  - `put_answer_shapes` (artist detail plus `albums:[{id,title,year,coverUrl}]`; album plus `artist{id,name}` and `tracks[{id,title,trackNo,duration}]`)
- [ ] **Step 2: Run** `cargo test -p kima-api --test metadata_editor`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(api): MetadataEditor overrides and reset`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 28: Subsonic genres and MusicBrainz ids

**Files:**
- Modify: `crates/kima-db/src/subsonic.rs`, `crates/kima-subsonic/src/map.rs`, `crates/kima-subsonic/src/routes/{search.rs, browse.rs}`, `crates/kima-subsonic/tests/fixtures/subsonic.sql` (adds MBIDs, file and MusicBrainz genres, lyrics, an artist image path), `crates/kima-subsonic/tests/snapshots/*.snap`
- Test: `crates/kima-subsonic/tests/{search.rs, browse.rs, snapshots.rs}`, unit tests in `map.rs`

**Divergences:** D-114 completed (Subsonic counts and exact matching, song `genre` the track's first genre); D-116: the clause "songs, albums and artists carry `musicBrainzId`"; note in D-082 that it is amended by D-116 (genre present).

**Interfaces:**
- Consumes: `genres::effective_*` (Task 23).
- Produces:
  - `SongItem` gains `recording_mbid: Option<Uuid>` and `genre: Option<String>` (first effective track genre, else the album artist's first effective genre); `AlbumItem` gains `release_mbid: Option<Uuid>`, `genre: Option<String>`; `ArtistItem` gains `mbid: Option<Uuid>`.
  - `AlbumListKind::ByGenre(String)`; `random_tracks` takes `genre: Option<&str>`; `songs_by_genre(pool, user, genre: &str, count: i64, offset: i64) -> sqlx::Result<Vec<SongItem>>`; `genre_counts(pool) -> sqlx::Result<Vec<(String, i64 /*songs*/, i64 /*albums*/)>>`. All genre matches are case-insensitive exact on `genres.key`.
  - Endpoints per spec §18 rows 14, 19, 20, 24; `map::song`/`album_*`/`artist_*` emit `genre` and `musicBrainzId` only when present.

- [ ] **Step 1: Failing tests:**
  - `get_genres_counts_distinct_tracks_and_albums`; `get_genres_sorted_by_name`
  - `songs_by_genre_missing_10`, `songs_by_genre_exact_case_insensitive` (`rock` does not match `post-rock`), `songs_by_genre_paging_and_order`
  - `album_list2_by_genre`, `album_list2_by_genre_missing_10` (message `byGenre requires genre`)
  - `random_songs_genre_filter`
  - `song_genre_falls_back_to_artist`; `music_brainz_ids_only_when_present`
  - `map.rs`: replace the M2 "never a genre" assertion with `genre_attribute_when_known`.
- [ ] **Step 2: Run** `cargo test -p kima-subsonic`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`. Update the snapshots with `INSTA_UPDATE=always` and read every changed `.snap` against spec §18.
- [ ] **Step 4: Run** `cargo test -p kima-subsonic` with `INSTA_UPDATE=no`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(subsonic): real genres and MusicBrainz ids`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 29: Subsonic artist info, album info, lyrics, artist cover art

**Files:**
- Create: `crates/kima-subsonic/src/routes/info.rs`, `crates/kima-subsonic/tests/info.rs`
- Modify: `crates/kima-subsonic/Cargo.toml` (adds `kima-ident`), `crates/kima-subsonic/src/routes/{mod.rs, stubs.rs, media.rs, system.rs}`, `crates/kima-subsonic/tests/{media.rs, system.rs, snapshots.rs}`, snapshots
- Test: the files above

**Divergences:** D-116 completed (`albumInfo` with `musicBrainzId`, 70/10; no image URL attributes; `songLyrics` advertised; amends D-075, D-082, D-086, with notes added to those rows); D-113 completed (Subsonic `getCoverArt ar-<id>`).

**Interfaces:**
- Consumes: `similar::resolve` (Task 26), `kima_db::lyrics::get` (Task 24), `media::cover` (Task 22), `text::normalize_artist_name`.
- Produces endpoints per spec §18: `getOpenSubsonicExtensions` adds `songLyrics` [1]; `getCoverArt` serves `ar-<id>` and bare artist ids (70 `Cover art not found` without an image); `getAlbumInfo`/`getAlbumInfo2`; `getArtistInfo`/`getArtistInfo2` (`count` default 20, max 50); `getLyrics`; `getLyricsBySongId` (line regex and fraction rule of spec §18 row 63; `lang:"und"`). The four info/lyrics handlers move out of `stubs.rs`.

- [ ] **Step 1: Failing tests** (`routes/subsonic/__tests__/artistInfo.route.test.ts` port replaces the M2 stub snapshot):
  - `artist_info2_biography_mbid_similar`; `artist_info_user_bio_wins`; `artist_info_no_image_url_attributes`; `artist_info_count_capped_50`; `artist_info_unknown_70_missing_10`
  - `album_info_mbid_and_errors` (70 unknown, 10 missing)
  - `get_lyrics_matches_title_contains_and_artist`; `get_lyrics_empty_when_both_missing`; `get_lyrics_never_fetches` (replay `asked()` empty)
  - `lyrics_by_song_id_synced_lines_ms` (`[1:02.5]` → 62500, `[01:02.05]` → 62050, `[01:02.005]` → 62005); `lyrics_by_song_id_plain_lines`; `lyrics_by_song_id_none_row_empty_list`; `lyrics_by_song_id_missing_10`
  - `cover_art_artist_prefix_and_bare_id`; `cover_art_artist_without_image_70` (replaces `cover_art_artist_70`)
  - `extensions_include_song_lyrics`
- [ ] **Step 2: Run** `cargo test -p kima-subsonic`. Expected: fail.
- [ ] **Step 3: Implement.** Update the snapshots and read each changed `.snap` against spec §18.
- [ ] **Step 4: Run** with `INSTA_UPDATE=no`. Expected: pass, including `all_81_paths_answer` and the DB-failure test (move these endpoints out of its "may answer ok" allow-list where they now read tables).
- [ ] **Step 5: Commit** with message `feat(subsonic): artist and album info, lyrics, artist cover art`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 30: Frontend: saved secrets, env lock, features

**Files:**
- Modify: `frontend/features/settings/components/sections/{ArtworkSection.tsx, CacheSection.tsx}`, `frontend/features/settings/types.ts`, `frontend/lib/api.ts`
- Create: `frontend/features/settings/components/sections/__tests__/{ArtworkSection.test.tsx, CacheSection.test.tsx}`, `frontend/tests/e2e/kima-rs/enrichment.spec.ts`
- Test: the files above

**Divergences:** D-122: the M3a clauses only ("ArtworkSection shows saved secrets as 'Saved' with Remove; CacheSection shows the env lock (C16)"). Spec §4 lists D-122 under M3b, but these M3a edits must carry their row in the same commit; M3b adds the Match albums pages to the same row.

**Interfaces:**
- Consumes: `GET`/`POST /api/system-settings` (Task 5), `POST /system-settings/test-fanart` (Task 20), `GET /system/features` (Task 11).
- Produces (copy verbatim from spec §21.1):
  - `types.ts` gains `hasFanartApiKey: boolean`, `hasAcoustidApiKey: boolean`, `autoEnrichMetadataLocked: boolean`; `lib/api.ts`'s `getFeatures` type gains `fpcalc: boolean`.
  - ArtworkSection, for each secret: when `has…` is true, the password field's placeholder is `Saved — type to replace` and a `Remove` link posts `null` for that key; "Test connection" is enabled with an empty field.
  - CacheSection: the "Auto enrich metadata" toggle's description is `Look up MusicBrainz, AcoustID, artist info and lyrics online`; when `autoEnrichMetadataLocked`, the toggle is disabled and shows `Turned off on the server (KIMA_IDENTIFY=off)`.

- [ ] **Step 1: Failing unit tests:**
  - `shows Saved placeholder and Remove when a key is stored`; `Remove posts null`; `Test connection enabled with an empty field when a key is stored`
  - `toggle disabled with lock note when locked`; `toggle description text`
- [ ] **Step 2: Write the Playwright spec** `enrichment.spec.ts`: sign in as admin; Settings > Artwork: type an AcoustID key, save, reload, see `Saved — type to replace`, click `Remove`, reload, the placeholder is gone; Settings > Library Enrichment: the toggle is disabled with the lock note (the CI kima runs with `KIMA_IDENTIFY=off`).
- [ ] **Step 3: Implement.** No other layout change.
- [ ] **Step 4: Run** `npx tsc --noEmit`, `npm run test:unit`, `npm run build`, then the Playwright spec against a local kima started with `KIMA_IDENTIFY=off`. Expected: all pass.
- [ ] **Step 5: Commit** with message `feat(frontend): saved secrets, the identification lock and the fpcalc feature`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 31: `enrichment:progress` events

**Files:**
- Create: `crates/kima-ident/src/events.rs`, `crates/kima-ident/tests/events.rs`
- Modify: `crates/kima-ident/src/{ctx.rs, coordinator/mod.rs, coordinator/control.rs}`, `crates/kima-api/src/routes/enrichment.rs`
- Test: `crates/kima-ident/tests/events.rs`, `crates/kima-api/tests/events.rs`

**Divergences:** D-112 completed ("emitted by the coordinator").

**Interfaces:**
- Consumes: `EventBus` (M0), the coordinator and controls (Tasks 18–19).
- Produces: `kima_ident::events::ProgressEvents` (Clone): `new(bus: EventBus) -> Self`; `notify(&self, phase: Option<Phase>, status: Status)`. It publishes `Event { user_id: None, data: {"type":"enrichment:progress","phase":<"artists"|"tracks"|"fingerprint"|null>,"status":<status>} }` at most once a second; a call inside the window schedules one trailing event at the window's end carrying the latest values. `IdentCtx` gains `progress_events: ProgressEvents`. The coordinator notifies on count changes and state transitions; each control route notifies after its change.

- [ ] **Step 1: Failing tests:**
  - `throttled_to_one_per_second_with_trailing` (paused clock: 10 calls in 300 ms publish 2 events, the second with the last values).
  - `payload_shape_and_broadcast_to_all_users`
  - `pause_route_emits_event` (api events.rs: an SSE client receives `enrichment:progress` after `POST /enrichment/pause`).
  - `cycle_emits_on_count_change_only`
- [ ] **Step 2: Run** `cargo test -p kima-ident --test events && cargo test -p kima-api --test events`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** with message `feat(ident): enrichment:progress events, at most one a second`. Stage `docs/DIVERGENCES.md` with this task's rows.

### Task 32: M3a wrap-up

**Files:**
- Modify: `docs/DIVERGENCES.md` (verify only), `README.md`, `.env.example`, `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`

**Divergences:** none new; verifies D-106 to D-118, D-124 and the M3a part of D-122.

- [ ] **Step 1: Verify the divergence log.** Do not add rows here; Tasks 5–31 wrote them.
  - D-106 to D-118 and D-124 are present; each matches spec §23 in ID, area and both columns, with every multi-task row complete (D-106, D-107, D-108, D-109, D-110, D-112, D-113, D-114, D-115, D-116, D-117), status `open`.
  - D-122 holds the M3a clauses only.
  - The cross-reference notes exist on D-043, D-048 (Task 21), D-075, D-082, D-086 (Tasks 28–29).
  - Any row a task added after D-124 is sequential.
- [ ] **Step 2: README and `.env.example`.** Add the seven spec §22 variables to the configuration table; a short "Identification" section: what runs automatically, the `KIMA_IDENTIFY=off` switch and the settings toggle, the AcoustID key (none ships; enter one in Settings > Artwork), first-run time on large libraries and `MUSICBRAINZ_BASE_URL` for a local mirror (spec §26), and that nothing is written to music files in M3a.
- [ ] **Step 3: Full verification:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `SQLX_OFFLINE=true cargo build --workspace`
  - `KIMA_NET_DENY_REAL=1 cargo test --workspace`, with ffmpeg present
  - the two C-locale runs of Task 1 Step 5
  - in `frontend/`: `npx tsc --noEmit`, `npm run test:unit`, `npm run build`
  - `docker build -t kima-rs:m3a .`, then run the image with `KIMA_IDENTIFY=off` and the fixture library from `tools/make-fixture-library.sh`, and run `npx playwright test -c playwright.kima-rs.config.ts` against it
  - push the branch and check the GitHub CI run

  Expected: all green.
- [ ] **Step 4: Roadmap.** In the M3 row, set the plan cell to `2026-10-19-m3a-identification.md` ✅ (M3a), with M3b (autotagger, organizer rework) still to plan. Remove the carried-forward items M3a resolved: the heartbeat, the M2 leftovers, minors 3 and 4. Keep minors 2 and 5–8 and the autotagger for M3b.
- [ ] **Step 5: Commit** with message `docs: M3a wrap-up`.
