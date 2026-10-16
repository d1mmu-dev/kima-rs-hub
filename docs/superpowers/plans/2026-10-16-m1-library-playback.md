# M1 Library & Playback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** kima scans and watches a (messy) music folder, presents it as Artist → Year – Album → songs, and lets users browse, search, stream (with transcodes), build playlists and resume playback from the forked frontend. An admin can preview and apply a tidy on-disk layout.

**Architecture:** A new domain crate `kima-library` holds tag reading, field resolution, grouping, fingerprinting, the reconcile/single-file scanner, the watcher, artwork, transcoding and the organizer. None of it depends on axum. `kima-db` gains one query module per domain (`library`, `plays`, `playlists`, `playback`, `organizer`, `settings`). `kima-api` gains thin route modules. `kima-server` wires the job worker, the watcher and the shared shutdown token into `main`. The forked frontend gets feature gating, two stubs, and two new admin pages.

**Tech Stack:** Rust 2024, axum 0.8, sqlx 0.8, tokio. New crates: `lofty` (tags), `notify` + `notify-debouncer-full` (watching), `image` (artwork resize), `sha2` (fingerprints and checksums), `unicode-normalization`, `walkdir`, `tokio-cron-scheduler` is NOT used (no schedules). ffmpeg runs as a subprocess. Postgres 17 + pgvector. Next.js 16 static export. Playwright.

**Spec:** `docs/superpowers/specs/2026-10-16-m1-library-playback-design.md` (binding). Parent spec: `docs/superpowers/specs/2026-10-16-kima-rust-design.md`. Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`. Divergence log: `docs/DIVERGENCES.md` (next free ID: D-032).

**Upstream reference:** kima-hub @ `12faf1cd34ff53054221818b0ccde58b8e9b0020`, cloned read-only at `/tmp/kima-hub`. If it is missing, re-clone it: `git clone https://github.com/Chevron7Locked/kima-hub /tmp/kima-hub`, then `git -C /tmp/kima-hub checkout 12faf1cd34ff53054221818b0ccde58b8e9b0020`. "Shape as upstream X" means: read that upstream file and return the same JSON keys and types that the frontend reads (`frontend/lib/api.ts` and `frontend/features/*/types.ts`). Fields the frontend never reads may be omitted. Each omitted field the frontend *does* read is a bug.

## Global Constraints

