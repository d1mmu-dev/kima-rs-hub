# M3b Autotagger and Organizer Rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The owner opens Settings > Library Tools > Match albums, picks an album, sees "MusicBrainz release *X*, distance 0.02 (strong)" with a per-track diff, clicks Accept, and the files carry Picard-compatible tags and sit at their organized paths; one click undoes it, and the audio of every file is byte-identical before and after.

**Architecture:** Matching lives in `kima_ident::autotag` (an `autotag` queue, one search job per album): candidates come from MusicBrainz release search, the album's tags and its recording MBIDs, each is scored with a beets-modelled distance, and everything apply needs is snapshotted on the candidate. When an apply is requested, `kima-ident` turns the snapshot into a plain `kima_db::autotag::Applied` value stored on the run, so the apply job, a `library` queue job in `kima-library`, needs no MusicBrainz code and makes no request. Apply writes tags through `kima_library::tagwrite` (a hidden copy, an audio-packet MD5 check, an atomic rename, journalled), updates the album row in place (stable id, artist re-parent / rename / create, collisions refused), and can chain an album-scoped organizer run. The organizer gains album scope, a by-album preview, and fixes for the parked M1 minors 2 and 5–8.

**Tech Stack:** Rust 2024, axum 0.8, sqlx 0.8, tokio, Postgres 17, lofty 0.25.4 (concrete tag types `Id3v2Tag`, `VorbisComments`, `Ilst`), ffmpeg (audio MD5). New dependency: `pathfinding = "4"` (in `kima-ident` only, `kuhn_munkres_min`). The forked Next.js 16 frontend (static export), vitest and Playwright.

**Spec:** `docs/superpowers/specs/2026-10-19-m3-identification-design.md` (binding; this plan covers **M3b only**: §4, §6.8, §6.9, §15 `autotagAutoApply`, §16.5, §17 `autotag:progress`, §19, §20, §21, §22, §23 D-119–D-123, §24, §26, §27). Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` (row M3). Style and granularity reference: `docs/superpowers/plans/2026-10-19-m3a-identification.md`. Divergence log: `docs/DIVERGENCES.md` (rows up to D-133 exist; D-119, D-120, D-121 and D-123 are reserved for M3b and absent; D-122 holds M3a's clauses).

**What M3b consumes from M3a (verified on master `d6df2fc`):**
- `kima_ident::musicbrainz` (`Release`, `Medium`, `Track`, `Credit`, `CreditArtist`, `ReleaseGroup`, `Genre`, `credit_names`, `top_genres`, private `get()`/`retype()`), `kima_ident::Mbid`, `kima_ident::text::{fold_identity, escape_lucene, normalize_for_search}`, `kima_ident::IdentCtx`, `kima_ident::events::ProgressEvents` (as a pattern only).
- `kima_net::{Net, Outcome, Req, Host}`, `kima_net::testing::{Replay, Recording, Entry}`.
- `kima_jobs::queue::{enqueue, enqueue_at, enqueue_unless_queued_with_key, requeue_running}`, `kima_jobs::worker::{Handler, JobCtx, JobError, handler_ctx}`.
- `kima_db::settings_kv::{AutoApply, KvSettings::autotag_auto_apply}` (already in M3a; GET/POST `/system-settings` already carry `autotagAutoApply`).
- `kima_library::scan::{LibraryCtx, update_path, reconcile_cancellable}`, `kima_library::watch::IgnoreSet`, `kima_library::organize::apply::{writable, IGNORE_GRACE}`, `kima_library::names::key`, `kima_library::tags::{RawTags, read}`.
- `kima_db::library::{upsert_track, refresh_album_mbids, prune_empty, VARIOUS_ARTISTS_MBID}`, `kima_db::stages`, `kima_db::retry_once_on_conflict`, `kima_db::attention`.
- Not yet present and added here: `scan::ReadCause`, `scan::reread_paths` (Task 14).

**Rulings where the spec is silent or ambiguous** (each is cheap to reverse; the owning task carries it):
1. The §19.6 values are computed in `kima-ident` when the apply is requested and stored on the run as `kima_db::autotag::Applied`; the job re-validates and never parses MusicBrainz JSON (Tasks 7, 19). Spec §19.9 step 1 says the job copies them from the candidate; the effect (values frozen on the run before any file is touched) is the same, and `kima-library` stays free of `kima-ident`.
2. Undo also restores what apply changed in the database beyond §19.7's list: each track's `recording_mbid`/`recording_source` (and its recording stage), the album's MusicBrainz genres and the artist's MBID, all captured in `album_before` (Task 15).
3. "Indexed through a symlink" is a `guessed.viaSymlink` flag in the existing jsonb, so §20.6 needs no migration (Task 11).
4. `cover_deferrals` gets its own migration `0023` (Task 2, reasons there).
5. Wipe and re-run keeps every track whose `recording_source` is `autotag`, whether accepted by hand or auto-applied (Task 2).
6. The release-group request of §19.2 step 6 stays: the release lookup's `release-groups` include carries no group genres (§27 Q3; Task 4).
7. The MusicBrainz fixtures for the autotagger are hand-written in the documented answer shape, with a `requests.json` for a later recording by hand, as M3a did for its source fixtures (Task 4).
8. Only plain searches are deduplicated per album; a manual query or MBID always queues (Task 8).
9. An autotagged album left with no present track returns to `identity_source='scan'` (Task 14).
10. A failed apply leaves the album `proposed`; `failed` is the search's state. Auto-apply never reorganizes (Tasks 16, 19).
11. "Busy" (409 on apply) = an autotag run of that album in `queued`/`writing`/`organizing`/`undoing`, or an organizer run in `planning`/`applying`/`undoing` (Task 19).
12. `string_dist` moves only a trailing `, the` to the front, as §19.3 says (beets also handles `, a`/`, an`; not ported).

## Global Constraints

- Everything in M0–M3a's Global Constraints still holds: GPL-3.0-only, edition 2024, `rust-version = "1.85"`, every version pinned in the workspace `Cargo.toml`; camelCase JSON on `/api`, errors `{"error": "<message>"}`; UUIDv7 ids; the database is never mocked (`#[sqlx::test(migrator = "kima_db::MIGRATOR")]`).
- **Before every commit:** `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `KIMA_NET_DENY_REAL=1 cargo test --workspace` against the dev Postgres (`DATABASE_URL=postgres://kima:kima@localhost:5456/kima`), all clean. Frontend tasks also run `npx tsc --noEmit`, `npm run test:unit`, `npm run build` in `frontend/`.
- **sqlx:** compile-checked `query!`/`query_as!`/`query_scalar!` for every new static query. After any query change run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/` in the same commit. CI builds with `SQLX_OFFLINE=true`; it must keep passing.
- **Commits:** the repo's configured identity (`dimmu <3417783+d1mmu-dev@users.noreply.github.com>`). **No `Co-Authored-By`, no AI or Claude attribution line of any kind** in any commit message. Conventional style. Do not push except in Task 24.
- **Divergence rows:** every kima-rs vs kima-hub difference goes into `docs/DIVERGENCES.md` in the same commit as the change that introduces it. Each task's **Divergences:** line names the rows its commit writes or extends. Rows D-119–D-123 take ID, area and both columns from spec §23, a one-line "Why" from the cited section or call, status `open`; where a row spans tasks, the first task writes its clauses and later tasks extend the same row, so after the last named task it reads as in §23 plus what this plan adds. A difference §23 does not list gets the next free ID, **D-134** onward (check the file's tail first).
- **No test reaches the network.** MusicBrainz is only ever reached through `kima_net::Net` with injected bases; tests use `kima_net::testing::Replay`; CI sets `KIMA_NET_DENY_REAL=1`. Apply and undo make no request at all (R3). New MusicBrainz fixtures are hand-written (Task 4); nothing is recorded in CI.
- **Never log or store a secret** (API keys) in logs, evidence, journal rows or cache keys.
- **ffmpeg is required in CI.** Every ffmpeg-dependent test starts with `if !require_ffmpeg() { return; }`. ffmpeg always gets `file:` followed by the absolute path.
- **Crate boundaries:** `kima-library` never depends on `kima-net` or `kima-ident`; shared autotag data types live in `kima-db` (`kima_db::autotag`). `kima-ident` never depends on axum.
- **Migrations:** `0021_autotag.sql` and `0022_organizer_scope.sql` are spec §6.8 and §6.9 verbatim (Task 1); `0023_artist_cover_deferrals.sql` is this plan's (Task 2).
- **Autotag constants (spec §19):** at most 10 candidates per album; release search `limit=5`, retried without `AND tracks:<n>` when it yields fewer than 2; at most 5 probed recordings (sources `file_tag`, `tracklist`, `isrc` first), releases kept when listed for `≥ ceil(k/2)` of `k`, at most 5; VA MBID `89ad4ac3-39f7-470e-963a-56509c546377`; snapshot stale after 30 days; recommendation strong `d < 0.04`, medium `d < 0.2`, else `none`, capped at medium by any missing or extra track; pair broken when its distance is `≥ 0.5`; Hungarian costs ×10⁶ as integers, padding cost 1.0; distances rounded to 3 decimals for ordering.
- **Weights (beets defaults, spec §19.3):** `album 3.0, artist 3.0, album_id 5.0, year 1.0, mediums 1.0, tracks 2.0, missing_tracks 0.9, unmatched_tracks 0.6`; per track `track_title 3.0, track_artist 2.0, track_index 1.0, track_length 2.0, track_id 5.0`; length grace 10 s, max 30 s.
- **Tag writing (spec §19.8):** only the keys of the §19.8 table are touched; hidden copy `<folder>/.kima-tag-<journal id>.<ext>`; `fsync` copy, re-read and verify, `rename`, `fsync` folder; original held in the `IgnoreSet` for the write plus `IGNORE_GRACE` (10 s); audio MD5 by `ffmpeg -v error -i file:<abs> -map 0:a:0 -c copy -f md5 -`; ID3v2.3 files stay v2.3, everything else MP3 is v2.4.
- **Events:** `autotag:progress` at most 4 a second, `{type, job:"search"|"apply"|"undo", albumId?, runId?, done, total, state?}`, `user_id: None`.
- **No new environment variables.** Spec §22's variables all arrived in M3a; M3b reads `autotagAutoApply` from `settings_kv` (already served by `/system-settings`).
- **Playwright and Docker runs keep `KIMA_IDENTIFY=off`.** The e2e test seeds a stored candidate (Task 23); no browser test triggers a lookup.

## Review Focus

1. **The watcher and a rescan running around an apply.** The tag write renames over the original; the watcher sees it after the 10 s grace and the next reconcile re-reads it. The album must keep its row id, its tracks must stay on it, no stage may be reopened and no fingerprint deleted, and no second album row may appear. The test lives in Task 14 (`rescan_after_tag_write_keeps_album_and_stages`).
2. **The album changing between search and apply:** a track added, removed or re-tagged elsewhere, or a file whose bytes changed after the journal was planned. Apply must refuse before touching any file (track set changed) or fail that file's row only (`changed outside kima-rs`), never write over unknown content. The tests live in Task 16 (`apply_refused_when_tracks_changed`, `file_changed_after_planning_fails_row_only`).
3. **File names ffmpeg and lofty could misread:** a basename starting with `-`, one containing `:`, one with non-UTF-8 bytes (Linux), and an album folder with a space and an apostrophe. The audio MD5 and the safe write must handle them like any other file. The test lives in Task 12 (`audio_md5_and_write_handle_odd_names`).
4. **Search text that breaks Lucene or a URL, and non-Latin titles:** `AC/DC`, `Don't Stop "Live" (Remix)`, `Guns N' Roses & Friends?`, `東京事変`. The query must be escaped once and percent-encoded once, and `string_dist` must compare two Japanese titles rather than empty them both. The tests live in Task 4 (`release_query_escapes`) and Task 5 (`string_dist_keeps_non_latin`).
5. **A crash in the middle of an apply or undo:** after the rename and before the journal commit, and with a leftover `.kima-tag-*` copy. Recovery must delete leftovers, mark a file already carrying the new tags `written`, redo one still at `before_sha`, and fail any other with `changed outside kima-rs`; undo must still restore every tag. The tests live in Task 16 (`crash_after_rename_recovered_as_written`, `leftover_tag_copies_deleted_on_resume`).

---

### Task 1: Migrations 0021–0022 and the autotag album/candidate store

**Files:**
- Create: `crates/kima-db/migrations/0021_autotag.sql` (spec §6.8 verbatim), `crates/kima-db/migrations/0022_organizer_scope.sql` (spec §6.9 verbatim), `crates/kima-db/src/autotag/mod.rs`, `crates/kima-db/src/autotag/albums.rs`, `crates/kima-db/tests/autotag.rs`
- Modify: `crates/kima-db/src/lib.rs` (`pub mod autotag;`)
- Test: `crates/kima-db/tests/autotag.rs`

**Divergences:** none (schema only).

**Interfaces:**
- Produces (`kima_db::autotag`):
  - `AlbumState { Queued, Searching, Proposed, NoCandidates, Failed, Applying, Applied, Skipped }`, `as_str()` = the §6.8 check values (`no_candidates` snake case).
  - `Recommendation { Strong, Medium, Weak }`, `as_str()` = `strong`, `medium`, `none`; serde with those strings.
  - `Origin { Tag, Search, Recording, Manual }`, serde lowercase.
  - `NewCandidate { release_mbid: Uuid, release_group_mbid: Uuid, origin: Origin, distance: f64, recommendation: Recommendation, penalties: Value, mapping: Value, extra_tracks: Value, missing_tracks: Value, release: Value, release_group: Value }`.
  - `CandidateRow` (all §6.8 columns, `FromRow`).
  - `LocalAlbum { id, title, artist_id, artist_name, year: Option<i32>, is_compilation, release_mbid: Option<Uuid>, release_mbid_source: Option<String>, release_group_mbid: Option<Uuid>, release_group_mbid_source: Option<String>, identity_source: String, tracks: Vec<LocalTrack> }` and `LocalTrack { id, path, disc: i32, track_no: i32, title, artist_name, duration_secs: i32, recording_mbid: Option<Uuid>, recording_source: Option<String>, tag_release_mbid: Option<Uuid>, tag_artist_mbids: Vec<Uuid>, disc_total: Option<i32>, track_total: Option<i32> }` (present tracks only, ordered `disc, track_no, path`).
  - `local_album(pool, album: Uuid) -> sqlx::Result<Option<LocalAlbum>>` (`None` when the album has no present track or does not exist).
  - `set_state(pool, album: Uuid, state: AlbumState, error: Option<&str>) -> sqlx::Result<()>` (upsert of `autotag_albums`, `updated_at = now()`).
  - `store_search(pool, album: Uuid, ordered: &[NewCandidate]) -> sqlx::Result<Option<Uuid>>`: one transaction; upserts each by `(album_id, release_mbid)` (refreshing every column and `fetched_at`), deletes this album's other candidates, sets `best_candidate` to the first's id, `recommendation` to its recommendation, `state` `proposed` (or `no_candidates` when `ordered` is empty, recommendation NULL), `searched_at = now()`, `error = NULL`. Returns the best id.
  - `candidates(pool, album) -> sqlx::Result<Vec<CandidateRow>>`, `candidate(pool, id) -> sqlx::Result<Option<CandidateRow>>`.
  - `unmatched_albums(pool) -> sqlx::Result<Vec<Uuid>>`: albums with a present track and `release_mbid IS NULL`.

- [ ] **Step 1: Failing tests** (autotag.rs):
  - `migration_constraints`: inserting an `autotag_albums` row with `state = 'bogus'` fails; `albums.identity_source = 'autotag'` with `applied_identity` NULL fails the check; `organizer_runs.state = 'superseded'` is accepted and `origin` defaults to `user`.
  - `candidate_cycle_set_null`: deleting a candidate referenced as `best_candidate` sets it NULL.
  - `store_search_replaces_and_sets_best` (two runs with overlapping release MBIDs: the stale one is deleted, the shared one keeps its id, `best_candidate` is the first of the second list).
  - `store_search_empty_is_no_candidates`.
  - `local_album_present_tracks_in_order` (a missing track is left out).
  - `unmatched_albums_skip_tagged_and_empty`.
- [ ] **Step 2: Run** `cargo test -p kima-db --test autotag`. Expected: fail (no migration, no module).
- [ ] **Step 3: Implement** the two migrations exactly as the spec prints them and the module.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass; the M1 organizer tests still pass with the new state check.
- [ ] **Step 5: Commit** `feat(db): autotag tables and organizer run scope (migrations 0021, 0022)`.

### Task 2: Enrichment cleanup: `cover_deferrals` column, wipe keeps autotag answers

**Files:**
- Create: `crates/kima-db/migrations/0023_artist_cover_deferrals.sql`
- Modify: `crates/kima-db/src/identity.rs` (`defer_completed_artist`, `ArtistForEnrichment`, `claim_artists`, `save_artist_outcome_in`, `reopen_completed_artist`), `crates/kima-db/src/library.rs` (`reopen_completed_artists`), `crates/kima-db/src/enrichment.rs` (`wipe`, the failures list and counts filters), `crates/kima-ident/src/artist.rs` (the `DEFER_CAP` doc and the three reads of `enrichment_failures` at the cover-retry sites), every other place `rg enrichment_failures crates` shows writing a negative number or resetting the count, `docs/DIVERGENCES.md` (D-113, D-126)
- Test: `crates/kima-ident/tests/album_covers.rs`, `crates/kima-ident/tests/control.rs`, `crates/kima-db/tests/autotag.rs`

**Why 0023 and not inside 0021/0022:** 0021 and 0022 stay byte-for-byte the spec's §6.8/§6.9 so the spec remains their reference; the column belongs to artist enrichment, not to the autotagger or the organizer; one migration per concern keeps the history greppable; and numbering it after 0022 keeps migrations landing in ascending order (this task runs after Task 1).

**Divergences:** D-113 amended (the count now lives in `artists.cover_deferrals`; drop the "stop-gap" clause); D-126 extended: "wipe and re-run leaves tracks whose `recording_source` is `autotag` (their recording rows, MBIDs, ISRCs and MusicBrainz genres) untouched".

**Interfaces:**
- Migration: `ALTER TABLE artists ADD COLUMN cover_deferrals smallint NOT NULL DEFAULT 0 CHECK (cover_deferrals >= 0);` then `UPDATE artists SET cover_deferrals = -enrichment_failures, enrichment_failures = 0 WHERE enrichment_failures < 0;` then `ALTER TABLE artists ADD CHECK (enrichment_failures >= 0);`.
- Produces: `identity::defer_completed_artist(pool, id, deferrals: i16)` now writes `cover_deferrals = deferrals` (status `failed`, `enrichment_failures` untouched). `ArtistForEnrichment` gains `cover_deferrals: i16`. Every reset that today writes `enrichment_failures = 0` on a reopen (wipe, `reopen_completed_artist(s)`, the editor paths) also writes `cover_deferrals = 0`; a completed write keeps it. `artist.rs` reads `a.cover_deferrals` instead of `a.enrichment_failures.min(0).saturating_neg()` and stops clamping `enrichment_failures` with `.max(0)`. The failures list/counts keep `enrichment_failures > 0` (now just the check, no sign trick).
- `enrichment::wipe` excludes tracks with `recording_source = 'autotag'` from the lock list and every later statement.

- [ ] **Step 1: Failing tests:**
  - `cover_deferrals_column_and_check` (autotag.rs): a new artist has `cover_deferrals = 0`; writing `enrichment_failures = -1` is refused by the new check. (The data move in 0023 runs before any test row exists, so it is reviewed, not tested.)
  - In `album_covers.rs`, rename the existing deferral-cap test's assertions to read `cover_deferrals` (cap 6 reached → stays `completed`, `cover_deferrals = 6`, `enrichment_failures = 0`) and add `new_album_resets_cover_deferrals`.
  - `wipe_keeps_autotag_answers` (control.rs): two tracks with `recording` rows `done`, one `recording_source='search'`, one `'autotag'`; after `wipe_and_rerun` the first is `pending` with no MBID, the second keeps `recording_mbid`, its stage row and its `musicbrainz` genres; `WipeCounts.tracks == 1`.
- [ ] **Step 2: Run** `cargo test -p kima-ident --test album_covers --test control && cargo test -p kima-db --test autotag`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** `fix(ident): cover retries get their own column; wipe keeps autotag answers`. Stage `docs/DIVERGENCES.md`.

### Task 3: Parked cleanup minors: busy through anyhow, scan transactions, `similar` visibility

**Files:**
- Modify: `crates/kima-api/src/error.rs`, `crates/kima-db/src/library.rs`, `crates/kima-library/src/scan.rs`, `crates/kima-ident/src/similar.rs`, `crates/kima-api/tests/library.rs` (the `candidates` test moves), `docs/DIVERGENCES.md` (D-130, D-131)
- Test: unit tests in `error.rs` and `similar.rs`, `crates/kima-library/tests/scan.rs`

**Divergences:** D-130 extended ("also when the conflict reaches the handler wrapped in another error"); D-131 extended ("marking tracks missing or restoring them commits together with their albums' refresh; a file's own store runs once more after a deadlock").

**Interfaces:**
- `ApiError::into_response`: an `Internal(e)` whose `e.chain()` holds a `sqlx::Error` for which `kima_db::is_retryable_conflict` is true answers exactly as `Busy` (503, `Retry-After: 1`, no error log).
- `kima_db::library::{mark_missing_in(conn: &mut PgConnection, paths: &[String]) -> sqlx::Result<u64>, clear_missing_in(conn, paths) -> sqlx::Result<u64>, albums_of_paths_in(conn, paths) -> sqlx::Result<Vec<Uuid>>}`; the pool versions delegate.
- `scan.rs`: a private `refresh_albums_in(conn, albums: &[Uuid])` (the body of today's `refresh_albums` closure). Each `MARK_CHUNK` of vanished paths, each chunk of restored paths, and `update_path`'s single-file mark/restore run "mark (or clear) + albums_of_paths + refresh" in **one** transaction through `retry_once_on_conflict`. `process_folder` calls `retry_once_on_conflict(|| store(...))` for both of its `store` call sites.
- `kima_ident::similar::{Candidate, candidates}` become `pub(crate)`; the test that used them moves into `similar.rs`'s `#[cfg(test)] mod tests` as a `#[sqlx::test(migrator = "kima_db::MIGRATOR")]`.