- Everything in M0's Global Constraints still holds: GPL-3.0-only, edition 2024, `rust-version = "1.85"`, versions pinned in the workspace `Cargo.toml`, camelCase JSON, errors `{"error": "<message>"}`, UUIDv7 ids, the DB never mocked (`#[sqlx::test(migrator = "kima_db::MIGRATOR")]`), `cargo fmt` + `cargo clippy --workspace --all-targets -- -D warnings` clean before each Rust commit, and `cargo sqlx prepare --workspace -- --all-targets` with `.sqlx/` committed after any query-macro change.
- Domain crates (`kima-core`, `kima-db`, `kima-jobs`, `kima-library`) never depend on axum.
- **Commits:** author and committer are `dimmu <3417783+d1mmu-dev@users.noreply.github.com>` (already the repo's local git config). **No `Co-Authored-By` or any AI attribution line** in commit messages. Conventional style (`feat:`, `fix:`, `test:`, `docs:`, `chore:`).
- Every difference from upstream gets a `docs/DIVERGENCES.md` row in the same commit (next free ID, status `open` unless the task says otherwise).
- New env vars, all optional:
  - `KIMA_WATCH_MODE`: `auto` (default), `poll` or `off`.
  - `KIMA_WATCH_POLL_SECS`: default `60`.
  - `KIMA_ORGANIZE_TARGET`: unset means copy mode is unavailable.
  - `KIMA_TRANSCODE_CACHE_MB`: default `2048`.
- Missing-file grace period: 7 days. Debounce: 2 s. Fingerprint: SHA-256 of (file size as 8 LE bytes ‖ first 64 KiB ‖ last 64 KiB).
- Stream qualities, as upstream: `original`, `high` = mp3 320 kbps, `medium` = mp3 192 kbps, `low` = mp3 128 kbps (libmp3lame). An invalid value returns 400 `{"error":"quality must be original, high, medium or low"}`.
- Organizer path pattern (spec §5.1):
  - `<Album artist>/<Year> - <Album>/<NN> - <Title>.<ext>`, with `NN` zero-padded to 2.
  - Multi-disc albums: `<D>-<NN> - <Title>`.
  - No year: `<Album artist>/<Album>/…`.
  - Compilations: `Various Artists/<Year> - <Album>/<NN> - <Track artist> - <Title>.<ext>`.
  - Collisions get ` (2)`, ` (3)` and so on.
- SSE progress event: `{"type":"library:progress","job":"scan"|"organize","done":n,"total":m}`, broadcast to admins (`user_id: None` is acceptable in M1, since only admins open those pages).
- One job queue named `library` with concurrency 1. It runs reconciles, single-file updates and organizer runs. They never overlap.

## Review Focus

1. **A file arriving while it is still being copied.** A 500 MB FLAC copied over SMB takes minutes. The watcher must not index a truncated file, and if it does read one early, the next settled event must fix the row. The test lives in Task 9.
2. **A user's library disk is unplugged and plugged back in.** Every track goes "missing" at once. Nothing may be purged before 7 days, and plays and playlists must still be attached after re-plugging. The test lives in Task 8.
3. **Seeking in the browser.** Chrome sends `Range: bytes=N-` and expects a 206 with the correct `Content-Range`. Suffix ranges (`bytes=-500`) and out-of-range requests (416) must behave. While a transcode is still being written, the response is a 200 without `Accept-Ranges`. The test lives in Task 13.
4. **Organizer crash mid-apply.** kima is killed after 40 % of a move. Restarting must resume from the journal without double-moving, overwriting or losing a file, and undo must restore every original path. The test lives in Task 17.
5. **Hostile and odd names.** Artist `AC/DC`, album `..`, a title with `:`, `?`, `*`, a 300-character title, the Windows reserved name `CON`, and NFD-vs-NFC duplicates must all map to safe, distinct, deterministic target paths, and no path may escape the target root. The test lives in Task 16.

---

### Task 1: Config, job worker wiring, LISTEN retry

**Files:**
- Modify: `crates/kima-core/src/config.rs`, `crates/kima-server/src/main.rs`, `crates/kima-server/Cargo.toml`, `crates/kima-jobs/src/worker.rs`
- Create: `crates/kima-server/src/jobs.rs`
- Test: `crates/kima-core/src/config.rs` (unit), `crates/kima-jobs/tests/worker.rs`

**Interfaces:**
- Consumes: `kima_jobs::worker::{spawn_worker, WorkerConfig, Handler}`, `AppState::with_shutdown`.
- Produces:
  - `Config` gains these fields:
    - `watch_mode: WatchMode` (an enum with `Auto`, `Poll` and `Off`)
    - `watch_poll_secs: u64`
    - `organize_target: Option<PathBuf>`
    - `transcode_cache_mb: u64`
  - `kima_server::jobs::Registry` maps a queue name to its `Handler`.
  - `kima_server::jobs::spawn_all(pool, registry, shutdown) -> Vec<JoinHandle<()>>`
  - `kima_server::jobs::drain(handles, timeout: Duration)`, where the timeout is 10 s.

- [ ] **Step 1: Failing config tests.** In config.rs, add these tests:
  - `watch_and_organize_defaults`: `watch_mode == WatchMode::Auto`, `watch_poll_secs == 60`, `organize_target == None`, `transcode_cache_mb == 2048`.
  - `watch_mode_parses`: `"poll"` gives `Poll` and `"off"` gives `Off`.
  - `bad_watch_mode_is_invalid`: `"sometimes"` gives `ConfigError::Invalid { name: "KIMA_WATCH_MODE", .. }`.
  - `poll_secs_must_be_positive`: `"0"` gives `Invalid`.
- [ ] **Step 2: Run** `cargo test -p kima-core config`. Expected: compile failure on the missing fields.
- [ ] **Step 3: Implement the fields and parsing.** Update the M0 test helper `config()` in `crates/kima-server/tests/app.rs` with the new fields.
- [ ] **Step 4: Failing worker test** `listen_is_retried_after_failure` in `crates/kima-jobs/tests/worker.rs`. Start a worker whose first LISTEN attempt fails, by adding an injectable `listen` retry interval: `WorkerConfig { listen_retry: Duration }`, defaulting to 30 s, set to 200 ms in the test. Close the listener connection by terminating it with `pg_terminate_backend` on the LISTEN backend. Enqueue a job, wait 1 s, and assert the job is done **without** waiting a poll interval. Use a poll interval of 30 s in the test, so only NOTIFY can wake it.
- [ ] **Step 5: Implement.** When `listener` is `None`, or `recv` returned an error, try `listen()` again at most once per `listen_retry`, inside the loop's `select!` timer arm.
- [ ] **Step 6: Wire the worker into main.**
  - Build one `CancellationToken`.
  - Pass it to `AppState::with_shutdown` and to `spawn_all`.
  - On shutdown, cancel it, then call `drain(handles, 10 s)`. `drain` logs a warn per worker still running at the deadline, then returns.
  - The registry is empty until Task 8; `spawn_all` with no entries spawns nothing.
- [ ] **Step 7: Run** `cargo test --workspace`. Expected: all pass.
- [ ] **Step 8: Commit** with message `feat: watch/organize config, job worker wired into main, LISTEN retry`.

### Task 2: Stream tokens, SSE revocation, lag resync

**Files:**
- Modify: `crates/kima-api/src/auth.rs`, `crates/kima-api/src/routes/auth.rs`, `crates/kima-api/src/routes/events.rs`, `crates/kima-api/src/events.rs`, `crates/kima-core/src/events.rs`
- Test: `crates/kima-api/tests/foundation.rs`, `crates/kima-api/tests/events.rs`

**Interfaces:**
- Produces:
  - `kima_api::auth::token_from_request(parts: &Parts) -> Option<String>`. It reads the `Authorization: Bearer` header first, then `?token=` **only** when the path ends in `/stream` or starts with `/api/library/cover-art`.
  - `POST /api/auth/stream-ticket` (auth) returns `{token}` with a 4 h stream JWT, shaped as upstream `routes/auth.rs:547`.
  - `kima_core::events::Event` gains `Event::revoke(user: Uuid)` (data `{"type":"session:revoked"}`), recognised by `Event::is_revocation(&self) -> bool`.
  - `kima_api::revoke_sessions(state, user)`: publishes the revoke event. Every later task that bumps `token_version` calls it.

- [ ] **Step 1: Failing tests:**
  - `stream_token_in_query_works_on_stream_paths`: `?token=<stream jwt>` on the probe `/media/stream` returns 200.
  - `query_token_rejected_off_stream_paths`: `?token=` on `/whoami` returns 401.
  - `access_token_in_query_works_on_stream_paths`: returns 200. Upstream accepts access tokens there too.
  - `stream_ticket_endpoint`: POST with auth returns `{token}`, and `JwtKeys::decode` of it has `kind() == Stream`.
  - `revoked_user_streams_end`: open an SSE stream, call `revoke_sessions`, and assert the body ends within 2 s. Another user's stream stays open.
  - `lagged_stream_gets_resync`: use a bus with capacity 2, publish 10 events without reading, then read. One frame is `data: {"type":"resync"}`.
- [ ] **Step 2: Run** `cargo test -p kima-api`. Expected: the new tests fail.
- [ ] **Step 3: Implement.** The `AuthUser` extractor uses `token_from_request`. The SSE stream ends via `take_while` when it sees a revocation for its user. `BroadcastStreamRecvError::Lagged(_)` maps to a `{"type":"resync"}` event.
- [ ] **Step 4: Run tests.** Expected: pass.
- [ ] **Step 5: Divergences.** Log D-032: upstream SSE does not end on revocation; kima-rs ends a revoked user's open streams (`accepted`). Log D-033: SSE sends `{"type":"resync"}` after a broadcast lag (`open`).
- [ ] **Step 6: Commit** with message `feat(api): stream tokens in query on media paths, SSE revocation and lag resync`.

### Task 3: User management and registration toggle

**Files:**
- Modify: `crates/kima-db/src/users.rs`, `crates/kima-api/src/routes/auth.rs`, `crates/kima-api/src/routes/mod.rs`
- Create: `crates/kima-db/src/settings.rs`, `crates/kima-db/migrations/0003_system_settings.sql`, `crates/kima-api/src/routes/system_settings.rs`
- Test: `crates/kima-db/tests/users.rs`, `crates/kima-api/tests/users.rs`

**Interfaces:**
- Produces:
  - `users::{list(pool) -> Vec<User>, create_by_admin(pool, id, username, hash, role) -> Result<User, Taken>, delete(pool, id) -> bool, set_password(pool, id, hash) -> i32 /* new token_version */, bump_token_version(pool, id) -> i32}`
  - `settings::{get(pool) -> SystemSettings, update(pool, patch: SettingsPatch) -> SystemSettings}`. `SystemSettings { registration_open: bool, library_root: Option<String> }`. Migration 0003 adds `library_root text` to `system_settings`.
  - HTTP endpoints, shaped as upstream `routes/auth.ts:215-385`:
    - `POST /api/auth/change-password {currentPassword,newPassword}`
    - `GET /api/auth/users` (admin)
    - `POST /api/auth/create-user {username,password,role}` (admin)
    - `DELETE /api/auth/users/:id` (admin; refuses to delete yourself, returning 400, and refuses to delete the last admin, returning 400)
  - Also `GET`/`POST /api/system-settings` (admin), with the M1 subset `{registrationOpen}`, shaped as upstream `routes/systemSettings.ts:115-160`.

- [ ] **Step 1: Failing DB tests:**
  - `set_password_bumps_token_version`
  - `last_admin_cannot_be_deleted` (asserted at the API layer, see below)
  - `create_by_admin_rejects_duplicate_ignoring_case`
- [ ] **Step 2: Failing API tests:**
  - `change_password_requires_current`: a wrong current password returns 401 `{"error":"Current password is incorrect"}`.
  - `change_password_revokes_old_tokens`: the old access token returns 401 afterwards, and the response carries a fresh `token` (as upstream) that works.
  - `admin_creates_and_lists_users`
  - `non_admin_gets_403` on all four admin routes.
  - `cannot_delete_self`
  - `cannot_delete_last_admin`
  - `registration_toggle_opens_signup`: POST `{registrationOpen:true}`, then a second register returns 200.
- [ ] **Step 3: Run** the tests. Expected: they fail with 404 or a compile error.
- [ ] **Step 4: Implement.** Every password change and deletion calls `revoke_sessions` (Task 2). argon2 work goes through `state.password_permit()`, with the permit moved into `spawn_blocking`.
- [ ] **Step 5: Run tests, then `cargo sqlx prepare`.**
- [ ] **Step 6: Commit** with message `feat: user management, change password, registration toggle`. Update D-024's kima-rs column: user management is now present.

### Task 4: Library schema

**Files:**
- Create: `crates/kima-db/migrations/0004_library.sql`, `crates/kima-db/src/library.rs`
- Test: `crates/kima-db/tests/library.rs`

**Interfaces:**
- Produces these tables:
  - `artists(id uuid pk, name text, name_key text unique, created_at, search tsvector)`
  - `albums(id uuid pk, artist_id fk, title text, title_key text, year int null, is_compilation bool, cover_path text null, created_at, search tsvector, unique(artist_id, title_key, coalesce(year, -1)))`
  - `tracks(id uuid pk, album_id fk, artist_name text /* track artist */, title text, disc int, track_no int, duration_secs int, codec text, bitrate int null, path text unique, size bigint, mtime timestamptz, fingerprint bytea, guessed jsonb /* which fields came from fallbacks */, missing_since timestamptz null, created_at, updated_at, search tsvector)`
  - Triggers keep each `search` column current. There are GIN indexes on all three, and an index on `tracks(fingerprint) where missing_since is not null`.
- Produces in `kima_db::library`:
  - `ResolvedTrack`: the struct Task 6 fills. Fields: `album_artist`, `album_artist_key`, `track_artist`, `album`, `album_key`, `year: Option<i32>`, `is_compilation`, `disc`, `track_no`, `title`, `duration_secs`, `codec`, `bitrate`, `path`, `size`, `mtime`, `fingerprint: [u8; 32]`, `guessed: Guessed`.
  - `upsert_track(tx, &ResolvedTrack) -> Uuid`. It finds or creates the artist by `name_key` (keeping the better-capitalised display name, as spec §4.3), finds or creates the album by `(artist, title_key, year)`, then upserts the track by path.
  - `find_by_path(pool, &str) -> Option<TrackRow>`
  - `mark_missing(pool, paths: &[String]) -> u64`
  - `repoint_by_fingerprint(pool, fp, new_path) -> Option<Uuid>`, which only matches missing tracks.
  - `purge_missing(pool, older_than: Duration) -> u64`
  - `prune_empty(pool)`, which deletes albums with no tracks and then artists with no albums.

- [ ] **Step 1: Failing tests:**
  - `same_artist_case_insensitive_one_row`: "radiohead" then "Radiohead" gives 1 artist named "Radiohead".
  - `album_identity_includes_year`: (A, "Hits", 1999) and (A, "Hits", 2009) give 2 albums.
  - `album_without_year_merges`
  - `upsert_by_path_updates_in_place`: same path, new title, same id.
  - `repoint_only_matches_missing`
  - `purge_respects_grace`: missing for 6 days is kept; missing for 8 days is purged.
  - `search_vector_updates`
- [ ] **Step 2: Run.** Expected: fail.
- [ ] **Step 3: Implement the migration and module.** Use `unaccent`-free `simple` config tsvectors, and weight the name or title as A.
- [ ] **Step 4: Run, then `cargo sqlx prepare`.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(db): artists, albums, tracks with search vectors and missing-file tracking`.

### Task 5: `kima-library` crate: tag reading and fingerprints

**Files:**
- Create: `crates/kima-library/Cargo.toml`, `src/lib.rs`, `src/tags.rs`, `src/fingerprint.rs`, `tests/fixtures.rs` (a test helper that generates audio with ffmpeg)
- Modify: root `Cargo.toml` (adds the workspace dep and the `lofty`, `sha2`, `walkdir`, `unicode-normalization` versions)
- Test: `crates/kima-library/tests/tags.rs`

**Interfaces:**
- Produces:
  - `tags::read(path: &Path) -> Result<RawTags, TagError>`, where `RawTags { album_artist, artist, album, title, year: Option<i32>, date: Option<String>, disc, track, compilation: bool, duration_secs, codec, bitrate }`. All the string fields are `Option<String>`, trimmed, with empty values mapped to None.
  - `tags::is_audio(path) -> bool`, by extension: mp3, flac, ogg, opus, m4a, aac, wav, wma, aiff, ape, wv.
  - `fingerprint::of(path) -> io::Result<[u8; 32]>`, using the Global Constraints formula.
  - Test helper `fixtures::make_audio(dir, rel_path, Tags) -> PathBuf`. It runs ffmpeg `-f lavfi -i sine=duration=2` and writes metadata. Tests `return` early, with a printed note, if ffmpeg is absent. CI installs it (Task 19).

- [ ] **Step 1: Failing tests:**
  - `reads_mp3_flac_ogg_m4a_tags`: one fixture per format with all tags. Assert each field.
  - `untagged_file_gives_nones`
  - `year_from_date_tag`: `date=1997-05-21` gives `Some(1997)`.
  - `fingerprint_stable_and_size_sensitive`: the same file gives the same fingerprint. Appending one byte changes it. A copy elsewhere gives the same fingerprint.
  - `fingerprint_small_file`: a file under 64 KiB works.
- [ ] **Step 2: Run.** Expected: fail.
- [ ] **Step 3: Implement** with lofty's `read_from_path`, `Accessor` and `ItemKey::AlbumArtist`/`FlagCompilation`.
- [ ] **Step 4: Run.** Expected: pass.
- [ ] **Step 5: Commit** with message `feat(library): tag reading and file fingerprints`.

### Task 6: Field resolution and folder fallback (pure)

**Files:**
- Create: `crates/kima-library/src/resolve.rs`, `crates/kima-library/src/names.rs`
- Test: `crates/kima-library/src/resolve.rs` (unit), `crates/kima-library/src/names.rs` (unit)

**Interfaces:**
- Consumes: `RawTags`.
- Produces:
  - `resolve::resolve(raw: &RawTags, rel_path: &Path) -> Resolved`. It follows the spec §4.1 table exactly, and `Guessed { artist_from_path, album_from_path, year_from_path, track_from_name, title_from_name, unknown_artist, unknown_album }` records every fallback used.
  - `names::key(s) -> String`: NFC normalise, trim, collapse whitespace, lowercase.
  - `names::canonical_various(s) -> Option<&'static str>`: maps "Various", "Various Artists", "VA", "V.A." and "Varios Artistas" (case-insensitive) to "Various Artists".
  - `names::better_caps(a, b) -> &str`: as upstream `musicScanner.ts:786`.
  - The `ResolvedTrack` assembly lives in Task 8.

- [ ] **Step 1: Port the upstream path-parsing tests.** Find the tests for `extractArtistFromRelativePath`/`extractAlbumFromRelativePath`: grep `/tmp/kima-hub/backend/src` for those names, and look under `__tests__`. Translate each case into a Rust `#[test]` in resolve.rs, with names prefixed `upstream_`.
- [ ] **Step 2: Add the spec cases:**
  - `albumartist_wins_over_artist`
  - `feat_track_stays_on_album_artist`
  - `band_with_ampersand_not_split`: "Simon & Garfunkel" stays one artist.
  - `year_from_folder_prefix`: `Artist/1997 - OK Computer/01.mp3` gives year 1997 and album "OK Computer".
  - `year_from_folder_suffix`: `OK Computer (1997)`.
  - `track_from_filename`: `03 - Title.mp3` and `1-03 Title.flac` give (disc 1, track 3).
  - `title_from_filename_strips_number`
  - `unknown_when_nothing`: gives "Unknown Artist" and "Unknown Album", with both flags set.
  - `compilation_flag_and_various_names_group_as_various`
- [ ] **Step 3: Run** `cargo test -p kima-library resolve names`. Expected: fail.
- [ ] **Step 4: Implement.**
- [ ] **Step 5: Run.** Expected: pass.
- [ ] **Step 6: Commit** with message `feat(library): field resolution with folder fallback (ported upstream cases)`.

### Task 7: Artwork

**Files:**
- Create: `crates/kima-library/src/artwork.rs`, `crates/kima-api/src/routes/cover_art.rs`
- Test: `crates/kima-library/tests/artwork.rs`, `crates/kima-api/tests/cover_art.rs`

**Interfaces:**
- Produces:
  - `artwork::find_cover(track_path: &Path) -> Option<CoverSource>`. It tries embedded art first (lofty `pictures()`, preferring CoverFront), then `cover.*`, `folder.*`, `front.*` (jpg, jpeg, png, webp, case-insensitive) in the track's folder.
  - `artwork::cached(cache_dir, album_id, size: Option<u32>) -> Result<PathBuf>`. It resizes with `image`, keeping the aspect ratio and fitting inside `size` × `size`. It writes JPEG at quality 85 under `<data_dir>/artwork/<album_id>/<size|orig>.jpg`.
  - Endpoint `GET /api/library/cover-art/:id?size=N`. The id is an album id; the response is an image with `Cache-Control: public, max-age=86400`, or 404. The query shape follows upstream `routes/library/coverArt.ts:144`. `?token=` is accepted (Task 2).

- [ ] **Step 1: Failing tests:**
  - `embedded_beats_folder_image`
  - `folder_image_case_insensitive`
  - `no_art_is_none`
  - `resize_fits_box`: 1000×500 at size 300 gives 300×150.
  - `cached_second_call_does_not_reencode`: compare mtimes.
  - The API test `cover_art_404_for_unknown_album`.
- [ ] **Step 2: Run, implement, run.**
- [ ] **Step 3: Commit** with message `feat(library): album artwork lookup, resize and cache`.

### Task 8: Scanner: reconcile and single-file update jobs

**Files:**
- Create: `crates/kima-library/src/scan.rs`, `crates/kima-library/src/jobs.rs`
- Modify: `crates/kima-server/src/jobs.rs` (registers the `library` queue), `crates/kima-server/src/main.rs` (enqueues a reconcile on startup)
- Test: `crates/kima-library/tests/scan.rs`

**Interfaces:**
- Consumes: `tags::read`, `resolve::resolve`, `fingerprint::of`, `kima_db::library::*`, `kima_jobs::queue::enqueue`, `EventBus`.
- Produces:
  - `scan::LibraryCtx { pool, root: PathBuf, events: EventBus, data_dir: PathBuf }`. The `root` is `system_settings.library_root` if set, else `Config.music_path`.
  - `scan::reconcile(ctx) -> ScanReport { added, updated, repointed, missing, purged, failed: Vec<(String, String)> }`.
  - `scan::update_path(ctx, rel_path) -> PathOutcome`. Its variants are `Added`, `Updated`, `Unchanged`, `Repointed`, `MarkedMissing`, `Ignored` and `Failed`.
  - Job payloads on queue `library`:
    - `{"kind":"reconcile"}`
    - `{"kind":"path","path":"<rel>"}`
    - `{"kind":"organize", …}` (Task 17)
  - `jobs::handler(ctx) -> Handler` dispatches on `kind`.
  - Progress events follow the Global Constraints shape, at most 4 per second.

- [ ] **Step 1: Failing tests.** Build fixture libraries with `make_audio`.
  - `reconcile_indexes_messy_tree`: a tagged album, an untagged `Artist/1999 - Album/01 - x.mp3`, a compilation, and a non-audio file. Assert the artist, album and track counts and the guessed flags.
  - `unchanged_files_are_skipped_on_rescan`: the second reconcile reports `updated == 0`. Add a counter or a test hook, so the test can assert `tags::read` was not called.
  - `edited_tags_update_row`
  - `deleted_file_marked_missing_not_deleted`
  - `moved_file_is_repointed_and_keeps_plays`: insert a play row, move the file, reconcile. The same track id is returned, and the play is still attached.
  - **Review Focus 2,** `unplug_and_replug_keeps_history`: make the whole root unreadable/empty, reconcile (everything missing), wait out "6 days" by setting `missing_since = now() - 6 days` in SQL, purge (keeps everything), restore the files, reconcile. Same ids, and the plays are intact.
  - `corrupt_file_is_reported_not_fatal`
  - `update_path_single_file`: add, change, delete one file through `update_path`.
- [ ] **Step 2: Run, implement, run.** Reconcile walks with `walkdir`. It compares size and mtime against `find_by_path` and calls `upsert_track` per changed file, in its own transaction. Afterwards it marks unseen paths missing, purges past the grace period, and runs `prune_empty`.
- [ ] **Step 3: Wire it in.** Register the `library` queue handler (concurrency 1). Enqueue `{"kind":"reconcile"}` on startup.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare`.
- [ ] **Step 5: Commit** with message `feat(library): reconcile and single-file update jobs`.

### Task 9: Watcher

**Files:**
- Create: `crates/kima-library/src/watch.rs`
- Modify: `crates/kima-server/src/main.rs`
- Test: `crates/kima-library/tests/watch.rs`

**Interfaces:**
- Produces:
  - `watch::spawn(ctx, mode: WatchMode, poll_secs, ignore: IgnoreSet, shutdown) -> WatchHandle`.
    - It uses `notify_debouncer_full` with a 2 s timeout. `Auto` uses `RecommendedWatcher` and `Poll` uses `PollWatcher` at `poll_secs`.
    - Each settled path enqueues `{"kind":"path"}`. A folder event expands to its files.
    - A watcher error or event-queue overflow enqueues `{"kind":"reconcile"}`.
  - `WatchHandle::status() -> WatchStatus { mode, active: bool, last_error: Option<String> }`, exposed through `GET /api/library/watch-status` (admin). When the inotify watch limit is hit, `last_error` names `fs.inotify.max_user_watches`.
  - `IgnoreSet`: a shared `Arc<Mutex<HashSet<PathBuf>>>`. The organizer (Task 17) adds and removes paths in it.

- [ ] **Step 1: Failing tests.** Use real files in a tempdir with a 200 ms debounce override for the test.
  - `new_file_is_indexed`
  - `modified_file_updates`
  - `deleted_file_marked_missing`
  - `rename_keeps_id`
  - `ignored_paths_not_enqueued`
  - **Review Focus 1,** `file_written_slowly_indexed_once_complete`: write a file in 5 chunks with 300 ms pauses (debounce 500 ms). Exactly one `Added` and the final duration is correct. Or, if a partial read happened, a later `Updated` with the correct size.
  - `poll_mode_sees_changes`: `poll_secs = 1`.
- [ ] **Step 2: Run, implement, run.**
- [ ] **Step 3: Wire it in.** In main, spawn the watcher unless the mode is `Off`, stopped by the shutdown token.
- [ ] **Step 4: Commit** with message `feat(library): watch the library and update changed files only`.

### Task 10: Library browse API

**Files:**
- Create:
  - `crates/kima-db/src/browse.rs`
  - `crates/kima-api/src/routes/library/mod.rs`
  - `crates/kima-api/src/routes/library/artists.rs`
  - `crates/kima-api/src/routes/library/albums.rs`
  - `crates/kima-api/src/routes/library/tracks.rs`
  - `crates/kima-api/src/routes/library/scan.rs`
- Test: `crates/kima-api/tests/library.rs`, plus a shared fixture `crates/kima-api/tests/common/library.rs`. The fixture seeds rows through `kima_db::library::upsert_track`, with no audio needed.

**Interfaces:**
- Produces endpoints, each shaped as the named upstream handler:
  - `GET /api/library/artists` (`library/artists.ts:168`: paging, `query`, sort)
  - `GET /api/library/artists/:id` (`:399`, with albums ordered by year, then title)
  - `GET /api/library/albums` (`albums.ts:25`)
  - `GET /api/library/albums/:id` (`:110`, tracks ordered by disc, then track number)
  - `GET /api/library/tracks` (`tracks.ts:264`)
  - `GET /api/library/tracks/:id` (`:514`)
  - `GET /api/library/tracks/shuffle` (`:325`)
  - `GET /api/library/recently-added` (`:201`)
  - `POST /api/library/scan` (admin): enqueues a reconcile and returns `{jobId}`.
  - `GET /api/library/scan/status/:jobId` (`scan.ts:46`)
- Missing tracks are excluded from every listing.
- Paging uses upstream's query names (`library/paging.ts`).
- `coverUrl`/`coverArt` fields point to `/api/library/cover-art/<albumId>`.

- [ ] **Step 1: Failing tests,** one per endpoint, asserting the keys the frontend reads. Grep `frontend/features/{library,artist,album}/types.ts` and use those field names in the assertions. Plus:
  - `artist_page_albums_sorted_by_year`
  - `missing_tracks_hidden`
  - `paging_limits_and_offsets`
  - `scan_requires_admin`
  - `unknown_id_404`
- [ ] **Step 2: Run, implement, run, then `cargo sqlx prepare`.**
- [ ] **Step 3: Commit** with message `feat(api): library browse endpoints`.

### Task 11: Search

**Files:**
- Create: `crates/kima-db/src/search.rs`, `crates/kima-api/src/routes/search.rs`
- Test: `crates/kima-api/tests/search.rs`

**Interfaces:**
- Produces `GET /api/search?q=&type=&limit=`, shaped as upstream `routes/search.ts:63`, library part only. The query is a `websearch_to_tsquery('simple', q)` prefix match: append `:*` to the last term. Results are ranked by `ts_rank`.
- `/search/discover*` stays unrouted. Its UI is gated in Task 18.

- [ ] **Step 1: Failing tests:**
  - `finds_artist_album_track`
  - `prefix_matches_partial_word`: "radioh" finds "Radiohead".
  - `case_and_accents`: "beyonce" finds "Beyoncé" via the `unaccent` extension. If the extension can't be created, log a divergence instead.
  - `missing_tracks_excluded`
  - `empty_query_400`
- [ ] **Step 2: Run, implement, run.**
- [ ] **Step 3: Commit** with message `feat: library search`.

### Task 12: Transcoding service

**Files:**
- Create: `crates/kima-library/src/transcode.rs`
- Test: `crates/kima-library/tests/transcode.rs`

**Interfaces:**
- Produces:
  - `transcode::Quality`, with `Original`, `High`, `Medium` and `Low`. `Quality::parse(&str) -> Option<Quality>` and `Quality::bitrate_kbps() -> Option<u32>`.
  - `Transcoder::new(cache_dir, max_bytes) -> Transcoder`.
  - `Transcoder::get(track_id, source: &Path, q) -> Result<TranscodeOutput>`, where `TranscodeOutput` is either `Cached(PathBuf)` or `InProgress { path: PathBuf, done: watch::Receiver<bool> }`.
    - The cache key is `(track_id, quality, source mtime)`.
    - ffmpeg arguments are `-i <src> -vn -map 0:a:0 -c:a libmp3lame -b:a <N>k -f mp3 <tmp>`. The output is written to `<key>.part` and renamed on success.
    - Concurrent requests for the same key share one ffmpeg process.
    - LRU eviction keeps the cache under `max_bytes`.
  - `Transcoder::ffmpeg_available() -> bool`.

- [ ] **Step 1: Failing tests:**
  - `transcode_produces_mp3_at_bitrate`: probe the output with lofty, bitrate ≈ 192.
  - `second_request_hits_cache`
  - `concurrent_requests_share_one_process`: a counter hook.
  - `cache_evicts_lru_over_cap`
  - `source_change_invalidates`
  - `failed_ffmpeg_cleans_part_file`
- [ ] **Step 2: Run, implement, run.**
- [ ] **Step 3: Commit** with message `feat(library): cached mp3 transcoding via ffmpeg`.

### Task 13: Streaming endpoint

**Files:**
- Create: `crates/kima-api/src/routes/library/stream.rs`, `crates/kima-api/src/range.rs`
- Modify: `crates/kima-api/src/state.rs` (adds `transcoder: Arc<Transcoder>` and `library_root: Arc<RwLock<PathBuf>>`)
- Test: `crates/kima-api/tests/stream.rs`, plus unit tests in `range.rs`

**Interfaces:**
- Produces:
  - `range::parse(header: &str, len: u64) -> RangeSpec`. `RangeSpec` is `Full`, `Partial { start, end }` or `Unsatisfiable`. It handles single ranges only; for multi-range requests it returns `Full`.
  - `GET /api/library/tracks/:id/stream?quality=`, with upstream semantics (`streaming.ts:53`). With no quality given, it uses the user's setting if upstream has one, else `original`.
    - Original and cached files support ranges: 206 with `Content-Range: bytes s-e/len` and `Accept-Ranges: bytes`. A satisfiable request gets 206, an unsatisfiable one gets 416 with `Content-Range: bytes */len`.
    - `InProgress` streams the growing file as 200 with no `Accept-Ranges`, following the writer until `done`.
    - `Content-Type` follows the codec map (upstream `audioStreaming.ts:451`).
    - Missing ffmpeg for a non-original quality falls back to the original, as upstream does.

- [ ] **Step 1: Failing range unit tests:**
  - `full_when_no_header`
  - `open_ended`: `bytes=100-`
  - `closed`: `bytes=0-99`
  - `suffix`: `bytes=-500`
  - `suffix_larger_than_file`: gives the whole file as `Partial(0, len-1)`.
  - `start_beyond_end_unsatisfiable`
  - `garbage_is_full`
- [ ] **Step 2: Failing HTTP tests. Review Focus 3.**
  - `range_206_headers`
  - `suffix_range`
  - `out_of_range_416`
  - `quality_invalid_400`, with the exact message from Global Constraints.
  - `query_token_auth_works`
  - `missing_track_404`
  - `transcode_first_play_200_no_ranges_then_cached_206`
- [ ] **Step 3: Run, implement, run.**
- [ ] **Step 4: Commit** with message `feat(api): streaming with range requests and transcodes`.

### Task 14: Plays, recently listened, playback state

**Files:**
- Create:
  - `crates/kima-db/migrations/0005_plays_playback.sql`
  - `crates/kima-db/src/plays.rs`
  - `crates/kima-db/src/playback.rs`
  - `crates/kima-api/src/routes/plays.rs`
  - `crates/kima-api/src/routes/playback_state.rs`
- Test: `crates/kima-api/tests/plays.rs`, `crates/kima-api/tests/playback.rs`

**Interfaces:**
- Produces:
  - `plays(id, user_id, track_id fk on delete cascade, played_at, heard bool)`, indexed as upstream's `Play` model.
  - `playback_state(user_id pk, playback_type, track_id, queue jsonb, current_index, is_shuffle, position, changed_by, updated_at)`.
  - `POST /api/plays` and `GET /api/plays`, as `routes/plays.ts`. `heard` follows `playRecording.ts`: a track over 30 s that was heard for min(half its length, 240 s), with dedup as `dedupWindowMs`. Port those two functions and their tests.
  - `GET /api/library/recently-listened` (`tracks.ts:28`).
  - `GET`, `POST` and `DELETE /api/playback-state`, plus `GET /api/playback-state/limit` and `POST /api/playback-state/live`, as `routes/playbackState.ts`. `live` publishes an SSE event to the same user's other streams.

- [ ] **Step 1: Port the `playRecording.ts` unit tests.** Add:
  - `play_recorded_and_listed`
  - `duplicate_within_window_not_double_counted`
  - `recently_listened_orders_by_last_play`
  - `playback_state_roundtrip`
  - `live_update_reaches_other_tab`
  - `queue_limit_enforced` (with the limit from upstream's `/limit`)
- [ ] **Step 2: Run, implement, run, then `cargo sqlx prepare`.**
- [ ] **Step 3: Commit** with message `feat: plays, recently listened and playback state`.

### Task 15: Playlists

**Files:**
- Create:
  - `crates/kima-db/migrations/0006_playlists.sql`
  - `crates/kima-db/src/playlists.rs`
  - `crates/kima-api/src/routes/playlists.rs`
- Test: `crates/kima-api/tests/playlists.rs`

**Interfaces:**
- Produces:
  - `playlists(id, user_id, name, is_public, created_at, updated_at)`
  - `playlist_items(id, playlist_id, track_id, sort int, unique(playlist_id, track_id))`
  - `hidden_playlists(user_id, playlist_id)`
  - Endpoints shaped as upstream `routes/playlists.ts`:
    - `GET /` (`:82`)
    - `POST /` (`:149`)
    - `GET /:id` (`:178`, readable by its owner, or by anyone if public)
    - `PUT /:id` (owner)
    - `POST /:id/hide` and `DELETE /:id/hide`
    - `DELETE /:id` (owner)
    - `POST /:id/items` and `DELETE /:id/items`
    - `POST /:id/items/batch`
    - `DELETE /:id/items/:trackId`
    - `PATCH /:id/items/:itemId/move`
  - Pending, Spotify and mix fields are returned empty or null as upstream's types require. `/pending/*` is not routed.

- [ ] **Step 1: Failing tests:**
  - `create_add_list_remove`
  - `duplicate_add_is_noop`
  - `move_reorders`
  - `private_playlist_404_for_others`
  - `public_playlist_readable_not_writable`: a write returns 403.
  - `hide_unhide`
  - `deleting_track_removes_from_playlists` (cascade)
- [ ] **Step 2: Run, implement, run, then `cargo sqlx prepare`.**
- [ ] **Step 3: Commit** with message `feat: playlists`.

### Task 16: Needs-attention report and organizer path building (pure)

**Files:**
- Create: `crates/kima-library/src/organize/mod.rs`, `src/organize/paths.rs`, `crates/kima-db/src/attention.rs`, `crates/kima-api/src/routes/library/attention.rs`
- Test: `crates/kima-library/src/organize/paths.rs` (unit), `crates/kima-api/tests/attention.rs`

**Interfaces:**
- Produces:
  - `organize::paths::Fields`: `album_artist`, `track_artist`, `album`, `year`, `disc`, `disc_total`, `track_no`, `title`, `ext` and `is_compilation`. The struct is deliberately independent of the scanner, so the M3 autotagger can feed corrected values (spec §3).
  - `organize::paths::target(f: &Fields) -> PathBuf`, following the Global Constraints pattern.
  - `organize::paths::sanitize_component(s) -> String`:
    - replaces `/ \ : * ? " < > |` and control characters with `_`;
    - strips trailing dots and spaces;
    - prefixes `_` to the Windows reserved names (CON, PRN, AUX, NUL, COM1-9, LPT1-9, case-insensitive, with or without an extension);
    - maps `.` and `..` to `_`;
    - truncates to 120 bytes on a UTF-8 boundary;
    - NFC-normalises.
  - `organize::paths::plan(tracks: &[(Uuid, Fields, PathBuf /* current rel */)], existing: &HashSet<PathBuf>) -> Vec<PlannedMove { track_id, from, to, collision: bool, unchanged: bool }>`. It is deterministic: sort by (to, track_id) before assigning ` (n)` suffixes. Sidecar files (images, `.cue`, `.log`) in each source album folder are planned to the album's target folder.
  - `GET /api/library/attention` (admin): a list per album of `{albumId, artist, album, year, issues: [..]}`. The issue codes are `artistFromPath`, `albumFromPath`, `unknownArtist`, `unknownAlbum`, `missingYear`, `conflictingYears`, `missingTrackNumbers`, `duplicateTrackNumbers` and `folderSplit`.

- [ ] **Step 1: Failing path tests:**
  - `basic_pattern`: `Radiohead/1997 - OK Computer/01 - Airbag.flac`
  - `multi_disc_prefix`: `2-05 - …`
  - `no_year`
  - `compilation_pattern`
  - `two_digit_padding`
  - **Review Focus 5,** `hostile_names`. `AC/DC` maps to `AC_DC`. Album `..` maps to `_`. `CON` maps to `_CON`. The title `a:b?c*` maps to `a_b_c_`. A 300-character title is ≤ 120 bytes and valid UTF-8. NFD "é" and NFC "é" give the same component. For every case, `target(..)` has no `..` component and is relative.
  - `collisions_get_suffix_deterministically`
  - `sidecars_follow_album`
  - `unchanged_detected`
- [ ] **Step 2: Failing attention tests:**
  - `reports_each_issue_code` (seed rows with matching `guessed` flags and data)
  - `requires_admin`
- [ ] **Step 3: Run, implement, run.**
- [ ] **Step 4: Commit** with message `feat(library): needs-attention report and organizer path planning`.

### Task 17: Organizer: preview, apply, journal, undo, switch root

**Files:**
- Create:
  - `crates/kima-db/migrations/0007_organizer.sql`
  - `crates/kima-db/src/organizer.rs`
  - `crates/kima-library/src/organize/apply.rs`
  - `crates/kima-api/src/routes/library/organizer.rs`
- Test: `crates/kima-library/tests/organizer.rs`, `crates/kima-api/tests/organizer.rs`

**Interfaces:**
- Consumes: `paths::plan`, `IgnoreSet` (Task 9), `kima_db::library`, `kima_db::settings` (`library_root`), the `library` queue.
- Produces:
  - `organizer_runs(id, mode 'copy'|'move', state 'previewed'|'applying'|'done'|'failed'|'undone', target_root, created_by, created_at, finished_at, summary jsonb)`
  - `organizer_journal(run_id, track_id null, from_path, to_path, sha256 bytea, state 'planned'|'done'|'skipped'|'undone', note)`
  - Endpoints (admin):
    - `POST /api/library/organizer/preview {mode}` creates a run and plans it as a job. It returns `{runId}`.
    - `GET /api/library/organizer/runs/:id?offset&limit` returns the run plus journal rows.
    - `POST /api/library/organizer/runs/:id/apply`
    - `POST /api/library/organizer/runs/:id/undo`
    - `POST /api/library/organizer/runs/:id/switch-root` (copy runs only, after `done`)
    - `GET /api/library/organizer/capabilities` returns `{copyAvailable: target set, moveAvailable: library root writable}`.
  - The route lives under `/organizer`, so it does not collide with upstream's `POST /library/organize`, which organizes Soulseek singles in M7.
  - Apply rules (spec §5.2):
    - Never overwrite: if the target exists, the row becomes `skipped` with a note.
    - Copy: write to `<to>.kima-part`, then fsync, verify the sha256, rename, and mark the row done.
    - Move on the same filesystem: rename. Across filesystems: copy, verify, then remove the source.
    - Before each move, add both paths to the `IgnoreSet`, update the track's `path`, and then remove them.
    - Remove source folders left empty.
    - Progress events every row, throttled to 4 per second.
  - Resume: a run left in `applying` at startup is re-enqueued, and rows already `done` are skipped. If a row's `to` exists with a matching sha256, mark it done; if it exists with a different sha256, mark it `skipped`.
  - Undo: process the done rows in reverse order. For a move, move the file back. For a copy, delete the copy, but only if its sha256 matches. Then mark the run `undone`.
  - Switch root: re-point `tracks.path` for every done row of the run, set `system_settings.library_root = target_root`, update `state.library_root`, and enqueue a reconcile.

- [ ] **Step 1: Failing tests:**
  - `preview_touches_nothing`: the tree's checksum is unchanged.
  - `copy_apply_creates_tree_and_keeps_source`
  - `copy_verify_detects_corruption`: a test hook flips a byte, so the row fails and no file is left at `to`.
  - `move_apply_updates_paths_and_keeps_plays`
  - `never_overwrites`
  - **Review Focus 4,** `crash_mid_move_resumes_cleanly`: inject a stop after N rows via a test hook, restart the apply, and assert that every file exists exactly once at its target, no source file is lost, and the counts match.
  - `undo_move_restores_all`
  - `undo_copy_removes_only_matching_copies`
  - `switch_root_repoints_and_reconcile_is_noop`: a reconcile after the switch reports 0 added and 0 missing.
  - `watcher_ignores_organizer_moves`
  - `move_unavailable_on_readonly_root`: 409 with `{"error":"Library folder is read-only; use copy mode"}`.
  - `admin_only`
- [ ] **Step 2: Run, implement, run, then `cargo sqlx prepare`.**
- [ ] **Step 3: Divergence.** Log D-034: upstream has no library organizer; kima-rs adds preview/copy/move/undo under `/api/library/organizer/*` (`accepted`).
- [ ] **Step 4: Commit** with message `feat(library): organizer apply, journal, resume, undo and switch root`.

### Task 18: Frontend: gating, stubs, new admin pages

**Files:**
- Modify:
  - `crates/kima-api/src/routes/system.rs` (the features flags)
  - the frontend nav and home components that render unported sections (find them by grepping for the routes of podcasts, audiobooks, discover, mixes, radio, downloads and import)
  - `frontend/lib/api.ts`
- Create:
  - `crates/kima-api/src/routes/stubs.rs`
  - `frontend/app/settings/organize/page.tsx`
  - `frontend/app/settings/attention/page.tsx`, plus small components under `frontend/features/organizer/`
- Test: `crates/kima-api/tests/auth_flow.rs` (features), frontend unit tests under `frontend/features/organizer/__tests__/`

**Interfaces:**
- Produces:
  - `/api/system/features` gains `library: true`, `podcasts: false`, `audiobooks: false`, `discover: false`, `mixes: false`, `downloads: false` and `imports: false`, keeping the 4 upstream flags.
  - The frontend hides each nav entry and home section whose flag is false. A direct visit to a hidden page shows a small "Not available yet" panel instead of erroring.
  - Stubs, with empty results in upstream's shape:
    - `GET /api/notifications/downloads/active` returns `[]` or upstream's empty shape.
    - `GET /api/discover/batch-status` returns upstream's idle shape.
  - The Organize page:
    - shows capabilities;
    - offers mode select and Preview;
    - renders a paged table (from → to, warnings) with filters "only changes", "collisions" and "guessed";
    - offers Apply, a progress bar fed by `library:progress` SSE, Undo, and "Switch library to the organized copy" (copy runs);
    - in move mode, shows a plain warning that files on disk will be renamed.
  - The Attention page is a table grouped by album, with issue chips and a filter by issue.
  - Both pages are admin-only in the nav, under Settings. Use existing settings-page components and styles.

- [ ] **Step 1: Failing Rust test.** `features_m1_flags` asserts the full features JSON.
- [ ] **Step 2: Failing frontend unit tests.** `organizer table filters` and `progress event updates bar`, using the repo's existing test setup.
- [ ] **Step 3: Implement.** Then run `npx tsc --noEmit`, `npm run test:unit` and `npm run build`. Expected: clean, all routes static.
- [ ] **Step 4: Divergence.** Update D-030: gating is now done (`resolved`). Log D-035: the new Organize and Attention pages (`accepted`).
- [ ] **Step 5: Commit** with message `feat(frontend): feature gating, poll stubs, organizer and attention pages`.

### Task 19: CI with Playwright and the end-to-end smoke test

**Files:**
- Modify: `.github/workflows/ci.yml`, `frontend/tests/e2e/kima-rs/smoke.spec.ts`
- Create: `frontend/tests/e2e/kima-rs/library.spec.ts`, `tools/make-fixture-library.sh` (ffmpeg sine fixtures in a messy layout, no copyrighted audio)

**Interfaces:**
- Consumes: everything above.
- Produces: a CI job `e2e` that runs on every push. It:
  1. installs ffmpeg via apt;
  2. builds the frontend export;
  3. builds `kima`;
  4. creates a fixture library with the script;
  5. starts kima with `KIMA_FRONTEND_DIR=frontend/out`, `MUSIC_PATH=<fixtures>`, `KIMA_ORGANIZE_TARGET=<tmp>` and `KIMA_WATCH_MODE=poll`;
  6. runs `npx playwright test -c playwright.kima-rs.config.ts`.
  The Rust job also installs ffmpeg, so fixture-based tests run.

- [ ] **Step 1: Write `library.spec.ts`:**
  - `scan, browse, play`: register, trigger a scan, open an artist, open an album, press play, and assert that `audio.currentTime > 0` within 5 s.
  - `seek works`: set `currentTime` to 1.0 and assert that it plays from there. The underlying request is a 206.
  - `playlist create and add`
  - `organizer preview lists moves`
  - `same-route navigation resets state`: from album A, navigate client-side to album B via a link on the page, and assert that B's title and B's track list render. Carried forward from M0.
  - `hidden sections are hidden`: no Podcasts nav entry.
- [ ] **Step 2: Run locally against a local kima** (as in the M0 final-review fix). Expected: all pass.
- [ ] **Step 3: Add the CI job, push the branch, and check the run on GitHub.** Expected: green.
- [ ] **Step 4: Commit** with message `ci: playwright e2e with fixture library`.

### Task 20: M1 wrap-up

**Files:**
- Modify: `README.md`, `docs/DIVERGENCES.md`, `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`

- [ ] **Step 1: README.** Cover:
  - the library: watching, `KIMA_WATCH_MODE`, and the inotify limit note;
  - the organizer: copy versus move, mounting a writable `/organized` in compose, and undo;
  - the new env vars;
  - the stream qualities.
  Add a commented-out `/organized` volume to `docker-compose.yml`.
- [ ] **Step 2: Full verification.**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `SQLX_OFFLINE=true cargo build --workspace`
  - `cargo test --workspace` (with ffmpeg present)
  - the frontend `tsc`, `test:unit` and `build`
  - the Docker image build and the Playwright run against the container
  Expected: all green.
- [ ] **Step 3: Divergence review.** Every M1 behaviour that differs from upstream has a row, the IDs are sequential, and every row has a status. Include D-036: track likes are not implemented, because upstream has no library-track likes (only discover-album likes, which belong to M7), and the parent spec's M1 row listing "likes" is amended (`accepted`).
- [ ] **Step 4: Roadmap.** Mark M1's plan cell ✅. Remove the carried-forward items M1 resolved, and keep the rest.
- [ ] **Step 5: Commit** with message `docs: M1 wrap-up`.