- [ ] **Step 1: Failing tests:**
  - `anyhow_wrapped_deadlock_is_503` (error.rs: `ApiError::Internal(anyhow::Error::new(fake_40P01).context("x"))` answers 503 with `Retry-After: 1` and body `{"error":"Temporarily busy, please retry"}`).
  - `mark_missing_and_album_refresh_commit_together` (scan.rs, using the `inject_once` trigger helper of `folder_refresh_retries_once_after_a_deadlock`, failing the first `refresh_album_mbids` update after a file is deleted): after the rescan the track is missing **and** the album's tag MBIDs follow the remaining track; `inject_once` used twice.
  - `store_retries_once_after_a_deadlock` (the same helper on the track upsert): the file is stored, not reported failed.
- [ ] **Step 2: Run** `cargo test -p kima-api --lib error && cargo test -p kima-library --test scan`. Expected: fail.
- [ ] **Step 3: Implement**, moving the similar test.
- [ ] **Step 4: Run** `cargo test --workspace`, then `cargo sqlx prepare --workspace -- --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** `fix: busy answers through wrapped errors, missing marks commit with their album refresh, store retried once`. Stage `docs/DIVERGENCES.md`.

### Task 4: MusicBrainz release lookups for the autotagger, with fixtures

**Files:**
- Modify: `crates/kima-ident/src/musicbrainz.rs`
- Create: `crates/kima-ident/tests/fixtures/musicbrainz/autotag.json`, `crates/kima-ident/tests/fixtures/musicbrainz/autotag-requests.json`, `crates/kima-ident/tests/autotag_mb.rs`; extend `crates/kima-ident/tests/fixtures/README.md` with an "Autotag fixtures (M3b)" paragraph
- Test: `crates/kima-ident/tests/autotag_mb.rs`, unit tests in `musicbrainz.rs`

**Divergences:** none.

**Interfaces:**
- Consumes: `Net::get_json`, `Req`, the private `get()`/`retype()`/`absent()` helpers.
- Produces:
  - `Release` gains `status: Option<String>`, `country: Option<String>`, `barcode: Option<String>`, `disambiguation: Option<String>`, `release_group: Option<ReleaseGroup>` (`release-group`); `Medium` gains `format: Option<String>`, `title: Option<String>`. All lenient like the existing fields; M3a's tests unchanged.
  - `ArtistClause<'a> { Name(&'a str), Various }`.
  - `release_query(title: &str, artist: ArtistClause<'_>, tracks: Option<usize>) -> String`: `release:"<T>" AND artist:"<A>" AND tracks:<n>` with `T`/`A` = `escape_lucene(normalize_for_search(..))`; `Various` gives `arid:89ad4ac3-39f7-470e-963a-56509c546377`; `None` drops the tracks clause.
  - `search_releases(net, query: &str) -> Outcome<Vec<Mbid>>`: `GET /release?query=<q>&limit=5&fmt=json`; never `Absent` on 404 (a search); ids in answer order.
  - `recording_releases(net, id: Mbid) -> Outcome<Vec<Mbid>>`: `GET /recording/{id}?inc=releases`, 404 `Absent`.
  - `release(net, id: Mbid) -> Outcome<Release>`: `GET /release/{id}?inc=recordings+artist-credits+release-groups+labels+isrcs+media+genres`, 404 `Absent`; a release without tracks is `Absent`.
  - `release_group(net, id: Mbid) -> Outcome<ReleaseGroup>`: `GET /release-group/{id}?inc=genres`, 404 `Absent`. (Spec §27 Q3: the release lookup's `release-groups` include carries no group genres, so this request stays.)
- Fixtures (hand-written, documented shape, the same convention as the M3a source fixtures; nothing sent to musicbrainz.org): one 10-track single-medium release ("Fixture Band – Fixture Album", 2001, track lengths 2000 ms so the 2 s ffmpeg tones match), one 2-medium release (3 + 3 tracks), one VA compilation (3 tracks, credit `Various Artists` with the VA MBID, per-track credits), their release groups with genres and counts, a search answer for each, a `recording?inc=releases` answer, and a 404. `autotag-requests.json` lists the same requests in `tools/record-fixtures` format.

- [ ] **Step 1: Failing tests:**
  - `release_query_escapes` (unit): `release_query("AC/DC", Name("Guns N' Roses & Friends?"), Some(3))` and `release_query("東京事変", Various, None)` equal hand-written expected strings; through a `Replay`, the asked URL for `Don't Stop "Live" (Remix)` is percent-encoded once.
  - `release_parses_new_fields` (status, country, barcode, media format, `release-group` id).
  - `release_lookup_inc_and_request_count` (the replay sees exactly one `/release/{id}` with the inc string above).
  - `release_without_tracks_absent`, `release_404_absent`, `search_404_is_permanent_not_absent`.
  - `recording_releases_lists_ids`.
- [ ] **Step 2: Run** `cargo test -p kima-ident --test autotag_mb`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p kima-ident`. Expected: pass, M3a's MusicBrainz tests unchanged.
- [ ] **Step 5: Commit** `feat(ident): MusicBrainz release search and lookups for the autotagger`.

### Task 5: `string_dist` and track distance

**Files:**
- Create: `crates/kima-ident/src/autotag/mod.rs`, `crates/kima-ident/src/autotag/distance.rs`, `crates/kima-ident/tests/autotag_distance.rs`
- Modify: `crates/kima-ident/src/lib.rs` (`pub mod autotag;`)
- Test: `crates/kima-ident/tests/autotag_distance.rs`

**Divergences:** none yet (D-119 is written in Task 6).

**Interfaces:**
- Consumes: `text::fold_identity`, `musicbrainz::{Track, Credit, credit_names}`, `kima_db::autotag::LocalTrack`.
- Produces (`kima_ident::autotag::distance`):
  - Weight constants named after the keys (`W_ALBUM = 3.0`, … `W_TRACK_ID = 5.0`), `LENGTH_GRACE_SECS = 10`, `LENGTH_MAX_SECS = 30`.
  - `string_dist(a: &str, b: &str) -> f64`: the spec §19.3 algorithm exactly (lowercase + `fold_identity`; `, the` suffix moved to the front; `&` → `and`; `basic` keeps `\p{L}\p{N}`, char-level Levenshtein over the longer length (hand-written, no new crate); the six patterns with weights 0.1, 0.0, 0.1, 0.3, 0.3, 0.2 in that order).
  - `Distance` (Default): `add(&mut self, key: &'static str, weight: f64, penalty: f64)`, `value(&self) -> f64` (`Σ w·p / Σ w`, 0 when empty), `shares(&self) -> BTreeMap<&'static str, f64>` (each key's `Σ w·p / Σ w`; sums to `value`).
  - `year_penalty(local: i32, release_year: i32, original_year: Option<i32>, this_year: i32) -> f64`.
  - `length_penalty(local_secs: i32, release_ms: u32) -> f64` = `min(1, max(0, |l − r| − 10) / 30)`, release ms through `musicbrainz::ms_to_secs`.
  - `ReleaseTrackRef<'a> { track: &'a Track, medium_position: u32, absolute_index: u32 }`.
  - `track_distance(local: &LocalTrack, rt: &ReleaseTrackRef<'_>, release_credit: &str, release_is_va: bool) -> f64` (the §19.3 track table; `track_artist` added when the release is VA or the track's joined credit differs from `release_credit`; `track_id` only when `recording_mbid` is set).

- [ ] **Step 1: Failing tests** (beets `test_autotag.py` `StringDistanceTest`, then kima-rs cases):
  - `equal_strings_zero` (`"Some String"`×2), `different_strings_nonzero` (`"Some String"`, `"Totally Different"`), `punctuation_ignored` (`"Some String"`, `"Some.String!"` → 0), `case_ignored` (`"sOME sTring"` → 0), `leading_the_has_lower_weight` (`d("The Band Name","Band Name") < d("XXX Band Name","Band Name")`), `parens_have_lower_weight` (`"One (Two)"` < `"One .Two."` against `"One"`), `brackets_have_lower_weight` (`"One [Two]"`), `ep_label_has_zero_weight` (`"My Song (EP)"`, `"My Song"` → 0), `featured_has_lower_weight` (`"My Song feat Someone"` < `"My Song blah Someone"`), `postfix_the` (`"The Song Title"`, `"Song Title, The"` → 0), `empty_strings_zero`, `solo_patterns_do_not_panic` (`"The"`/`""`, `"(EP)"`/`"(EP)"`, `", An"`/`""`), `heuristic_does_not_harm_distance` (`"Untitled"`, `"[Untitled]"` → 0), `ampersand_expansion` (`"And"`, `"&"` → 0), `accented_characters` (`"éáñ"`, `"ean"` → 0).
  - `string_dist_keeps_non_latin` (Review Focus 4): `d("東京事変","東京事変") = 0`, `d("東京事変","椎名林檎") = 1.0`.
  - `year_penalty_table`: `(1999, 1999, None) = 0`; `(1990, 2005, Some(1990)) = 0`; `(1999, 2005, Some(1990), this 2026) ≈ 6/36`; `(1999, 2005, None) = 1`.
  - `length_penalty_table`: diff 9 s → 0; 15 s → 5/30; 50 s → 1.
  - `track_index_by_medium_or_absolute`: local `track_no` 4 matches medium position 4 → 0; matches absolute index 4 on disc 2 → 0; neither → 1.
  - `distance_normalises_added_keys_only` and `shares_sum_to_value`.
- [ ] **Step 2: Run** `cargo test -p kima-ident --test autotag_distance`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same. Expected: pass.
- [ ] **Step 5: Commit** `feat(ident): beets-style string and track distance`.

### Task 6: Track mapping, album distance, ordering, recommendation

**Files:**
- Create: `crates/kima-ident/src/autotag/score.rs`
- Modify: root `Cargo.toml` (`pathfinding = "4"`), `crates/kima-ident/Cargo.toml`, `crates/kima-ident/src/autotag/mod.rs`, `crates/kima-ident/tests/autotag_distance.rs`, `docs/DIVERGENCES.md` (D-119)
- Test: `crates/kima-ident/tests/autotag_distance.rs`

**Divergences:** D-119 written with its first clauses: "Beets-style autotagger: weighted distance (beets weights; strong < 0.04, medium < 0.2)", why: spec §19.3–§19.4 (C13).

**Interfaces:**
- Consumes: Task 5, `musicbrainz::{Release, ReleaseGroup}`, `kima_db::autotag::{LocalAlbum, Recommendation}`.
- Produces (`kima_ident::autotag::score`):
  - `MappedPair { track_id: Uuid, medium: u32, position: u32, release_track_mbid: Mbid, distance: f64 }` and `MissingTrack { medium: u32, position: u32, title: String, length: Option<u32> }`, serde camelCase (the §6.8 `mapping`/`missing_tracks` JSON).
  - `Mapping { pairs: Vec<MappedPair>, extra: Vec<Uuid>, missing: Vec<MissingTrack> }`.
  - `map_tracks(local: &LocalAlbum, release: &Release) -> Mapping`: square matrix of size `max(n_local, n_release)` padded with cost 1.0, costs `(d × 1e6).round() as i64`, `pathfinding::kuhn_munkres::kuhn_munkres_min`; a pair with distance `≥ 0.5` becomes one extra and one missing.
  - `Scored { distance: f64, recommendation: Recommendation, penalties: BTreeMap<String, f64>, mapping: Mapping }`.
  - `score(local: &LocalAlbum, release: &Release, group: &ReleaseGroup, this_year: i32) -> Scored`: the album table of §19.3 (`artist` skipped when the local artist is "Unknown Artist", 0 when both are VA; `album_id` when local `release_mbid` is set; `year` when both years are known, the original year from `group.first_release_date`; `mediums` when the local disc count is > 1 or any `disc_total` is read; `tracks` once per pair; `missing_tracks` and `unmatched_tracks` once per item).
  - `recommend(distance: f64, incomplete: bool) -> Recommendation`.
  - `OrderKey` and `order(cands: &mut [(Release, Scored)])`: distance rounded to 3 decimals, `Official` first, media rank (`Digital Media`, `CD`, any `… CD`, others; worst medium of the release), country rank (`XW`, `XE`, others), earliest date (the `choose_release` padding rule), MBID.

- [ ] **Step 1: Failing tests:**
  - `perfect_match_strong` (Task 4's 10-track fixture against a local album with the same titles, numbers, lengths 2 s, no year, no MBIDs): `distance == 0.0`, strong, 10 pairs.
  - `renamed_track_breaks_pair_and_caps_medium`: one local title replaced by `"Zzzz"`: 9 pairs, 1 extra, 1 missing, `distance ≈ 1.5 / 25.5` (±1e-9), medium.
  - `missing_track_caps_medium` (9 local tracks): one missing, medium even though `d < 0.04`.
  - `wrong_year_penalty`: local year 1990, release 2001, group first release 2001, `this_year` 2026 → the `year` share is `1/(Σw)` (penalty 1).
  - `va_compilation_artist_zero` (local album artist "Various Artists", the VA fixture): `artist` share 0, `track_artist` added per pair.
  - `two_disc_mapping` (the 2-medium fixture, local discs 1–2): every pair on the right medium, `mediums` penalty 0.
  - `ordering_tie_breakers`: equal distances order Official before Bootleg, then `Digital Media` before `CD` before `Vinyl`, then `XW` before `GB`, then the earlier date, then the smaller MBID.
  - `hungarian_prefers_global_minimum` (two locals that each fit the other's greedy best).
- [ ] **Step 2: Run** `cargo test -p kima-ident --test autotag_distance`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: pass.
- [ ] **Step 5: Commit** `feat(ident): album mapping, distance, ordering and recommendation`. Stage `docs/DIVERGENCES.md`.

### Task 7: The applied values and the changes diff

**Files:**
- Create: `crates/kima-db/src/autotag/applied.rs`, `crates/kima-ident/src/autotag/proposal.rs`, `crates/kima-ident/tests/autotag_proposal.rs`
- Modify: `crates/kima-db/src/autotag/mod.rs`, `crates/kima-ident/src/autotag/mod.rs`
- Test: `crates/kima-ident/tests/autotag_proposal.rs`

**Divergences:** none.

**Interfaces:**
- Produces (`kima_db::autotag`, serde camelCase, `Clone + PartialEq + Debug`; the type that crosses from `kima-ident` to `kima-library`):
  - `AppliedAlbum { title: String, artist: String, artist_sort: String, artist_mbids: Vec<Uuid>, release_mbid: Uuid, release_group_mbid: Uuid, date: Option<String>, original_date: Option<String>, year: Option<i32>, compilation: bool, genres: Vec<String>, disc_total: u32 }`.
  - `AppliedTrack { track_id: Uuid, matched: bool, disc: Option<u32>, disc_total: Option<u32>, track_no: Option<u32>, track_total: Option<u32>, title: Option<String>, artist: Option<String>, artist_sort: Option<String>, artist_mbids: Vec<Uuid>, recording_mbid: Option<Uuid>, release_track_mbid: Option<Uuid> }` (an extra track: `matched: false`, every per-track field `None`/empty).
  - `Applied { album: AppliedAlbum, tracks: Vec<AppliedTrack>, mbids_only: bool }`.
- Produces (`kima_ident::autotag::proposal`):
  - `applied(release: &Release, group: &ReleaseGroup, mapping: &Mapping) -> Applied` per the §19.6 table: credit line `name + joinphrase`; sort line from `artist.sort-name + joinphrase`; year from the original date else `R.date`; compilation iff the release credit's single artist id is the VA MBID; genres = `top_genres(group.genres, 3)` else `top_genres(release.genres, 3)`; track title is the release track title; track artist from the track credit else the release credit.
  - `mbids_only(a: Applied) -> Applied`: keeps the MBIDs and the disc/track numbers and totals; `title`, `artist`, `artist_sort`, album `title`/`artist`/`artist_sort`/`year`/`compilation`/`date`/`original_date`/`genres` set to the local values the caller passes through `with_local_identity(&mut Applied, &LocalAlbum)`; `mbids_only = true`.
  - `Change { track_id: Option<Uuid>, field: &'static str, current: Value, proposed: Value }` (serde camelCase).
  - `changes(local: &LocalAlbum, a: &Applied) -> Vec<Change>`: album fields first (`album`, `albumArtist`, `albumArtistSort`, `albumArtistMbids`, `releaseMbid`, `releaseGroupMbid`, `date`, `originalDate`, `year`, `compilation`, `genres`), then per track in local order (`disc`, `discTotal`, `trackNo`, `trackTotal`, `title`, `artist`, `artistSort`, `artistMbids`, `recordingMbid`, `releaseTrackMbid`). Every field is listed (the frontend hides unchanged ones); `current` is `null` for what the database does not hold (sort names, dates, release-track MBID).
  - `user_mbid_kept(local: &LocalAlbum) -> bool`: the album's `release_mbid_source` or `release_group_mbid_source` is `user` (R2; the review page says so, apply keeps them).

- [ ] **Step 1: Failing tests:**
  - `applied_from_fixture_release` (every §19.6 row for track 1 of the 10-track fixture: title, credit line, sort line, MBIDs, `n/t` numbers, original date from the group, year from it, top-3 genres by count).
  - `genres_fall_back_to_release_then_empty`; `va_release_is_compilation`; `track_credit_overrides_release_credit`.
  - `extra_track_gets_album_fields_only`.
  - `mbids_only_keeps_local_identity`.
  - `changes_lists_every_field_with_nulls_for_unstored`.
  - `applied_round_trips_through_json` (the shape `autotag_runs.applied` stores).
- [ ] **Step 2: Run** `cargo test -p kima-ident --test autotag_proposal`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same. Expected: pass.
- [ ] **Step 5: Commit** `feat: autotag applied values and the proposed changes`.

### Task 8: The search job and the `autotag` queue

**Files:**
- Create: `crates/kima-ident/src/autotag/search.rs`, `crates/kima-library/src/autotag/mod.rs`, `crates/kima-library/src/autotag/events.rs`, `crates/kima-ident/tests/autotag_search.rs`
- Modify: `crates/kima-library/src/lib.rs` (`pub mod autotag;`), `crates/kima-ident/src/autotag/mod.rs`, `crates/kima-server/src/jobs.rs` (`register_autotag`, `prepare_autotag_queue`), `crates/kima-server/src/main.rs`, `crates/kima-server/tests/library_jobs.rs` (or the file that checks registered queues), `docs/DIVERGENCES.md` (D-119)
- Test: `crates/kima-ident/tests/autotag_search.rs`, unit tests in `events.rs`

**Divergences:** D-119 extended: "MusicBrainz release candidates (snapshotted, apply offline)".

**Interfaces:**
- Consumes: Tasks 1, 4, 6, 7; `IdentCtx`; `kima_jobs::queue::{enqueue_unless_queued_with_key, enqueue, enqueue_at, requeue_running}`; `worker::handler_ctx`.
- Produces:
  - `kima_library::autotag::events::{AutotagJob { Search, Apply, Undo }, AutotagEvents}`: `AutotagEvents::new(bus: EventBus)`, `progress(&mut self, job, album: Option<Uuid>, run: Option<Uuid>, done: usize, total: usize, state: Option<&str>)` (dropped inside 250 ms of the last one), `finish(&mut self, …)` (always sent). Payload per Global Constraints.
  - `kima_ident::autotag::search::{QUEUE = "autotag", MAX_ATTEMPTS = 3, MAX_CANDIDATES = 10}`.
  - `ManualQuery { artist: String, album: String }`, `SearchJob { album_id: Uuid, query: Option<ManualQuery>, release_mbid: Option<Mbid> }` (payload `{"kind":"search","albumId",…}`, camelCase).
  - `parse_release_ref(s: &str) -> Option<Mbid>`: an MBID, or a URL whose path is `/release/<mbid>` on `musicbrainz.org` (any subdomain, http or https, trailing segments ignored).
  - `enqueue_search(pool, job: &SearchJob) -> sqlx::Result<Option<Uuid>>`: sets the album `queued`; a plain search (no query, no MBID) is deduped on `albumId` against queued jobs; a manual one never is.
  - `collect(ctx: &IdentCtx, album: &LocalAlbum, job: &SearchJob) -> Outcome<Vec<(Mbid, Origin)>>`: §19.2 steps 1–4 in order, deduped by release MBID, at most 10.
  - `run_search(ctx: &IdentCtx, job: &SearchJob, attempt: SearchAttempt) -> Result<(), JobError>` with `SearchAttempt { last: bool }`: state `searching`; collect; fetch each release and its release group (step 5–6); score with `this_year` from the clock; `order`; `store_search`; events `{job:"search"}` per fetched candidate. A `RateLimited { until }` from any request re-enqueues the same payload with `enqueue_at(until)` and returns `Ok` (album stays `queued`); `Transient` returns `JobError::Retry`, and on the last attempt first sets `failed` with the error; `Permanent` sets `failed` and returns `JobError::Permanent`. Identification off: `failed` with `Outbound lookups are turned off`, `Permanent`. A release or group that is `Absent` is skipped, not fatal. The album gone (`local_album` = `None`): `Permanent("album not found")`, no state row written.
  - `handler(ctx: IdentCtx) -> Handler` (the job's `attempts == max_attempts` is `last`).
  - `kima_server::jobs::JobRegistry::register_autotag(&mut self, ctx: IdentCtx)` (concurrency 1) and `prepare_autotag_queue(pool) -> sqlx::Result<()>` (`requeue_running`); main calls both next to the enrich queue's.
  - Auto-apply is added to the end of `run_search` in Task 19.

- [ ] **Step 1: Failing tests** (autotag_search.rs; library fixture with ffmpeg tones tagged by `make_audio`, MusicBrainz through `Replay` with the Task 4 fixtures):
  - `untagged_copy_matches_strong` (§1 M3b success): 10 untagged-but-titled tracks of "Fixture Band – Fixture Album" → state `proposed`, best candidate is the fixture release, recommendation `strong`, `release` and `release_group` snapshots stored, `fetched_at` set.
  - `request_budget`: the replay saw at most 1–2 searches, ≤ 5 recording lookups, ≤ 10 release and ≤ 10 release-group fetches; a second identical search makes no request (the `http_cache`).
  - `tag_candidate_first_and_manual_mbid` (origins `tag`, `manual` in that order, then `search`).
  - `recording_candidates_majority` (3 probed recordings, a release listed by 2 is kept, one listed by 1 is not).
  - `compilation_uses_arid`; `fewer_than_two_retries_without_tracks`.
  - `rate_limit_reenqueues_and_returns_ok` (a 503 from MusicBrainz: one new `autotag` job with `run_at` ≥ the pause, the album `queued`).
  - `transient_failure_fails_album_only_on_last_attempt`.
  - `identify_off_refuses_without_request` (`asked()` empty).
  - `parse_release_ref_forms` (bare, `https://musicbrainz.org/release/<id>`, `https://beta.musicbrainz.org/release/<id>/discids`, a non-MusicBrainz host → `None`, a recording URL → `None`).
  - `plain_search_deduped_manual_not`.
  - `events_throttled_and_final_sent` (events.rs, paused clock).
- [ ] **Step 2: Run** `cargo test -p kima-ident --test autotag_search && cargo test -p kima-library --lib autotag`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets` if any query was added.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass, including the server test that lists registered queues (now `autotag`, `enrich`, `library`).
- [ ] **Step 5: Commit** `feat(ident): autotag candidate search on its own queue`. Stage `docs/DIVERGENCES.md`.

### Task 9: Organizer: album scope, `superseded`, preview by album, `disc_total`

**Files:**
- Modify: `crates/kima-db/src/organizer.rs`, `crates/kima-library/src/organize/apply.rs` (`preview`, `fields_of`, `switch_root`), `crates/kima-api/src/routes/library/organizer.rs`, `docs/DIVERGENCES.md` (D-121)
- Test: `crates/kima-library/tests/organizer.rs`, `crates/kima-api/tests/organizer.rs` (the existing organizer API test file; create it if the organizer API tests live elsewhere, after checking with `rg -l organizer crates/kima-api/tests`)

**Divergences:** D-121 written with its first clauses: "Album-scoped runs, `superseded` state, preview grouped by album" (amends D-069, D-070 — add a note to those rows).

**Interfaces:**
- Produces:
  - `organizer::Run` gains `album_ids: Option<Vec<Uuid>>`, `origin: String` (and `RUN_COLUMNS`).
  - `organizer::create_scoped_run(pool, id, mode, source_root, target_root, created_by: Option<Uuid>, album_ids: &[Uuid], origin: &str) -> sqlx::Result<Run>`; `create_run` delegates with `NULL` scope and origin `user`.
  - `PlanTrack` gains `disc_total: Option<i32>`; `plan_tracks(pool, album_ids: Option<&[Uuid]>) -> sqlx::Result<Vec<PlanTrack>>`. `fields_of` fills `Fields.disc_total` from it (0 when absent).
  - `preview` plans only the scoped tracks; `existing` is still every file under the source (move) or target (copy).
  - `organizer::supersede_previewed(conn: &mut PgConnection) -> sqlx::Result<u64>`: every `previewed` run → `superseded`.
  - `organizer::rows_by_album(pool, run_id, offset: i64, limit: i64) -> sqlx::Result<(Vec<AlbumRows>, i64)>` with `AlbumRows { album_id: Option<Uuid>, artist: Option<String>, album: Option<String>, rows: Vec<RowView> }`: albums ordered by artist, album, id; sidecars grouped with the album whose track shares their target folder, else a trailing group with `album_id` NULL; `total` = number of groups.
  - `switch_root` refuses a run with `album_ids` set: `OrganizeError::Refused("Only a whole-library copy can become the library")`, and the route answers 409 with that message.
  - `GET /api/library/organizer/runs/{id}/albums?offset&limit` (admin; `limit` default 20, ≤ 100): `{albums:[{albumId, artist, album, rows:[row_json]}], total}`.
  - `POST /api/library/organizer/preview` accepts `albumIds?: [uuid]` (an empty list = 400 `albumIds must not be empty`).
  - Applying a `superseded` run answers 409 `This preview is out of date; preview again`.

- [ ] **Step 1: Failing tests:**
  - `scoped_preview_plans_only_those_albums` and `scoped_preview_never_claims_another_albums_name` (another album's file sits at the scoped track's target: the scoped row gets ` (2)`).
  - `switch_root_refuses_scoped_run`.
  - `disc_total_used_in_names` (a 2-disc album whose naming rule uses the total).
  - `rows_by_album_never_splits_a_group` (25 albums: page 1 has 20 groups with all their rows, page 2 has 5).
  - API: `preview_with_album_ids`, `runs_albums_route_pages`, `apply_superseded_409`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test organizer && cargo test -p kima-api --test organizer`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass, the M1 organizer tests unchanged.
- [ ] **Step 5: Commit** `feat(organizer): album-scoped runs, superseded previews, preview pages by album`. Stage `docs/DIVERGENCES.md`.

### Task 10: Organizer minors 5–7, the resume-fsync nit, `.lrc`/`.nfo` sidecars

**Files:**
- Modify: `crates/kima-library/src/organize/{apply.rs, paths.rs}`, `docs/DIVERGENCES.md` (D-121)
- Test: `crates/kima-library/tests/organizer.rs`, unit tests in `paths.rs`

**Divergences:** D-121 extended: "`.lrc`/`.nfo` sidecars, no duplicate copies on a second copy run, content check on sidecar move undo, hard-link-safe `waiting` set, full fsync walk on resume".

**Interfaces:**
- `paths::is_sidecar` accepts `jpg jpeg png gif webp bmp cue log lrc nfo` (case-insensitive).
- Minor 5: in a copy preview, a planned target that already exists in the target with the source's size and SHA-256 (SHA only when sizes match) is a `NewRow { unchanged: true, collision: false }` at that path, not a suffixed collision.
- Minor 6: undo of a row with `track_id` NULL moves the file back only when it still hashes to the row's `sha256`; otherwise the row becomes `undo_failed` with note `changed since the move` and the file stays.
- Minor 7: `waiting: HashMap<(u64, u64), usize>` (insert increments, the move decrements, a key waits while its count > 0).
- Resume-fsync nit: `move_file` takes `started: bool`; when true, the fsync walk after a cross-filesystem copy goes up to the target root instead of stopping at the deepest pre-existing folder.

- [ ] **Step 1: Failing tests:**
  - `lrc_and_nfo_travel_with_album` (paths unit and an apply).
  - `second_copy_run_plans_no_duplicates` (copy, apply, preview again: every row `unchanged`, no ` (2)` path; a file changed in the target is still a collision).
  - `sidecar_undo_checks_content` (move, overwrite the moved `cover.jpg` with other bytes, undo: the row is `undo_failed` `changed since the move`, the file stays at the target; a track row still undoes).
  - `hard_linked_duplicates_keep_waiting` (two rows whose sources are hard links of one inode and a third row targeting that inode's current path: no refuse or skip, every row `done`).
  - `resume_fsyncs_up_to_root` (with `force_cross_fs` and `stop_mid_row`: on resume the started row's fsync walk reaches the target root; observe through a test hook `Hooks.on_sync_dir: Option<Arc<dyn Fn(&Path)>>` that records synced folders).
- [ ] **Step 2: Run** `cargo test -p kima-library --test organizer`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `fix(organizer): no duplicate copies, sidecar undo content check, hard-link-safe waiting, full fsync on resume`. Stage `docs/DIVERGENCES.md`.

### Task 11: Symlinks (M1 minor 2) and the needs-attention `symlink` issue

**Files:**
- Modify: `crates/kima-library/src/scan.rs` (`update_path`, `reconcile_cancellable`), `crates/kima-library/src/watch.rs` (`expand`), `crates/kima-db/src/library.rs` (`Guessed`, new setter), `crates/kima-db/src/attention.rs`, `docs/DIVERGENCES.md` (D-123; a note on D-068)
- Test: `crates/kima-library/tests/{scan.rs, watch.rs}`, `crates/kima-db/tests/attention.rs` (or the existing attention test file), `crates/kima-api/tests/library.rs` (stream still serves)

**Divergences:** D-123 (in full, spec §23 text), amends D-068.

**Interfaces:**
- Ruling: the "indexed through a symlink" flag is stored in `tracks.guessed` (jsonb), so no migration: `Guessed` gains `via_symlink: bool` (`#[serde(default)]`, camelCase `viaSymlink`).
- `library::set_via_symlink(pool, paths: &[String], on: bool) -> sqlx::Result<u64>` (jsonb update of `guessed`).
- `scan::symlinked(root: &Path, rel: &str) -> bool`: the file or any ancestor folder under `root` is a symlink (`symlink_metadata`), `pub(crate)`.
- `update_path` uses `tokio::fs::symlink_metadata`; a symlink (file or ancestor) is: `Ignored` when no row exists; for an existing row whose link target exists, `Unchanged` (and `set_via_symlink(true)`), never `MarkedMissing`.
- `watch::expand` uses `symlink_metadata` and never descends into, or emits, a symlink.
- Reconcile: among the vanished rows, a path where `symlinked(root, path)` is true and `root.join(path)` resolves to an existing file is not vanished; it is flagged `via_symlink` and counted unchanged. A row no longer under a link has the flag cleared when re-read.
- `attention::SYMLINK = "symlink"`, raised by `any(|g| g.via_symlink)`, declared after `FOLDER_SPLIT`.
- The stream route is unchanged (still serves such tracks); the organizer and the tag writer refuse them (the organizer already does via `symlinked_ancestor`; Task 12 for the writer).

- [ ] **Step 1: Failing tests:**
  - `update_path_refuses_new_symlink` (a symlinked file and a file under a symlinked folder: `Ignored`, no row).
  - `expand_skips_symlinked_dirs` (watch.rs).
  - `indexed_symlink_never_marked_missing` (index a real file, replace it with a symlink to a copy elsewhere, reconcile twice and `update_path` once: the row is present each time, never `missing_since`, never purged after the grace with a mocked old timestamp; `guessed.viaSymlink` true).
  - `dangling_symlink_is_missing`.
  - `attention_lists_symlink_issue`.
  - `stream_serves_symlinked_track` (api).
- [ ] **Step 2: Run** `cargo test -p kima-library --test scan --test watch && cargo test -p kima-db && cargo test -p kima-api --test library`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `fix(library): symlinks are never indexed anew and never purged; attention lists them`. Stage `docs/DIVERGENCES.md`.

### Task 12: `tagwrite`: native mapping, audio MD5, safe write, ID3v2

**Files:**
- Create: `crates/kima-library/src/tagwrite/{mod.rs, values.rs, id3.rs, audio.rs, safe.rs}`, `crates/kima-library/tests/tagwrite.rs`
- Modify: `crates/kima-library/src/lib.rs`, `crates/kima-library/Cargo.toml` (`hex` from the workspace), `crates/kima-library/src/organize/apply.rs` (`sha256_file` becomes `pub(crate)` for reuse), `docs/DIVERGENCES.md` (D-120)
- Test: `crates/kima-library/tests/tagwrite.rs`

**Divergences:** D-120 written with its first clauses: "Writes Picard-compatible tags (§19.8; ID3v2.3 files stay v2.3), verified by an audio-packet hash", why spec §19.8 (C14, R4).

**Interfaces:**
- Consumes: `kima_db::autotag::{AppliedAlbum, AppliedTrack}`, `IgnoreSet`, `IGNORE_GRACE`.
- Produces (`kima_library::tagwrite`):
  - `Container { Mp3, Flac, Ogg, Opus, Mp4 }`; `container_of(path: &Path) -> Result<Container, TagWriteError>` by lofty's probed file type; anything else is `Unsupported(ext)` with message `Tag writing is not supported for .<ext> files`.
  - `Id3Version { V3, V4 }` (`as_str` = `v3`/`v4`); `id3_version(path) -> Result<Option<Id3Version>, TagWriteError>` (`None` for non-MP3 and for an MP3 without ID3v2, which gets v2.4).
  - `TagValues` (`values.rs`): the §19.8 fields as `Option`s (`None` = not touched); `TagValues::from_applied(album: &AppliedAlbum, track: &AppliedTrack) -> TagValues` (an extra track gets album-scope fields except the MBIDs, §19.6). (`mbids_only` applies are always target `db`, so they never reach the writer.)
  - `NativeTags = BTreeMap<String, Vec<String>>`, keys spelled exactly as the §19.8 table (`TIT2`, `TXXX:MusicBrainz Album Id`, `UFID:http://musicbrainz.org`, `TRCK`, …; Vorbis upper-case names; MP4 `©nam`, `trkn`, `----:com.apple.iTunes:MusicBrainz Track Id`, …). `[]` means absent.
  - `native(container: Container, id3: Option<Id3Version>, v: &TagValues) -> NativeTags`.
  - `read_native(path, container, keys: &[String]) -> Result<NativeTags, TagWriteError>`.
  - `audio_md5(path: &Path) -> Result<[u8; 16], TagWriteError>` (the Global Constraints command, parsing `MD5=<hex>`).
  - `WriteHooks { fail_lofty: bool, stop_after_rename: bool }` (Default; tests only).
  - `safe_write(path: &Path, journal_id: i64, container: Container, id3: Option<Id3Version>, new: &NativeTags, ignore: &IgnoreSet, hooks: &WriteHooks) -> Result<Written, TagWriteError>` with `Written { after_sha: [u8; 32], audio_md5: [u8; 16] }`: the §19.8 safe-write sequence; the original's audio MD5 is taken before the copy and compared with the copy's; a mismatch deletes the copy and returns `AudioChanged`. Concrete tag types only; every untouched frame/comment/atom preserved; a v2.3 file saved with `WriteOptions::default().use_id3v23(true)`.
  - `TagWriteError { Unsupported(String), Symlink, AudioChanged, Verify(String), Io(String), Lofty(String) }`; `AudioChanged` displays `the tag writer changed the audio; nothing written`; `Symlink` displays M1's `SYMLINK_NOTE` (`organize/apply.rs`, made `pub(crate)`).
  - `refuse_symlink(root: &Path, rel: &str) -> Result<(), TagWriteError>` (Task 11's `symlinked`).

- [ ] **Step 1: Failing tests** (copies of ffmpeg tones in a temp dir; Picard tags and a cover via the existing helpers):
  - `mp3_v24_round_trip`: write every §19.8 field; re-read: each touched key equals the new value; the cover picture, a `TXXX:REPLAYGAIN_TRACK_GAIN` and a custom `TXXX:MY TAG` are unchanged; `audio_md5` equal before and after; `id3_version` still v4.
  - `mp3_v23_stays_v23` (R4: a v2.3 file written: still v2.3; `TYER`/`TDAT` and `TORY` present; multi-valued artist MBIDs read back joined by `/`).
  - `mp3_without_id3_gets_v24`.
  - `genre_multi_value_v24_v23` (v2.4 NUL-separated, v2.3 joined `/`).
  - `undo_values_restore_absent_keys` (write new, then write the old map with `[]` entries: the keys are gone again).
  - `forced_lofty_failure_leaves_original_byte_identical` (`fail_lofty`; whole-file SHA equal; no `.kima-tag-*` left).
  - `audio_changed_refused` (a test hook that truncates the copy's audio before the MD5: `AudioChanged`, original untouched).
  - `audio_md5_and_write_handle_odd_names` (Review Focus 3: `-start.mp3`, `a:b.mp3`, a non-UTF-8 name on Linux, folder `It's Here/`).
  - `wma_and_wav_unsupported_message`; `symlinked_file_refused`.
  - `path_held_in_ignore_set_until_grace`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test tagwrite`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same and `cargo clippy --workspace --all-targets -- -D warnings`. Expected: pass.
- [ ] **Step 5: Commit** `feat(library): safe Picard-compatible tag writing for MP3, with an audio check`. Stage `docs/DIVERGENCES.md`.

### Task 13: `tagwrite`: Vorbis comments (FLAC, Ogg, Opus) and MP4

**Files:**
- Create: `crates/kima-library/src/tagwrite/{vorbis.rs, mp4.rs}`
- Modify: `crates/kima-library/src/tagwrite/mod.rs`, `crates/kima-library/tests/tagwrite.rs`
- Test: `crates/kima-library/tests/tagwrite.rs`

**Divergences:** none new (D-120's clause covers every format).

**Interfaces:**
- Consumes: Task 12's `native`, `read_native`, `safe_write`, `TagValues`.
- Produces: the Vorbis and MP4 arms of `native`/`read_native`/`safe_write` per the §19.8 table: Vorbis one field per value, `TRACKTOTAL`/`DISCTOTAL` always and `TOTALTRACKS`/`TOTALDISCS` only when already present; MP4 `trkn`/`disk` as `(n, t)`, `©gen` joined `; `, freeform atoms one data atom per value, `cpil` boolean.

- [ ] **Step 1: Failing tests**, one per container (`flac`, `ogg`, `opus`, `m4a`), each the same shape as `mp3_v24_round_trip` (touched keys read back; cover art, ReplayGain and a custom field untouched; audio MD5 equal), plus `vorbis_totaltracks_only_when_present` and `mp4_freeform_multi_value`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test tagwrite`. Expected: the new tests fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p kima-library`. Expected: pass.
- [ ] **Step 5: Commit** `feat(library): tag writing for FLAC, Ogg, Opus and MP4`.

### Task 14: Scanner: tag overrides, `reread_paths`, autotag album pinning

**Files:**
- Modify: `crates/kima-library/src/tags.rs`, `crates/kima-library/src/scan.rs` (`prepare`, `store`, `process_folder`, `refresh_touched_once`), `crates/kima-db/src/library.rs`, `crates/kima-db/src/autotag/mod.rs` (overrides reads)
- Test: `crates/kima-library/tests/scan.rs`

**Divergences:** D-119 extended: "a rescan keeps an autotagged album's tracks on its row while their tags agree" (part of "album rows updated in place").

**Interfaces:**
- Produces:
  - `tags::TagOverrides` (serde camelCase, every field optional): `albumArtist, artist, album, title, year, date, disc, track, compilation, recordingMbid, releaseTrackMbid, releaseMbid, releaseGroupMbid, artistMbids, albumArtistMbids, genres, discTotal, trackTotal`; `RawTags::apply_overrides(&mut self, o: &TagOverrides)` (a present field replaces the read one); `TagOverrides::from_applied(album: &AppliedAlbum, track: &AppliedTrack) -> TagOverrides` (the same scoping as `TagValues::from_applied`).
  - `kima_db::autotag::overrides_for_paths(pool, paths: &[String]) -> sqlx::Result<HashMap<String, serde_json::Value>>`.
  - `scan::ReadCause { Scan, TagWrite }`; `scan::reread_paths(ctx: &LibraryCtx, paths: &[String], cause: ReadCause) -> Result<ScanReport, ScanError>`: re-reads exactly those files (grouped per folder through `process_folder`, siblings as in `update_path`); with `TagWrite` the store neither reopens stages nor deletes `track_acoustid` (the audio is proven unchanged).
  - `prepare` merges the path's override over the file's tags before `resolve` (every read: reconcile, `update_path`, `reread_paths`).
  - `kima_db::library::AppliedIdentity { artist_key: String, title_key: String, year: Option<i32> }` and `autotag_identity(conn, album: Uuid) -> sqlx::Result<Option<AppliedIdentity>>` (Some only while `identity_source='autotag'`).
  - `kima_db::library::upsert_track_pinned(tx, t: &ResolvedTrack, album: Uuid) -> sqlx::Result<Upserted>`: the track upsert onto that album row; no artist or album upsert, the album row's year untouched.
  - `store`: when the path's current album has an applied identity and the derived `(names::key(album_artist), names::key(album))` equals it and the derived year equals the applied year or is `None`, it calls `upsert_track_pinned`; otherwise `upsert_track` as today.
  - `library::release_autotag_albums(conn, albums: &[Uuid]) -> sqlx::Result<u64>`: an album among them with `identity_source='autotag'` and no present track goes back to `scan` with `applied_identity = NULL`; called from the folder refresh and `refresh_albums_in`.

- [ ] **Step 1: Failing tests** (set an album's applied identity by SQL in the test, as Task 15 will):
  - `override_merged_on_every_read` (an override renames a track's title: reconcile, `update_path` and `reread_paths` all store the override).
  - `rescan_after_tag_write_keeps_album_and_stages` (Review Focus 1): a `done` fingerprint and `done` recording stage, `track_acoustid` present; rewrite the file's tags to the applied identity with a different mtime; `reread_paths(TagWrite)` then a full reconcile then an `update_path`: same album id, stages still `done`, fingerprint row kept, exactly one album row with that title.
  - `pinned_year_not_recomputed` (the applied year 2001; the folder's other files say 1999: the album row keeps 2001).
  - `disagreeing_track_leaves_row` and `album_returns_to_scan_when_no_track_agrees`.
  - `new_file_with_applied_identity_joins_row`.
  - `tag_write_cause_does_not_reopen` vs `scan_cause_reopens_changed_file`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test scan`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(library): scanner merges tag overrides and keeps autotagged albums on their row`. Stage `docs/DIVERGENCES.md`.

### Task 15: Album and artist identity on apply and undo (database)

**Files:**
- Create: `crates/kima-db/src/autotag/{runs.rs, identity.rs}`, `crates/kima-db/tests/autotag_identity.rs`
- Modify: `crates/kima-db/src/autotag/mod.rs`, `crates/kima-db/src/stages.rs`, `docs/DIVERGENCES.md` (D-119)
- Test: `crates/kima-db/tests/autotag_identity.rs`

**Divergences:** D-119 extended: "album rows updated in place with collisions refused (no merge)".

**Interfaces:**
- Produces (`kima_db::autotag`):
  - `Target { Files, Copy, Db }` (serde lowercase); `RunState` with the §6.8 values.
  - `NewRun { id: Uuid, album_id: Uuid, candidate_id: Uuid, applied: Applied, target: Target, reorganize: bool, created_by: Option<Uuid> }`; `RunRow` (all `autotag_runs` columns); `create_run(conn, r: &NewRun) -> sqlx::Result<RunRow>` (state `queued`; also sets the album `applying` and `chosen_candidate`); `get_run(pool, id)`; `transition_run(pool, id, from: &[RunState], to: RunState) -> sqlx::Result<bool>`; `finish_run(pool, id, state: RunState, summary: &Value, error: Option<&str>)`; `active_run(pool, album) -> sqlx::Result<Option<RunRow>>` (states `queued`, `writing`, `organizing`, `undoing`).
  - `IdentityKeys { artist_key: String, title_key: String }` (callers compute with `names::key`).
  - `ConflictAlbum { album_id: Uuid, artist: String, title: String, year: Option<i32> }` with `message()` = `Another album already has this identity: <artist> – <title> (<year>)`, the ` (<year>)` part omitted without a year.
  - `find_collision(conn, album: Uuid, keys: &IdentityKeys, year: Option<i32>) -> sqlx::Result<Option<ConflictAlbum>>`: another album row of the artist whose `name_key` is `keys.artist_key` with that `title_key` and `COALESCE(year,-1)`.
  - `capture_before(conn, album: Uuid) -> sqlx::Result<Value>`: `{title, titleKey, year, isCompilation, releaseMbid, releaseMbidSource, releaseGroupMbid, releaseGroupMbidSource, identitySource, appliedIdentity, artistId, artistName, artistKey, artistMbid, artistMbidSource, mbGenres:[name…], tracks:[{trackId, recordingMbid, recordingSource}]}` (ruling: undo also restores what apply changed in the database beyond §19.7's list).
  - `IdentityError { Collision(ConflictAlbum), AlbumGone, Db(sqlx::Error) }`.
  - `apply_identity(conn, run: &RunRow, keys: &IdentityKeys) -> Result<ArtistStep, IdentityError>`: locks album then artists (id order) then tracks; re-checks the collision under the locks; writes `album_before` (via `capture_before`) and the artist step onto the run; updates the album row in place (title, `title_key`, year, `is_compilation`, MBIDs with source `autotag` unless the current source is `user` (R2), `identity_source='autotag'`, `applied_identity`); the artist step (re-parent / rename in place when the old artist would have no other album, missing tracks included / create with enrichment `pending`) recorded as `artist_action`, `old_artist_id`, `new_artist_id`, `old_artist_name`; `artists.mbid` with source `autotag` when the album credit has a single artist and the source is not `user`; album genres `source='musicbrainz'` replaced by `applied.album.genres`; tracks: `recording_mbid` with `recording_source='autotag'` and `stages::close_recording_autotag`; `organizer::supersede_previewed`. With `applied.mbids_only` the identity fields and the artist step are skipped (`artist_action='none'`).
  - `ArtistStep { action: &'static str, old_artist_id: Option<Uuid>, new_artist_id: Option<Uuid>, old_artist_name: Option<String> }`.
  - `put_overrides(conn, run_id, overrides: &[(Uuid, Value)]) -> sqlx::Result<()>` and `delete_overrides(conn, run_id) -> sqlx::Result<u64>`.
  - `undo_collision(conn, run: &RunRow) -> sqlx::Result<Option<ConflictAlbum>>` (the old identity from `album_before`, against the artist row the undo would use).
  - `undo_identity(conn, run: &RunRow) -> Result<(), IdentityError>`: restores the album row and `album_before`'s MBIDs, sources, genres, artist MBID and each track's `recording_mbid`/`recording_source` (a track that had none gets its `recording` stage back to `pending`); reverses the artist step (§19.7 "Undo, in reverse"); deletes the run's overrides; sets the album `proposed`.
  - `kima_db::stages::close_recording_autotag(conn, track: Uuid) -> sqlx::Result<()>`: upsert `recording` `done`, `source='autotag'`, `evidence={"reason":"autotag"}`, errors cleared, `finished_at = now()`.
  - Callers prune empty artists with the existing `library::prune_empty` after the commit.

- [ ] **Step 1: Failing tests** (§24.3 R1/R2 cases, all database-only):
  - `album_id_stable_and_user_state_survives` (a star, a `display_title`, a candidate and a playlist item survive apply and undo; the album id never changes).
  - `artist_reparented_to_existing`, `artist_renamed_in_place_when_only_album` (the artist keeps its id, bio and image path; `old_artist_name` stored), `artist_created_when_old_has_other_albums`; each undone back to the original rows.
  - `collision_refused_under_lock` (another album already holds the new identity: `Collision` with the message and id, nothing changed).
  - `undo_refused_when_old_identity_taken`.
  - `user_mbid_kept` (R2: a `user` release MBID survives; precedence user > autotag > tag on a later `refresh_album_mbids`).
  - `recording_stage_closed_and_restored`.
  - `mbids_only_leaves_identity`.
  - `previewed_organizer_run_superseded`.
  - `overrides_put_and_deleted`.
- [ ] **Step 2: Run** `cargo test -p kima-db --test autotag_identity`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(db): autotag album identity applied in place and undone`. Stage `docs/DIVERGENCES.md`.

### Task 16: The apply job: files and database targets, journal, crash recovery

**Files:**
- Create: `crates/kima-library/src/autotag/{apply.rs, capability.rs, journal.rs}`, `crates/kima-db/src/autotag/journal.rs`, `crates/kima-library/tests/autotag_apply.rs`
- Modify: `crates/kima-library/src/autotag/mod.rs`, `crates/kima-library/src/jobs.rs` (`LibraryJob`), `crates/kima-server/src/jobs.rs` (`prepare_library_queue` resumes autotag runs), `docs/DIVERGENCES.md` (D-120)
- Test: `crates/kima-library/tests/autotag_apply.rs` (fixtures: a 3-track FLAC album and an MP3 album; the run is created with `create_run` from an `Applied` value the test builds directly, since `kima-library` has no `kima-ident` to build it)

**Divergences:** D-120 extended: "in files mode only to a writable library, never otherwise; database-only mode writes no file; journalled, undoable; the read-only default stays read-only".

**Interfaces:**
- Consumes: Tasks 12–15.
- Produces:
  - `LibraryJob::AutotagApply { run_id }` (`#[serde(rename = "autotag-apply")]`) and `AutotagUndo { run_id }` (`"autotag-undo"`), payload key `runId`; `kima_library::autotag::{enqueue_apply(pool, run_id) -> sqlx::Result<Uuid>, enqueue_undo(pool, run_id) -> sqlx::Result<Uuid>}`.
  - Message constants: `FILES_READ_ONLY = "The library is mounted read-only. Mount it writable, or use copy or database-only."`, `COPY_FIRST = "Copy the library first (Settings > Organize, copy mode), then tag the copy."`, `TRACKS_CHANGED = "The album's tracks changed since the match was computed; search again"`, `CHANGED_OUTSIDE = "changed outside kima-rs"`.
  - `capability::Availability { available: bool, reason: Option<String> }`; `Capabilities { files: Availability, copy: Availability, copy_run_id: Option<Uuid> }`; `capabilities(ctx: &LibraryCtx) -> Capabilities` (files: `apply::writable(root)`; copy: Task 17); `album_refusal(ctx, album: &LocalAlbum, target: Target) -> Option<String>` (files/copy: the joined unsupported-format list, or `SYMLINK_NOTE`; copy: Task 17).
  - `kima_db::autotag::journal::{NewJournalRow, JournalRow, insert_planned(conn, run, rows) -> sqlx::Result<Vec<i64>>, mark_written(conn, row_id, after_sha: &[u8]) -> sqlx::Result<()>` (sets `applied_seq = nextval('autotag_applied_seq')`), `set_row_state(pool, row_id, state, note) , rows(pool, run_id) , written_desc(pool, run_id)}`.
  - `apply::run_apply(ctx: &LibraryCtx, run_id: Uuid, hooks: &ApplyHooks, cancel: &CancellationToken) -> Result<(), AutotagError>` with `ApplyHooks { write: WriteHooks, stop_after_files: Option<usize> }` and `AutotagError { Refused(String), Interrupted, Db(sqlx::Error), Io(String) }` (`Refused` → `JobError::Permanent`, recorded on the run as `failed` with the message; others → `Retry`). Steps exactly as spec §19.9 "Apply" 1–4 and 6 (step 5, reorganize, in Task 17); the run moves `queued → writing`, `album_refusal`, the track-set check (mapping ids + extra ids = the album's present track ids), and `find_collision` all run before the journal; per file: `safe_write`, then one transaction `mark_written`; `cancel` checked between files; then one transaction `apply_identity` (+ `put_overrides` for `db`), then `prune_empty`, then `reread_paths(TagWrite)` for files, `reread_paths(Scan)` for `db`; album `applied`, run `done`, summary `{written, skipped, failed, organized}`. A row whose file's whole-file SHA no longer equals `before_sha` at write time is `failed` `changed outside kima-rs`. Events `{job:"apply", runId, albumId, done, total, state}`.
  - `apply::resume_interrupted(pool) -> sqlx::Result<usize>`: re-queues runs in `writing`/`organizing`/`undoing` with no pending job; the job itself, on finding `planned` rows, applies the §19.9 crash-recovery rules (delete leftover `.kima-tag-<id>.*`, redo at `before_sha`, mark `written` when touched keys already equal `new_tags`, else `failed` `changed outside kima-rs`).

- [ ] **Step 1: Failing tests:**
  - `files_apply_writes_and_reorganizes_nothing`: every touched tag reads back; `audio_md5` equal; the album row updated in place; stages not reopened; run `done` summary `{written:3,…}`.
  - `files_refused_on_read_only_root` (chmod'ed temp root: `failed` with `FILES_READ_ONLY`, no `.kima-tag-*`, file SHAs unchanged).
  - `files_refused_for_wma_album` (an album with a `.wav` member: the joined unsupported message; `db` for the same album works).
  - `db_apply_writes_no_file_and_survives_rescan` (whole-file SHAs unchanged; overrides present; a forced reconcile keeps the applied values).
  - `apply_refused_when_tracks_changed` (Review Focus 2: a file added after the candidate: `TRACKS_CHANGED`, nothing touched).
  - `file_changed_after_planning_fails_row_only` (Review Focus 2: a hook rewrites one file between journal and write: that row `failed` `changed outside kima-rs`, the others `written`).
  - `collision_refused_in_job_before_files`.
  - `crash_after_rename_recovered_as_written` and `leftover_tag_copies_deleted_on_resume` (Review Focus 5, `stop_after_rename` then `run_apply` again).
  - `planned_row_at_before_sha_is_redone`.
  - `progress_events_carry_run_id`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test autotag_apply`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(library): autotag apply with journalled tag writes and crash recovery`. Stage `docs/DIVERGENCES.md`.

### Task 17: Apply: the copy target, the reorganize chain, switch-root clears overrides

**Files:**
- Modify: `crates/kima-library/src/autotag/{apply.rs, capability.rs}`, `crates/kima-library/src/organize/apply.rs` (a public `preview_then_apply`; `switch_root`), `crates/kima-db/src/organizer.rs` (`latest_copy_run`, `copy_rows_for_tracks`), `crates/kima-db/src/autotag/mod.rs` (`delete_copy_overrides`), `docs/DIVERGENCES.md` (D-120)
- Test: `crates/kima-library/tests/autotag_apply.rs`

**Divergences:** D-120 extended: "in copy mode only to organized copies".

**Interfaces:**
- Produces:
  - `organizer::latest_copy_run(pool) -> sqlx::Result<Option<Run>>`: mode `copy`, state `done`, `switched_at` NULL, `album_ids` NULL, newest.
  - `organizer::copy_rows_for_tracks(pool, run_id, tracks: &[Uuid]) -> sqlx::Result<HashMap<Uuid, String>>` (`done` rows' `to_path`).
  - `capabilities` copy: available when `latest_copy_run` exists and its `target_root` is writable; `copy_run_id` = its id; else `COPY_FIRST`. `album_refusal(.., Copy)` also requires a `done` row for every present track of the album (else `COPY_FIRST`).
  - Copy target in `run_apply`: `autotag_runs.copy_run_id` set; journal rows `root='copy'`, `path` = the copy row's `to_path`; writes only under that run's `target_root`; then the same identity transaction as `db` (overrides, so the web app shows the match); `reread_paths(Scan)`.
  - Reorganize (files only, spec §19.9 step 5): `create_scoped_run(mode move, album_ids [album], origin 'autotag')`, run state `organizing`, `organize::apply::preview_then_apply(ctx, run_id, cancel) -> Result<(), OrganizeError>`, `organizer_run_id` stored; `summary.organized` = the organizer run's moved count. `reorganize` with target `copy`/`db` is refused at request time (Task 21).
  - `switch_root` (organizer) deletes every `track_tag_overrides` row of runs whose `copy_run_id` is the switched run, in the switch transaction: `autotag::delete_copy_overrides(conn, copy_run: Uuid) -> sqlx::Result<u64>`.

- [ ] **Step 1: Failing tests:**
  - `copy_refused_without_done_copy_run` (`COPY_FIRST`).
  - `copy_never_opens_an_original_for_writing` (originals' mtimes and SHAs unchanged; copies carry the tags; overrides present).
  - `copy_refused_when_album_not_fully_copied`.
  - `reorganize_chains_scoped_move` (the album's files end at their organized paths; `organizer_run_id` set; another album's files untouched).
  - `switch_root_deletes_copy_overrides`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test autotag_apply`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(library): autotag copy target and the reorganize chain`. Stage `docs/DIVERGENCES.md`.

### Task 18: The undo job

**Files:**
- Create: `crates/kima-library/src/autotag/undo.rs`
- Modify: `crates/kima-library/src/autotag/mod.rs`, `crates/kima-library/src/jobs.rs`, `docs/DIVERGENCES.md` (D-120)
- Test: `crates/kima-library/tests/autotag_apply.rs`

**Divergences:** D-120 extended ("undoable" with the order of §19.9: organizer undo first, tags back, identity restored).

**Interfaces:**
- Produces: `undo::run_undo(ctx: &LibraryCtx, run_id: Uuid, hooks: &ApplyHooks, cancel: &CancellationToken) -> Result<(), AutotagError>`: spec §19.9 "Undo" 1–4. Before anything: `undo_collision` (refused → run back to its earlier state with the message, nothing touched). Run `done`/`failed` → `undoing`. Organizer run `done` → organizer undo through the existing undo path first. `written` rows in reverse `applied_seq`: file must hash to `after_sha` (else `undo_failed` `changed since the tag write`), write `old_tags` with the recorded `id3_version` through `safe_write`, re-read to check tag values, `undone`. Then one transaction `undo_identity` (+ `prune_empty` after), then `reread_paths(TagWrite)` (files) or `(Scan)` (db/copy). Run `undone`, or `undo_failed` if any row failed. Copy-target rows undo before the organizer's copy run is ever undone (the review page says so; nothing extra here).

- [ ] **Step 1: Failing tests:**
  - `undo_restores_every_tag_and_path` (files + reorganize: paths back, every touched key equals its old value, absent stays absent, `audio_md5` equal throughout, album row restored with the same id).
  - `undo_v23_returns_v23` (R4).
  - `undo_db_removes_overrides_and_rescan_shows_originals`.
  - `undo_refused_when_old_identity_taken_touches_nothing`.
  - `undo_row_changed_since_write_fails_that_row` (the run ends `undo_failed`, other rows `undone`).
  - `undo_of_artist_rename_restores_name`.
- [ ] **Step 2: Run** `cargo test -p kima-library --test autotag_apply`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(library): autotag undo`. Stage `docs/DIVERGENCES.md`.

### Task 19: Starting an apply, and auto-apply of strong matches

**Files:**
- Create: `crates/kima-ident/src/autotag/start.rs`, `crates/kima-ident/tests/autotag_start.rs`
- Modify: `crates/kima-ident/src/autotag/{mod.rs, search.rs}`, `docs/DIVERGENCES.md` (D-119)
- Test: `crates/kima-ident/tests/autotag_start.rs`

**Divergences:** D-119 extended: "optional auto-apply of strong matches (off by default)".

**Interfaces:**
- Consumes: Tasks 7, 15, 16, 17; `KvSettings::autotag_auto_apply`.
- Produces:
  - `ApplyRequest { album_id: Uuid, candidate_id: Uuid, target: Target, reorganize: bool, mbids_only: bool, created_by: Option<Uuid> }`.
  - `StartError { NotFound, Invalid(String), Unavailable(String), Busy, Collision(ConflictAlbum), Db(sqlx::Error) }`.
  - `start(pool: &PgPool, lib: &LibraryCtx, req: &ApplyRequest) -> Result<Started, StartError>` with `Started { run_id: Uuid, job_id: Uuid }`: the candidate must belong to the album (`NotFound`); `mbids_only` forces target `db`; `reorganize` only with `files` (else `Invalid("reorganize needs the files target")`); `capabilities`/`album_refusal` (`Unavailable(reason)`); `active_run` or an organizer run in `planning`/`applying`/`undoing` → `Busy`; parse the candidate's `release`/`release_group` snapshots and `mapping` into `Release`/`ReleaseGroup`/`Mapping`, build `proposal::applied` (+ `mbids_only` with `with_local_identity`); unless `mbids_only`, `find_collision` on the new identity (`Collision`); `create_run`, then `enqueue_apply` (an enqueue error marks the run `failed` with it and returns `Db`).
  - End of `run_search`: when the stored recommendation is `strong` and the setting is `Db`, `start` with target `db`; when `Files`, only if `capabilities.files.available` and `album_refusal(.., Files)` is `None`, target `files`; never `reorganize`; `created_by` NULL; any `StartError` is logged at `info` and leaves the album `proposed`.

- [ ] **Step 1: Failing tests:**
  - `start_creates_run_with_applied_values` (the run's `applied` equals `proposal::applied` of the snapshot).
  - `start_refusals` (wrong album → `NotFound`; reorganize with db → `Invalid`; read-only root with files → `Unavailable(FILES_READ_ONLY)`; a queued run → `Busy`; another album with the identity → `Collision`).
  - `apply_runs_with_lookups_off_and_dead_mb` (R3: `IdentifySwitch` off, `Net` bases at a dead port, `KIMA_NET_DENY_REAL=1`: the queued apply job completes `done`).
  - `auto_apply_off_by_default`, `auto_apply_db_on_strong_only` (a medium match is not applied), `auto_apply_files_only_when_available`.
- [ ] **Step 2: Run** `cargo test -p kima-ident --test autotag_start`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(ident): start autotag applies; optional auto-apply of strong matches`. Stage `docs/DIVERGENCES.md`.

### Task 20: Autotag API: read routes

**Files:**
- Create: `crates/kima-api/src/routes/library/autotag.rs`, `crates/kima-api/tests/autotag.rs`
- Modify: `crates/kima-api/src/routes/library/mod.rs`, `crates/kima-db/src/autotag/mod.rs` (list queries), `crates/kima-db/src/attention.rs` (`report_for`)
- Test: `crates/kima-api/tests/autotag.rs`

**Divergences:** none (kima-rs routes; D-119 covers the feature).

**Interfaces:**
- Consumes: Tasks 1, 6, 7, 15, 16, 17.
- Produces (all `AdminUser`; ids parsed with `parse_id`, a non-UUID answers 404; shapes exactly spec §16.5):
  - `GET /library/autotag/capabilities` → `{identifyEnabled, files:{available, reason}, copy:{available, reason, copyRunId}, db:{available:true}}`.
  - `GET /library/autotag/albums?state&recommendation&q&offset&limit` (limit default 50, max 200): albums with a present track; `state:"new"` for albums with no `autotag_albums` row (and the `state=new` filter selects them); `q` matches artist or album case-insensitively (`ILIKE` on unaccented text through the existing search helpers); `distance` and `releaseMbid` from the best candidate; `issues` from `attention::report_for(pool, album_ids) -> sqlx::Result<HashMap<Uuid, Vec<&'static str>>>` (the attention analysis over the tracks of the folders those albums live in, answered for those albums only). DB: `kima_db::autotag::{AlbumFilter, AlbumListRow, list_albums(pool, f: &AlbumFilter, offset, limit) -> sqlx::Result<(Vec<AlbumListRow>, i64)>}`.
  - `GET /library/autotag/albums/{albumId}?candidateId` → the §16.5 shape; `candidates` ordered by `score::order`; `stale` = `fetched_at` older than 30 days; `selected` = the given candidate, else `chosen_candidate`, else `best_candidate`; `changes` from `proposal::changes`; `conflict` from `find_collision` for the selected candidate's identity; a `userMbidKept: bool` flag beside `selected` (R2). 404 `{"error":"Album not found"}`.
  - `GET /library/autotag/runs?offset&limit` (20) and `GET /library/autotag/runs/{id}?offset&limit` (200): rows with `changes:[{field, old, new}]` from `old_tags`/`new_tags`, `total`. 404 `{"error":"Run not found"}`.
  - `kima_db::autotag::{list_runs, run_rows}` for those.

- [ ] **Step 1: Failing tests:** `capabilities_shapes` (read-only root → files unavailable with `FILES_READ_ONLY`; no copy run → `COPY_FIRST`), `albums_list_new_state_and_filters`, `albums_list_paging_limit_capped`, `album_detail_shape` (every §16.5 key; `changes` hides nothing), `album_detail_conflict_named`, `album_detail_stale_after_30_days`, `runs_list_and_detail`, `non_admin_403`, `non_uuid_404`.
- [ ] **Step 2: Run** `cargo test -p kima-api --test autotag`. Expected: fail.
- [ ] **Step 3: Implement**, then `cargo sqlx prepare --workspace -- --all-targets`.
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(api): autotag capabilities, album list and review data, runs`.

### Task 21: Autotag API: search, apply, skip, undo

**Files:**
- Modify: `crates/kima-api/src/routes/library/autotag.rs`, `crates/kima-api/tests/autotag.rs`, `docs/DIVERGENCES.md` (D-119)
- Test: `crates/kima-api/tests/autotag.rs`

**Divergences:** D-119 extended: "admin review page" (its API).

**Interfaces:**
- Consumes: Tasks 8, 18, 19.
- Produces (admin):
  - `POST /library/autotag/search` `{albumIds?, all?, query?:{artist, album}, releaseMbid?}` → 202 `{queued}`. 400 `{"error":"Give albumIds or all"}` when neither; 400 `{"error":"query and releaseMbid need exactly one album"}`; 400 `{"error":"Not a MusicBrainz release id or URL"}` for a bad `releaseMbid`; identification off → 409 `{"error":"Outbound lookups are turned off"}`; `all` = `unmatched_albums`.
  - `POST /library/autotag/albums/{albumId}/apply` `{candidateId, target, reorganize?, mbidsOnly?}` → 202 `{runId, jobId}`; `StartError` maps to 404 (`NotFound`), 400 `{error}` (`Invalid`, `Unavailable` with the capability reason), 409 `{"error":"An apply or organizer run is already in progress"}` (`Busy`), 409 `{error, conflictAlbumId}` (`Collision`).
  - `POST /library/autotag/albums/{albumId}/skip` → `{state:"skipped"}`.
  - `POST /library/autotag/runs/{id}/undo` → 202 `{jobId}`; 409 `{"error":"Only a finished run can be undone"}` unless `done`, or `failed` with written rows; 409 `{error, conflictAlbumId}` when `undo_collision` finds the old identity taken.
  - Errors add `conflictAlbumId` through a new `ApiError::ConflictWith { message: String, extra: serde_json::Value }` variant (409, body = `{"error": message}` merged with `extra`).

- [ ] **Step 1: Failing tests:** `search_queues_and_sets_state`, `search_all_unmatched`, `search_validation_400s`, `search_refused_when_identify_off`, `apply_202_then_job_done_db_target` (runs the library job in-test), `apply_409_collision_with_album_id`, `apply_400_unavailable_reason`, `apply_409_busy`, `skip_sets_state`, `undo_202_and_409s`.
- [ ] **Step 2: Run** `cargo test -p kima-api --test autotag`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test --workspace`. Expected: pass.
- [ ] **Step 5: Commit** `feat(api): autotag search, apply, skip and undo`. Stage `docs/DIVERGENCES.md`.

### Task 22: Frontend: grouped preview, organizer, needs attention, Library Tools

**Files:**
- Create: `frontend/features/organizer/components/AlbumGroupedPreview.tsx`, `frontend/features/organizer/__tests__/AlbumGroupedPreview.test.tsx`
- Modify: `frontend/app/settings/organize/page.tsx` (uses `AlbumGroupedPreview` and the `/albums` paging; shows `superseded` as "Out of date — preview again"), `frontend/features/organizer/{types.ts, filters.ts}` (`ISSUE_LABELS.symlink = "Symlinked files"`; `OrganizerRun.state` gains `superseded`), `frontend/app/settings/attention/page.tsx` ("Match" button per album → `/settings/autotag/album/?albumId=<id>`), `frontend/features/settings/components/sections/LibraryToolsSection.tsx` (third link "Match albums" → `/settings/autotag`), `frontend/lib/api.ts` (`getOrganizerRunAlbums`, `previewOrganizer(mode, albumIds?)`); delete `PreviewTable.tsx` once nothing imports it
- Test: the new test file, `frontend/features/organizer/__tests__/organizer.test.tsx`

**Divergences:** none (D-121 already names "preview grouped by album").

**Interfaces:**
- Consumes: Task 9's `GET /library/organizer/runs/{id}/albums`, Task 11's `symlink` code.
- Produces: `AlbumGroupedPreview(props: { groups: OrganizerAlbumGroup[]; page: number; total: number; onPage: (n: number) => void })` with collapsible album headers (row count, issue chips), the existing filters inside each group, shared with Task 23. `OrganizerAlbumGroup = { albumId: string | null; artist: string | null; album: string | null; rows: OrganizerRow[] }`.

- [ ] **Step 1: Failing unit tests:** `groups rows under album headers with counts`, `collapses and expands a group`, `sidecar group without album is labelled "Other files"`, `attention shows Symlinked files label and a Match link`, `library tools has a Match albums link`.
- [ ] **Step 2: Run** `npm run test:unit` in `frontend/`. Expected: fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `npx tsc --noEmit && npm run test:unit && npm run build`. Expected: pass; the existing organizer Playwright steps in `library.spec.ts` still pass against a local kima (`KIMA_IDENTIFY=off`).
- [ ] **Step 5: Commit** `feat(frontend): organizer preview grouped by album; symlink label; Match albums entry points`.

### Task 23: Frontend: Match albums pages and the e2e flow

**Files:**
- Create: `frontend/app/settings/autotag/page.tsx`, `frontend/app/settings/autotag/album/page.tsx`, `frontend/features/autotag/{types.ts, components/CandidateList.tsx, components/ChangesTable.tsx, components/MappingDiff.tsx, components/TargetPicker.tsx, progress.ts}`, `frontend/features/autotag/__tests__/{ChangesTable.test.tsx, MappingDiff.test.tsx, TargetPicker.test.tsx}`, `frontend/tests/e2e/kima-rs/autotag.spec.ts`, `tools/seed-autotag-candidate.sql`
- Modify: `frontend/lib/api.ts` (every §16.5 call), `frontend/hooks/useEventSource.ts` (`autotag:progress` → invalidate `["autotag-albums"]`, `["autotag-album", albumId]`, `["autotag-runs"]`), `.github/workflows/ci.yml` (e2e: after kima is healthy and the first scan is done, `psql "$DATABASE_URL" -f tools/seed-autotag-candidate.sql`), `docs/DIVERGENCES.md` (D-122)
- Test: the files above

**Divergences:** D-122 extended: "New Settings > Library Tools > Match albums pages in the Next.js frontend".

**Interfaces:**
- Consumes: Tasks 20–22.
- Produces (spec §21.2 copy): list page with state/recommendation/text filters, selection, "Find matches", "Match all unmatched", a capability banner, live progress, recent runs with Undo; review page with `CandidateList` (distance bar, recommendation chip, origin, penalties on hover), `ChangesTable` (unchanged fields hidden by default with a "Show unchanged" toggle), `MappingDiff` (missing and extra rows highlighted), search box (artist/album or MBID/URL), `TargetPicker` (unavailable targets disabled with their reason), "Re-organize after writing" (files only), Accept / Skip; on a conflict: the album named and linked, Accept disabled, "Apply MBIDs only", "Merge (later)" disabled; "Refresh candidates" when `stale`; a note when `userMbidKept`; with target `copy` the note "Undo this tag write before undoing the copy run" (spec §26); a `reorganize` run shows its planned moves through `AlbumGroupedPreview`.
- `tools/seed-autotag-candidate.sql`: inserts one `autotag_albums` row (`proposed`, `strong`) and one candidate for the fixture album "Sleater-Kinney – The Woods" (2 tracks from `tools/make-fixture-library.sh`), whose release title is `The Woods (Remastered)`; the snapshot JSON is hand-written in the Task 4 shape and the mapping pairs are built from the two track ids found by path.

- [ ] **Step 1: Failing unit tests:** `ChangesTable hides unchanged fields by default`, `ChangesTable shows them on toggle`, `MappingDiff highlights missing and extra rows`, `TargetPicker disables unavailable targets with the reason`, `review page disables Accept and offers Apply MBIDs only on conflict`.
- [ ] **Step 2: Write the Playwright spec** `autotag.spec.ts` (spec §21.3): sign in as admin; Settings > Library Tools > Match albums; open "The Woods"; see "strong"; choose "Database only"; Accept; wait for the run to finish; the album page shows `The Woods (Remastered)`; back on Match albums, Undo the run; the album page shows `The Woods`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `npx tsc --noEmit && npm run test:unit && npm run build`, then the seed and `npx playwright test -c playwright.kima-rs.config.ts` against a local kima with `KIMA_IDENTIFY=off`. Expected: all pass.
- [ ] **Step 5: Commit** `feat(frontend): Match albums list and review pages`. Stage `docs/DIVERGENCES.md`.

### Task 24: M3b wrap-up

**Files:**
- Modify: `docs/DIVERGENCES.md` (verify only), `README.md`, `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`

**Divergences:** none new; verifies D-119–D-123 and the rows Tasks 2, 3 and 14 extended.

- [ ] **Step 1: Verify the divergence log.** Do not add rows here.
  - D-119, D-120, D-121, D-123 present and complete against spec §23 plus this plan's clauses; D-122 carries both the M3a clauses and the Match albums pages; status `open`.
  - D-113 (cover deferrals column), D-126 (wipe keeps autotag answers), D-130, D-131 extended; notes on D-068, D-069, D-070.
  - Any row a task added from D-134 on is sequential.
- [ ] **Step 2: README.** An "Autotagger" section: what Match albums does, the three targets and when each is available (the read-only default mount gives `db` and, after a copy run, `copy`), auto-apply (`autotagAutoApply`, off by default), undo, and that tags are written only where the library is writable. Organizer: album-scoped runs, `.lrc`/`.nfo` sidecars, symlinked tracks listed in needs-attention.
- [ ] **Step 3: Full verification:**
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `SQLX_OFFLINE=true cargo build --workspace`
  - `KIMA_NET_DENY_REAL=1 cargo test --workspace`, with ffmpeg present
  - in `frontend/`: `npx tsc --noEmit`, `npm run test:unit`, `npm run build`
  - `docker build -t kima-rs:m3b .`; run the image with `KIMA_IDENTIFY=off` and the fixture library from `tools/make-fixture-library.sh`, seed with `tools/seed-autotag-candidate.sql`, and run `npx playwright test -c playwright.kima-rs.config.ts`
  - push the branch and check the GitHub CI run

  Expected: all green.
- [ ] **Step 4: Roadmap.** Set the M3 row's plan cell to `2026-10-19-m3a-identification.md` ✅ (M3a), `2026-10-27-m3b-autotagger.md` ✅ (M3b); remove the parked minors 2 and 5–8 and the autotagger from its notes.
- [ ] **Step 5: Commit** `docs: M3b wrap-up`.
