# M5b (mixes, radio, what's-new, recommendations) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** kima-rs serves upstream v1.12.3's mixes, radio, what's-new and recommendations backend: the 7 `routes/mixes.ts` endpoints under `/api/mixes`, the 10 `routes/whatsNew.ts` endpoints under `/api/whats-new`, `GET /api/recommendations/for-you`, and the 4 radio/decade endpoints of `routes/library/tracks.ts` (`/library/decades`, `/library/radio/stations`, `/library/radio/row`, `/library/radio`) — **22 endpoints** — plus the services and jobs behind them: the recipe engine (44 recipes, 13 mood stations + Wildcard), the 42 daily-mix generators, station play order, night plays, familiarity boost, mood-bucket mixes and the saved mood mix, the mood-tag writer, radio-cover picking (5-minute job), the mix-cache drop on a moods change, the release catalog with per-listener acknowledgements/choices, and the update check. At the end `GET /api/system/features` sends `"mixes": true`. Everything personal is keyed by `profile_id` (M-PROF).

**Architecture:** One new crate `kima-mixes` (pure parts: seeded draws, artist cap/spread, recipes/stations parsing, station order, mix assembly; DB parts behind `kima-db`), like `kima-vibe` holds the vibe formula. `kima-db` gets `mixes`, `radio`, `releases` modules and one migration (`0063_mixes.sql`). `kima-ident`'s moods phase writes `tracks.mood_tags` and its `subscribe_changes` event drops cached mixes. `kima-api` gets `routes/mixes.rs`, `routes/whats_new.rs`, `routes/recommendations.rs` and radio/decade handlers in `routes/library/radio.rs`. `kima-api-types` gets `mixes.rs`, `radio.rs`, `releases.rs` DTOs and fixtures. Redis state (mix lists, station sessions, familiarity scan, energy band, radio covers) has no Redis in kima-rs (D-003): see Q1.

**Tech Stack:** unchanged (Rust 2024, axum 0.8, sqlx 0.8, tokio, `md-5`/`sha2` already in the workspace for the seeded draws, `kima-net` for the update check). **No new third-party dependency** (`md-5` and `sha2` are already in `Cargo.lock` [V]). Test-only: `axum_test`, `#[sqlx::test]`, goldens generated from the pin's git objects by `node` (like `tools/vibe-goldens/upstream.mjs`).

**Specs / parents:** roadmap row M5b (`docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md:44`, "Mixes, radio and what's-new … | M5"); M5 spec hand-off `docs/superpowers/specs/2026-11-05-m5-vibe-design.md` §13 row **M5b** (mood_state readings and 22 heads for recipes, `moodTags` writer, `UserMoodMix` + `/api/mixes/mood/buckets/*`, `/whats-new/mood-mixes*`, radio row's `vibeEmbeddings` gate, `familiarity.ts`/`nightPlays.ts` via public `kima_db::vibe::{matching_context, top_scores, rank_matches}`, drop cached mixes on a moods change) and §4.4 degradation matrix; M-PROF rule (new personal tables use `profile_id` from creation, roadmap row M-PROF); M-SYNC Appendix B hand-off row M5b; divergence log `docs/DIVERGENCES.md` (highest row on this base: **D-254**).

**Upstream reference:** kima-hub tag `v1.12.3` = `c7527577d925b20f5adf410c7fd4e0c8ac250f69`. Full-history clone `~/.hermes/kima-rs-hub/kima-hub-upstream` (read with `git show c7527577:<path>`). External reference checkout (`KIMA_HUB_REFERENCE`, see `tools/prepare-kima-hub-reference.sh`) pulled `--ff-only` on 2026-11-19 at **`9450c94b`** (clean). `git diff --stat c7527577 9450c94b` over `backend/src/routes/{mixes,mixesValidation,whatsNew,recommendations}.ts`, `routes/library/tracks.ts`, `services/mixes/`, `services/moodBucketService.ts` and `backend/prisma/` is **empty**; only `services/releases/catalog.ts` changed (+17/−1: release text). So every line number below holds for both, except catalog text. Files read for this plan: the four route files in full, `routes/library/tracks.ts:1-60,540-688`, `services/mixes/{index,helpers,recipes,stationSelection,radioRow,stations,mixDay,timeMixes,cacheKeys}.ts` (full or the cited ranges), headers of every other `services/mixes/*.ts`, `services/moodBucketService.ts:15-35,270-472`, `services/releases/{catalog,index,moodMixes}.ts`, `services/updateCheck.ts` (exports), `workers/index.ts:180-205`, `workers/unifiedEnrichment.ts:1240-1275`, `prisma/schema.prisma` models `UserMoodMix`, `ReleaseAcknowledgement`, `ReleaseChoice`, `Playlist.mixId`, `Track.moodTags`, and `frontend/lib/api.ts` callers.

**Verification marks:** **[V]** = read in upstream or kima-rs source at the named file:line; **[C]** = from a comment/commit message, not traced in code; **[?]** = not verified, listed in Appendix C.

## Global Constraints

- All earlier milestones' constraints hold: GPL-3.0-only, edition 2024, camelCase JSON on `/api` with `{"error": "<message>"}`, UUIDv7 ids (D-016; upstream cuid), the database is never mocked, **no test reaches the network** (`KIMA_NET_DENY_REAL=1`; the update check's GitHub host is a loopback replay), commits authored **and** committed as `dimmu <3417783+d1mmu-dev@users.noreply.github.com>`, Conventional Commits, **no** `Co-Authored-By`/AI trailers, explicit `git add <paths>` only. Never push `main`; push `feature/m5b-mixes` only with an explicit refspec and only on the owner's go-ahead.
- **Faithful port.** Constants (`MIN_MIX_TRACKS` 8, `TRACK_LIMIT` 20, `MOOD_MIX_LIMIT` 15, `RECIPE_LINE` 1.25, `STEADY_BEAT` 2 (on Essentia's 0–5.32 scale; kima uses `2/5.32` on its 0–1 `tempo_confidence`, Q7 = A), `DAILY_MIX_COUNT` 5, `MIXES_CACHE_TTL` 3600 s, `MAX_CACHED_ZONES` 4, `SESSION_TTL_S` 12 h, `STATIONS_KEPT` 3, `REPLAY_PASSES` 1000, `REPLAY_SONGS` 200 000, `SEED_PATTERN`, `MIN_STATION_SONGS` 15, `MAX_GENRES` 20, `HOME_GENRES` 6, `HOME_DECADES` 4, `NIGHT_START_HOUR` 22, `NIGHT_END_HOUR` 4, `COLD_START_SONGS` 30, `FAMILIAR_*`, `NO_COVER_SECONDS` 600), error strings, status codes and response keys are upstream's. `recipes.json` and `moodDefinitions.json` are vendored **byte for byte** from the pin with a SHA-256 test. Every deliberate difference is a D-row in the **same commit** as the code; rows are amended, never deleted.
- **Divergence IDs:** M5b uses **D-260 – D-279**. Checked 2026-11-19: `docs/DIVERGENCES.md` on `0d28f90` ends at D-254 and `grep -c 'D-2[67][0-9]'` = 0; only two worktrees exist (`master`, `feature/m5b-mixes`, both at `0d28f90`). D-255–D-259 stay free as a buffer.
- **Migration numbers:** main ends at `0062_artist_enrichment_retry_at.sql`; no `0063`–`0069` on any branch (checked 2026-11-19 over every worktree and every local branch's `crates/kima-db/migrations`). M-PROF had reserved 0060–0069 but used only 0060; 0061/0062 came from post-M5 fixes. M5b takes **0063–0069**; Task 1 uses `0063_mixes.sql`; a schema fix is a new file, never an edit of an applied one. After a migration change `touch crates/kima-db/src/lib.rs`.
- **Profiles (M-PROF, binding):** upstream `userId` → kima `profile_id` everywhere a listener owns something: mix lists/caches, station sessions, familiarity scans, radio covers, `user_mood_mix`, `release_acknowledgements`, `release_choices` (Q9), saved mix playlists (`playlists.profile_id` + `mix_id`). Admin-only behaviour = the account's main profile (existing `AdminUser`). A test per personal table proves a sibling profile of the same account and another account see nothing (`profile_isolation`-style).
- **TDD:** each task's first step writes the failing tests named in it and runs them red for the expected reason, then implements the minimum, then runs green.
- **Gates before every commit:** `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings` (stable, with and without `--features kima-ml/pg-publish`); the task's test binaries on a **run-owned isolated Postgres** (parent-created `pgvector/pgvector:pg17`, `127.0.0.1:55474+`, URL in a mode-600 file, `SELECT current_database()` checked before each run, `KIMA_NET_DENY_REAL=1`; never the dev DB, never the live app's DB); after any `sqlx::query!` change `cargo sqlx prepare --workspace -- --all-targets` from the repo root and commit `.sqlx/` with it; `SQLX_OFFLINE=true cargo check --workspace --all-targets`; `tools/check-pin-refs.sh`; profile-key and podcast-key checks. Use `cargo test -p <crate> --test <binary>` (a name filter can silently match zero integration tests).
- **No Next.js work, no frontend in production paths.** The external checkout is read only (clean-tree + `pull --ff-only` + SHA recorded before each comparison, `tools/prepare-kima-hub-reference.sh`).

## Owner decisions (binding, do not re-ask)

1. M5b depends on M5 (merged and deployed); the vibe engine, `track_moods`/`mood_state`, CLAP readings and `moods::subscribe_changes` exist.
2. Mood heads stay **blocked** behind a capability that reports unavailable until their training-data licences are checked (M5 Q3); moods are `unanalyzed`. M5b must degrade (matrix below), never provision heads.
3. Personal data is per profile (M-PROF); profile names visible only within the account.
4. `/system/features` gates sections (D-030/D-170); a flag is flipped only when its routes are implemented.
5. kima-web pages for mixes/radio are **M10** per the roadmap (M10 row: "mixes/radio"), **except** the thin slice the owner pulled into M5b (Q10 B below: `/radio` page, home "Made for you" row, mix page = Phase F); MoodMixer, what's-new window, changelog page and the rest stay M10.

Answers to the plan's open questions (owner, **2026-11-19**; the full question text is kept under "Rozstrzygnięte" at the end):

6. **Q1 = A.** Listener state (mix lists, station sessions, familiarity scans, energy band, radio covers) lives in an in-process TTL store in `AppState`, lost on restart; D-260.
7. **Q2 = B.** The Workout and mood radio tiles are listed only while mood statistics are available (on top of upstream's `vibeEmbeddings` gate), so no tile plays an empty station while the heads are blocked; D-266.
8. **Q3 = B.** Upstream's release mechanism is ported exactly, but kima-rs ships its **own** catalog, empty at the start (`pending` → `{releases:[]}`, `current` → 404 `No release`, `changelog` → `{releases:[],openCount:0}`, `open` → `{count:0,items:[]}`); the `changelog` feature flag stays off until the first kima-rs entry exists; D-267.
9. **Q4 = B.** The update check asks the `d1mmu-dev/kima-rs-hub` GitHub releases through `kima-net` (rate limit, `KIMA_NET_DENY_REAL`), with upstream's on/off switch (`updateChecksEnabled`); D-270.
10. **Q5 = A.** No GPU choice: no catalog item asks `gpu.accelerate`, so `POST /whats-new/{version}/choice` with that key answers 400 `Unknown choice`; revisit with M12; D-268.
11. **Q6 = B (not the plan's recommendation).** Track rows in `GET /api/mixes/{id}`, `GET /api/mixes/mood/buckets/{mood}` and `POST /api/mixes/mood/buckets/{mood}/save` carry **every** scalar column upstream's Prisma `Track` returns, plus every scalar column of the included `album` and `album.artist{id,name,mbid}`, each mapped to a kima column or `null` where kima has no data. The column inventory and its mapping are Appendix D and D-264.
12. **Q7 = A.** Steady beat = linear rescale of upstream's `bpmConfidence ≥ 2` (Essentia scale 0–5.32) to kima's 0–1 `tempo_confidence`: `≥ 2/5.32 ≈ 0.376`; D-262. Follow-up (not M5b): calibrate on the owner's library (share of tracks passing vs upstream's typical share) once real-library access is approved.
13. **Q8 = A.** Missing `energy` and `danceability` are treated as absent, as upstream's null paths (artist-radio term skipped); no CLAP substitute; D-263. **Correction (owner 2026-11-20, review finding I2):** Q8's premise was wrong for the night energy band: upstream reads it from `Track.arousal`, not `energy`, and kima has that as the CLAP `arousal_raw` (D-264). The band is ported 1:1 (library percentiles 0.25/0.75 of CLAP arousal, 1/0/0.5), D-263's band half resolved.
14. **Q9 = A.** Release acknowledgements and choices are per profile; admin-only choices are made only by the account's main profile; D-269.
15. **Q10 = B.** Phase F runs in M5b: kima-web API client + `/radio` page, home "Made for you" row and `/mix?id=` page; the rest (MoodMixer, what's-new window, changelog page, artist radio button, for-you row) stays M10.
16. **Q11 = A.** The mix day is the server's calendar date, as upstream (`mixDay.ts:8-11`); deployment note: set `TZ=Europe/Warsaw` for the container/unit so mixes roll over at local midnight.

## Review Focus

1. **Determinism.** A mix's ids, seeds and draws must equal upstream's for the same inputs: `getSeededRandom` (JS UTF-16 code units, 32-bit `hash << 5` wrap, `Math.abs`), the LCG `(seed * 9301 + 49297) % 233280` over f64-exact integers, `seededKey` = MD5 hex of `"<seed>:<id>"` compared byte-wise (`COLLATE "C"` in SQL), `weightedOrder` = SHA-256 first 13 hex digits, `capByArtist` caps 2→3→∞. Proven by goldens from the pin, not by reading.
2. **Degradation is never a 500.** Mood heads absent ⇒ recipes return null and generators fall back; a station with nothing answers `{"tracks": []}`; presets answer `trackCount: 0` with `pendingTracks`.
3. **Profile isolation.** Every cache key, session, saved mix, playlist `mix_id` uniqueness and release row is per profile.
4. **Atomic saves.** `POST /mixes/{id}/save` creates playlist + items in one transaction; concurrent saves of one mix → one playlist (409 for the loser, as upstream's unique index); `/whats-new/mood-mixes/save` reuses the winner (upstream P2002 path).
5. **Router shape.** axum vs Express fall-through: `GET /api/mixes/refresh` must reach the `/:id` handler (404 "Mix not found"), not 405; `POST /api/whats-new/mood-mixes/choice` must reach `/:version/choice` (404 "Unknown release"). Test both.

---

## How upstream's models map onto kima-rs [V `backend/prisma/schema.prisma`]

| Upstream | kima-rs (`0063_mixes.sql`) | Note |
|---|---|---|
| `UserMoodMix` (`:877-890`): `id cuid`, `userId @unique` (cascade), `mood`, `trackIds String[]`, `coverUrls String[]`, `servedTrackIds String[]`, `generatedAt`, `createdAt`, `updatedAt` | `user_mood_mix(profile_id uuid PK REFERENCES profiles ON DELETE CASCADE, mood text NOT NULL, track_ids uuid[] NOT NULL, cover_urls text[] NOT NULL, served_track_ids uuid[] NOT NULL, generated_at timestamptz NOT NULL, created_at, updated_at)` | no surrogate id (never exposed); `track_ids` not FK-checked, re-read through `tracks_by_ids` like upstream |
| `Playlist.mixId String?`, `@@unique([userId, mixId])` (`:412-427`) | `ALTER TABLE playlists ADD COLUMN mix_id text; CREATE UNIQUE INDEX playlists_profile_mix_idx ON playlists (profile_id, mix_id) WHERE mix_id IS NOT NULL` | mix ids are strings like `era-1990-2026-11-19`, `your-mood-mix-<ms>` |
| `PlaylistItem.rank` (lexoRank, `utils/lexoRank.ts`) | kima `playlist_items.sort` only | saves write `sort = index` as the existing playlist code does; no rank column (existing playlists divergence covers it; cite the row in Task 9) |
| `ReleaseAcknowledgement` (`:1158-1172`): `userId`, `version`, `dismissedAt`; unique `(userId, version)` | `release_acknowledgements(profile_id, version text, dismissed_at, PRIMARY KEY (profile_id, version))` | per profile (Q9) |
| `ReleaseChoice` (`:1174-1190`): `userId`, `version`, `key`, `enabled bool default false`, `decidedBy ChoiceDecidedBy default DISMISSED`, `decidedAt`; unique `(userId, key)` | `release_choices(profile_id, key text, version text, enabled boolean NOT NULL DEFAULT false, decided_by text NOT NULL CHECK (decided_by IN ('EXPLICIT','DISMISSED')), decided_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (profile_id, key))` | enum as checked text (kima convention) |
| `Track.moodTags String[]` (`:702`) | `ALTER TABLE tracks ADD COLUMN mood_tags text[] NOT NULL DEFAULT '{}'` | written by the moods phase (Task 5); M5 spec §6 deferred it here |
| `SystemSettings.lastUpdateCheck`, `latestKnownVersion` (`updateCheck.ts:76-78`) | `settings_kv` keys `lastUpdateCheck`, `latestKnownVersion` | no migration |
| `SystemSettings.albumsRefiledAt`, `gpuOnnxEnabled` (`releases/index.ts`) | `settings_kv` `albumsRefiledAt` (written by the existing re-file job, check `kima_db::organizer`/refile); GPU: none (env `KIMA_ONNX_PROVIDER`, D-252) | Q5 |
| `Track.analysisStatus`, `bpm`, `bpmConfidence`, `key`, `keyScale`, `instrumentalness`, `arousal`, `energy`, `valence`, `danceability` | `track_stages(stage='analysis', status='done')`; `track_analysis.tempo_bpm`, `tempo_confidence` (0–1), `key_tonic`/`key_mode`; `track_clap_readings.instrumentalness`, `arousal_raw`/`valence_raw`; `energy`: none; `danceability`: `musicnn_danceability` (unprovisioned) | Q7, Q8; M5 spec §8 mapping |
| `Album.location = 'LIBRARY'`, `OwnedAlbum`, `Track.corrupt` | kima: every album is library (M7 adds location, M5 spec §13 M7 row); no `corrupt` column | `LIBRARY_PLAYABLE` = `tracks.missing_since IS NULL`; D-row in Task 2 |
| `SimilarArtist(fromArtistId,toArtistId,weight)` | `artists."similar"` jsonb `[{name, mbid, match}]` (0017) resolved to library artists by MBID then name, as Subsonic `getSimilarSongs` already does (D-190) | reuse that helper |
| `Artist.heroUrl` + Redis `hero:<id>` | `artists.image_path` → native cover token | as vibe map covers (D-193) |

## Upstream inventory: every endpoint [V]

Router mounts: `index.ts:155` `/api/whats-new` (`apiLimiter`), `:162` `/api/recommendations`, `:174` `/api/mixes`; library routes under `/api/library` (`routes/library/index.ts:17` `requireAuthOrToken`).

| # | Method + path | Upstream file:line | Auth | Service | Request | Response (200) / errors |
|---|---|---|---|---|---|---|
| 1 | `GET /api/mixes` | `routes/mixes.ts:111` | `requireAuthOrToken` (`:20`) | `getMixesForUser` (`:80-90`) → `programmaticPlaylistService.generateAllMixes` (`services/mixes/index.ts:57-308`), cache `mixes:<userId>` per zone (`:30-77`) | `?tz=` IANA zone (`:26-28`, `nightZone`) | `ProgrammaticMix[]` = `{id,type,name,description,trackIds,coverUrls,trackCount,color}` (`helpers.ts:8-17`), saved mood mix appended with `type:"mood", mood` (`index.ts:295-305`); 500 `Failed to get mixes` |
| 2 | `GET /api/mixes/mood/buckets/presets` | `mixes.ts:122` | same | `moodBucketService.getMoodPresets` (`moodBucketService.ts:283-305`) | — | `[{id,name,color,icon,trackCount,pendingTracks}]` (5 moods `:20-26`); 500 `Failed to get mood presets` |
| 3 | `GET /api/mixes/mood/buckets/:mood` | `mixes.ts:132` | same | `getRememberedTrackIds` → `getMoodMix(mood, 15, remembered)` → `recordServedTracks` (`moodBucketService.ts:307-350,439-470`); recipe `pickPageTracks` (`recipes.ts:578-588`) | path mood ∈ `happy,sad,chill,aggressive,party` | `{id:"mood-<mood>-<ms>",mood,name:"<Name> Mix",description,trackIds,coverUrls,trackCount,color,tracks:[full track rows + album.artist{id,name,mbid}]}`; 400 `{error:"Invalid mood: x", validMoods}`; 400 `{error:"Not enough tracks for mood: x", suggestion:"Wait for more tracks to be analyzed"}`; 500 `Failed to get mood mix` |
| 4 | `POST /api/mixes/mood/buckets/:mood/save` | `mixes.ts:175` | same | `validateSaveTrackIds` (`mixesValidation.ts:15-28`) → `saveUserMoodMix` (`moodBucketService.ts:352-395`) → drop mixes cache | body `{trackIds?: string[] ≤ 15}` | `{success:true, mix:{id:"your-mood-mix-<ms>",mood,name:"Your <Name> Mix",description,trackIds,coverUrls,trackCount,color,generatedAt,tracks}}`; 400 invalid mood / `trackIds must be an array` / `Each trackId must be a string` / `trackIds must not exceed 15 tracks` / `None of those songs can be saved: each is missing, corrupt or not in the library` / not enough tracks; 500 `Failed to save mood mix` |
| 5 | `POST /api/mixes/refresh` | `mixes.ts:236` | same | `generateAllMixes(…, forceRandom=true)` + `cacheMixes` | `?tz=` | `{message:"Mixes refreshed", mixes}`; 500 `Failed to refresh mixes` |
| 6 | `POST /api/mixes/:id/save` | `mixes.ts:257` | same | `getMixesForUser` → existing playlist check → one `playlist.create` with items (`:303-319`) → `announcePlaylistChange` | `?tz=`, body `{name?}` | `{id,name,trackCount}`; 404 `Mix not found`; 409 `{error:"Mix already saved as playlist",playlistId,name}`; 422 `None of this mix's songs are in the library any more`; 500 `Failed to save mix as playlist` |
| 7 | `GET /api/mixes/:id` | `mixes.ts:337` | same | `getMixesForUser` → `loadTracksInMixOrder` (`:93-109`, `LIBRARY_PLAYABLE`) | `?tz=` | `{...mix, tracks}`; 404 `Mix not found`; 500 `Failed to get mix` |
| 8 | `GET /api/whats-new/pending` | `routes/whatsNew.ts:35` | `requireAuth` (`:32`) | `pendingReleases` (`releases/index.ts:48-66`) | — | `{releases: Release[]}` (newest only, until acknowledged; refile item dropped once `albumsRefiledAt`); 500 `Failed to load releases` |
| 9 | `GET /api/whats-new/update-status` | `whatsNew.ts:45` | same | `services/updateCheck.ts` `getUpdateStatus` (GitHub `releases/latest`, 10 s, `inFlight` single-flight, `settings` cache) | — | `{enabled,current,latest,hasUpdate,lastCheckedAt}`; 500 `Failed to check for updates` |
| 10 | `GET /api/whats-new/current` | `whatsNew.ts:56` | same | `currentReleaseFor` (`releases/index.ts:68-…`) | — | `{release, answers:{key:bool}, done:[key]}`; 404 `No release`; 500 `Failed to load release` |
| 11 | `GET /api/whats-new/changelog` | `whatsNew.ts:68` | same | `changelogFor` (`releases/index.ts:212-271`) | — | `{releases: ChangelogRelease[], openCount}`; 500 `Failed to load the changelog` |
| 12 | `GET /api/whats-new/open` | `whatsNew.ts:78` | same | `openRequirements` (`releases/index.ts:274-278`) | — | `{count, items:[{version,key,title}]}`; 500 `Failed to load open requirements` |
| 13 | `GET /api/whats-new/mood-mixes` | `whatsNew.ts:90` | same | `moodMixState` (`releases/moodMixes.ts:85-107`) | — | `{mix:{id,name,mood,trackCount,retired}|null, decision:"rebuild"|"keep"|null, playlist:{id,name}|null}`; 500 `Failed to load your mood mix` |
| 14 | `POST /api/whats-new/mood-mixes/decision` | `whatsNew.ts:105` | same | `decideMoodMixes` (`moodMixes.ts:116-142`) | `{decision:"rebuild"|"keep"|"remove"}` (zod) | `{decision, removed}`; 400 `{error:"Invalid request", details}`; 400 `This mood mix can't be shown any more. Save it as a playlist or remove it.`; 500 `Failed to record your choice` |
| 15 | `POST /api/whats-new/mood-mixes/save` | `whatsNew.ts:126` | same | `saveMoodMixAsPlaylist` (`moodMixes.ts:151-185`) | `{mixId: string ≥1}` | `{id,name,trackCount,created}`; 400 invalid; 404 `Mood mix not found`; 500 `Failed to save your mood mix` |
| 16 | `POST /api/whats-new/:version/choice` | `whatsNew.ts:145` | same; GPU key admin only | `findRelease` → GPU apply (`setGpuToggles`) → `setChoice` (`releases/index.ts:289-308`) | `{key: string ≥1, enabled: boolean}` | `{key,enabled,decidedBy:"EXPLICIT"}`; 404 `Unknown release`; 403 `Only admins can change GPU acceleration`; 400 `GPU acceleration is only available on linux/x86_64 hosts`; 400 invalid / `Unknown choice`; 500 `Failed to record choice` |
| 17 | `POST /api/whats-new/:version/dismiss` | `whatsNew.ts:184` | same | `dismissRelease` (`releases/index.ts:311-364`, one transaction; admin-only choices only for admins) | — | `{version, dismissed:true, recordedDefaults:[key]}`; 404 `Unknown release`; 500 `Failed to dismiss release` |
| 18 | `GET /api/recommendations/for-you` | `routes/recommendations.ts:12` | `requireAuthOrToken` (`:9`) | inline: last 50 library plays → top 3 artists → 10 similar each (by weight) → dedupe → drop owned artists → `album.groupBy rgMbid` counts → `heroUrl` or Redis `hero:<id>` | `?limit=10` (`parseInt`, no clamp [V `:16`]) | `{artists:[{id,mbid,name,heroUrl,coverArt,albumCount}]}` (`[]` without plays); 500 `Failed to get recommendations` |
| 19 | `GET /api/library/decades` | `routes/library/tracks.ts:582` | `requireAuthOrToken` | `libraryDecades` (`services/mixes/radioRow.ts:59-76`) | — | `{decades:[{decade,count}]}` (≥15 songs, newest first); 500 `Failed to get decades` |
| 20 | `GET /api/library/radio/stations` | `tracks.ts:592` | same | `moodStationList` (`services/mixes/stations.ts:111`) | — | `{stations:[{value,name,description}]}` (14) |
| 21 | `GET /api/library/radio/row` | `tracks.ts:600` | same | `radioRow` (`radioRow.ts:103-109`) + `stationCovers` (`stationCovers.ts:123`) | `?scope=home|all` (default home), `?tz=` | `{stations:[{type,value,name?,description?,count?,cover}]}`; 400 `scope must be home or all`; 500 `Failed to get radio stations` |
| 22 | `GET /api/library/radio` | `tracks.ts:613` | same | `selectRadioTracks` (`services/mixes/stationSelection.ts:89-475`), `stillPlayable`, `spreadByArtist` | `?type=discovery|favorites|decade|genre|mood|workout|artist|all`, `value`, `limit` (`parseLimit(…,50,100)`), `seed`, `offset`, `tz` | `{tracks:[{id,title,displayTitle,duration,trackNo,discNumber,discSubtitle,filePath,artist:{id,name},album:{id,title,coverArt}}], next?}`; 400 `Radio type is required`; 400 `Unknown mood: x`; 400 `BAD_STATION_REQUEST` (`stationSelection.ts:44`); 400 `Artist ID required for artist radio`; 200 `{tracks:[],message:"The x station has been retired"}`; 401 `This station is built from your own plays; sign in to use it`; 500 `Failed to get radio tracks` |

Related, **already served** (parity check only, Task 11): `GET /api/library/genres` (`tracks.ts:573`, `libraryGenres` `radioRow.ts:40-57`: ≥15 songs, top 20, lower-cased, count desc then name) — kima `crates/kima-api/src/routes/library/genres.rs:20` adds an artist-name filter (existing divergence; confirm its row). **Not M5b:** `/api/releases` (`index.ts:179`, Release Radar = M7 discover/downloads).

**Frontend callers** [V `frontend/lib/api.ts`]: radio `:823`, radio row `:874`, for-you `:916`, mixes `:1610-1628`, mood-mixes `:1640-1657`, update-status `:1645`, buckets `:1664-1680`; changelog page fetches `/whats-new/changelog` directly (`app/changelog/page.tsx:37`); `WhatsNewGate`/`WhatsNewWindow` call pending/current/choice/dismiss. `/library/decades` and `/library/radio/stations` have no web caller (the apps use them, `tracks.ts:591` comment).

**Subsonic counterparts:** none. Upstream's Subsonic routes call no mix/radio service (`git grep services/mixes -- routes/subsonic` empty [V]); `getInternetRadioStations` is a not-supported stub upstream (`subsonic/compat.ts:38-54`) and already in kima (`kima-subsonic/src/routes/mod.rs:110`). `getRandomSongs`, `getTopSongs`, `getSimilarSongs(2)` exist in kima (M2/M5 D-190) and are untouched.

### Services, jobs, caches [V]

| Upstream | What | kima-rs |
|---|---|---|
| `services/mixes/index.ts:54-308` | 42 generators; 5 picked by LCG over `getSeededRandom("<day>-<userId>")` (or random on refresh); failures logged and replaced by the next untried type; saved mood mix appended | `kima_mixes::daily` (Task 9) |
| `services/mixes/recipes.ts` (628 lines) + `recipes.json` (44 recipes) + `moodDefinitions.ts/.json` | recipe parse/validate, `recipeRankingSql`, members ≥ `RECIPE_LINE` SD, filters (`bpm`, `instrumental_min/max`, `arousal_pct`, `minor`), familiarity boost, `UnrankableLibraryError`, `pickMixTracks`/`pickPageTracks`/`pickStationPage`/`pickWildcardPage` | `kima_mixes::recipes` + `kima_db::mixes::recipe` (Tasks 3–4) |
| `services/moods/tags.ts` (+ `moodTags.integration.test.ts` 17, `tags.test.ts` 6) | writes `Track.moodTags` in the mood phase | `kima-ident` moods phase (Task 5) |
| `services/mixes/{genreMixes,moodMixes,discoveryMixes,timeMixes,recipeMix,helpers,artistCap,mixDay}.ts` | generators and draws | Tasks 2, 7, 8 |
| `services/mixes/nightPlays.ts` | night zone, night songs (22:00–03:59 local, 365 d), cold start top-up via vibe matching, energy band cached `mixes:energy-band` | Task 8 |
| `services/mixes/familiarity.ts` | 200 seeds (365 d) → vibe `topScores` → boosts; cache `mixes:familiar:<user>` 3 h / 60 s empty; Redis lock 60 s, wait 20 s | Task 8 |
| `services/mixes/stationOrder.ts` | per-listener station sessions in Redis (`station:<station>:<listener>`, 12 h, 3 kept), `replayPasses` rebuild, seam window | Task 12 |
| `services/mixes/stationSelection.ts`, `stations.ts` | radio types; 13 mood stations + Wildcard, retired values, subtitles | Tasks 11, 13 |
| `services/mixes/radioRow.ts`, `stationCovers.ts` | row composition (`vibeEmbeddings` gate `:87-91`), per-listener per-local-day covers in Redis, `warmRadioCovers` | Task 14 |
| `workers/index.ts:191-205` | `radio-covers` cycle: first after 60 s, then every 5 min, single-flight | Task 14 (kima-jobs schedule or a server task) |
| `workers/unifiedEnrichment.ts:1251-1262,2826` | `dropCachedMixes(moodsOnly)` after a moods change; full drop after core/full enrichment (`clearMixCache`) | Task 5/6 |
| `services/moodBucketService.ts:274-470` | presets, mood mix, saved mix, served-track memory | Task 10 |
| `services/releases/{catalog,index,moodMixes}.ts`, `services/updateCheck.ts`, `services/gpuRuntime.ts` | release window, choices, mood-mix question, update check | Tasks 15–17 |

**No scheduled mix refresh job exists upstream** [V]: mixes regenerate lazily on a cache miss (1 h TTL) or `POST /mixes/refresh`; the only timer is `radio-covers`.

### Upstream tests to port (counts = `it(`/`test(` lines) [V]

`routes/__tests__/`: `mixesMoodServe` 4, `mixesSave` 5, `mixesTimeZone` 17, `mixesValidation` 7, `whatsNewChangelog` 6, `whatsNewGpuChoice` 5, `whatsNewMoodMixes` 28, `whatsNewReopen` 4. `routes/library/__tests__/`: `radio.integration` 33, `radioArtistRule` 2, `radioDiscoveryPlayable` 1, `radioFavoritesPlayable` 1, `radioOwnPlays` 12, `radioRow` 15, `genreSpellings` 10. `services/__tests__/`: `mixSampling` 14, `moodBucketMixPool` 8, `moodBucketService` 13, `releases` 29. `services/mixes/__tests__/`: `deepCutsOwnPlays` 2, `mixCutoffs` 4, `mixDay` 5, `mixGenerators` 33, `mixPick` 4, `mixRotationFailures` 3, `nightMixEligibility` 5, `nightPlays` 28, `randomDiscoveryEligibility` 1, `recipes.integration` 63, `recipes` 16, `savedMoodMix` 3, `seededDraw` 14, `stationOrder` 20, `stations` 12, `unplayedTracks` 3, `unrankableRecipe` 1, `weightedOrder` 3. `services/moods/__tests__/`: `moodTags.integration` 17, `tags` 6. Total ≈ 512. Each task names the files it ports; a test that needs Redis semantics is ported against the in-process store (Q1).

## Dependencies on M5 and the degradation matrix

Mirrors M5 spec §4.4. "Heads" = learned mood heads (`vibe_mood_heads` asset, **blocked**); "probe" = feel probe (provisioned on the owner's app); "readings" = CLAP zero-shot readings (M5 Task 17, live); "analysis" = DSP tempo/key (`track_analysis`, live since D-253).

| Missing | Effect in M5b | How the API says so |
|---|---|---|
| **Heads (today)** | `mood_state` has no rows/statistics → `recipeMembers` null (`recipes.ts:499-500`) → all **44 recipes** unavailable: 25 recipe mixes, the recipe fallback of the 2 night mixes (Late Night, Midnight Drive still play the listener's own night plays), day mix, Party/Workout/Focus, mood-bucket mixes, 13 mood stations, Workout station, Wildcard (no `mood_tags`, needs `track_moods`), night mixes' recipe fallback, familiarity (recipe-only) | `GET /mixes`: those generators return null, the 5 are filled from the 11 non-recipe generators (Era, Genre, Top Tracks, Rediscover, Artist Similar, Random Discovery, Deep Cuts, Key Journey, Tempo Flow, Vocal Detox, Minor Key Mondays) by `index.ts:271-293`; presets `trackCount:0`, `pendingTracks` from the mood snapshot; bucket GET/save 400 "Not enough tracks…"; upstream's radio row would still list Workout + 14 mood tiles when `vibeEmbeddings` is true and they would play `{"tracks":[]}`; **Q2 = B**: kima lists them only while mood statistics are available (D-266), so they are hidden today |
| Probe | No matching state → night cold-start top-up and familiarity have no vectors; `vibeEmbeddings` false → radio row hides Workout and mood tiles (upstream gate) | night mixes from own plays only; stations unchanged |
| Readings | `arousal`, `instrumental`, `acoustic` components and `arousal_pct`/`instrumental_*` filters have no statistics → recipes using them unrankable (`UnrankableLibraryError` → no mix/empty station); Vocal Detox, Minor Key Mondays (arousal median) empty | as upstream |
| Analysis (tempo/key) | `bpm` filter (Dance Floor), `minor` component (Happy, Melancholy, stations), Key Journey, Tempo Flow, Minor Key Mondays, night `pace` | rows without analysis are excluded/neutral as upstream; **Q7 = A**: steady beat = `tempo_confidence ≥ 2/5.32 ≈ 0.376` (D-262) |
| `energy` (no column in kima) | artist radio vibe score drops the energy term; the night energy band is read from `Track.arousal` = CLAP `arousal_raw` as upstream (owner 2026-11-20; it was 0.5 for every song until then) | D-263; **Q8 = A** (absent, upstream null path) for artist radio |
| `danceability` (MusiCNN unprovisioned) | artist radio drops the term | D-263 (Q8 = A) |

Public M5 API used: `kima_db::vibe::{matching_context (rank.rs:203), rank_matches (:448), top_scores (:463)}` [V], `kima_ident::stages::moods::subscribe_changes` [V `coordinator_vibe.rs:324-341`], mood snapshot `pending` (used by `/vibe/mood-summary`), `vibe_embeddings` predicate (`kima_ident::stages::vibe_assets::vibe_embeddings`, used by `/system/features`) [V `routes/system.rs:17-22`].

## kima-web pages: M10 vs M5b

Per the roadmap M10 row these pages are **M10**, except the thin slice the owner pulled into M5b (**Q10 = B**, 2026-11-19): the `/radio` page, the home "Made for you" row and the mix page (Phase F, Tasks 22–23).

| Upstream page/component | Endpoints | Milestone |
|---|---|---|
| Home `app/page.tsx` "Made for you" (`MixesGrid`) | 1 | **M5b** Phase F (Task 23) |
| Home radio row (`features/home/components/LibraryRadioStations.tsx`), for-you artists | 21, 18 | M10 |
| `app/mix/[id]/page.tsx` | 7, 6, 5 | **M5b** Phase F (Task 23, `/mix?id=`) |
| `components/MoodMixer.tsx` | 2, 3, 4 | M10 |
| `app/radio/page.tsx` | 21, 22 | **M5b** Phase F (Task 22) |
| `components/whats-new/{WhatsNewGate,WhatsNewWindow,MoodMixChoice,…}`, `app/changelog/page.tsx`, TopBar/Sidebar version chip | 8–17 | M10 |
| Artist page "radio" button (`app/artist/[id]/page.tsx`) | 22 `type=artist` | M10 |

---

## Phase A: Foundations

### Task 0: Record decisions before code (docs only)

**Files:** `docs/DIVERGENCES.md`, `docs/STATUS.md`, this plan's Owner decisions / Open questions sections.
- [x] After the owner answers Q1–Q11 (2026-11-19), write the answers into "Owner decisions" (6–16) and "Rozstrzygnięte" (decision + date) and reserve rows D-260..D-271 for each divergence named below (status `accepted (owner …); not implemented yet`, not `to-decide`, because every one is decided; each implementing task amends its row); grep this plan for text encoding the old option and fix it in the same commit (Q6 B: Appendix D, Tasks 9/10/19; Q2 B: Task 14; Q3 B/Q5 A: Tasks 15/20; Q10 B: Phase F).
- [x] Commit `docs(m5b): record owner decisions`.

Reserved rows: D-260 listener state (Q1, Task 6) · D-261 `LIBRARY_PLAYABLE` = present track (Task 2) · D-262 steady beat (Q7, Task 4) · D-263 missing energy/danceability (Q8, Tasks 8/13) · D-264 track rows = full upstream column set (Q6, Tasks 9/10) · D-265 mix/mood-mix cover URLs as native tokens (Tasks 9/10) · D-266 mood/Workout radio tiles gate (Q2, Task 14) · D-267 own release catalog (Q3, Tasks 15/20) · D-268 no GPU choice (Q5, Task 15) · D-269 releases per profile (Q9, Task 15) · D-270 update check target (Q4, Task 17) · D-271 recommendations storage/cover/"owned" (Task 18).

### Task 1: Schema `0063_mixes.sql` and `kima_db::mixes` basics

**Files:** Create `crates/kima-db/migrations/0063_mixes.sql`, `crates/kima-db/src/mixes/mod.rs`, `crates/kima-db/tests/mixes_schema.rs`. Modify `crates/kima-db/src/lib.rs`.
- [x] **RED:** `mixes_schema.rs`: `user_mood_mix_is_per_profile_and_cascades`, `playlist_mix_id_is_unique_per_profile_only`, `release_rows_are_per_profile_and_cascade`, `mood_tags_default_empty_on_existing_tracks`, `upgrade_from_0062_keeps_every_row` (seed a 0062 DB with playlists/tracks, migrate, compare full rows). Run `cargo test -p kima-db --test mixes_schema` → fails (no tables).
- [x] Write the migration per the model table above (one file; `decided_by` CHECK; partial unique index on `(profile_id, mix_id)`).
- [x] GREEN; `cargo sqlx prepare --workspace -- --all-targets`; commit `feat(db): mixes, mood mix and release tables (0063)`.

### Task 2: Pure draws with upstream goldens (`kima-mixes` crate)

**Files:** Create `crates/kima-mixes/{Cargo.toml,src/lib.rs,src/seeded.rs,src/artist.rs,src/day.rs}`, `tools/mix-goldens/upstream.mjs` (reads `services/mixes/helpers.ts`, `artistCap.ts`, `mixDay.ts`, `utils/artistSpread.ts` from the pin's git objects like `tools/vibe-goldens/upstream.mjs`, writes JSON), `crates/kima-mixes/tests/fixtures/draw_goldens.json`, `crates/kima-mixes/tests/draws.rs`.
- [x] **RED** `draws.rs`: `seeded_random_matches_js_for_ascii_unicode_and_overflow` (incl. a non-BMP string: UTF-16 code units), `daily_pick_indices_match_upstream_lcg`, `seeded_key_is_md5_hex_of_seed_colon_id`, `seeded_order_and_take_agree_and_match_goldens`, `weighted_order_seeded_matches_goldens`, `cap_by_artist_raises_2_3_then_lifts`, `spread_by_artist_matches_goldens`, `mix_day_is_server_local_date_and_weekday_parses_utc`, `song_artist_key_matches_upstream` (ports `seededDraw` 14, `weightedOrder` 3, `mixDay` 5, `mixSampling` pure cases).
- [x] Generate goldens (`node tools/mix-goldens/upstream.mjs --pin c7527577 > …`), commit with the pin SHA inside the file.
- [x] Implement; GREEN; clippy; commit `feat(mixes): seeded draws and artist cap with upstream goldens`. D-261 (amend): `LIBRARY_PLAYABLE` = present track (no `corrupt`, no `location` until M7). Mix day per Q11 = A (server calendar date).

### Task 3: Recipes, definitions and stations (pure)

**Files:** Create `crates/kima-mixes/assets/{recipes.json,moodDefinitions.json}` (byte copies from the pin), `src/recipes.rs`, `src/stations.rs`, `tests/recipes_parse.rs`.
- [x] **RED:** `vendored_files_match_pin_sha256` (hashes recorded from `git show c7527577:…`), port `recipes.test.ts` 16 (parse errors: id pattern, unknown component, twice, weights, familiarity bounds, filter ranges), `stations.test.ts` 12 (`station_list_order_and_subtitles`, `retired_values_answer_retired`, `unknown_value_is_unknown`, `late_night_is_personal`).
- [x] Implement `parse_recipes`, `get_recipe`, `MOOD_STATIONS`, `station_subtitle`, `resolve_mood_station`, `mood_station_list`; GREEN; commit `feat(mixes): recipes and mood stations`.

### Task 4: Recipe ranking in SQL on the kima schema

**Files:** Create `crates/kima-db/src/mixes/recipe.rs`, `crates/kima-db/tests/mixes_recipe.rs` (+ seeder `tests/mixes_common/mod.rs`).
- [x] **RED:** port `recipes.integration.test.ts` 63 and `unrankableRecipe` 1 over synthetic `track_moods` + `mood_state` + `track_clap_readings` + `track_analysis` rows: `member_needs_line_1_25_sd_above_mean`, `low_component_pushes_away`, `bpm_filter_counts_double_and_half_on_steady_beat`, `steady_beat_is_upstream_2_of_5_32_rescaled` (0.375 excluded, 0.377 included), `minor_null_when_scale_unknown`, `arousal_pct_band_over_eligible`, `one_copy_per_title_and_artist_furthest_above_line`, `no_mood_state_means_no_members`, `fewer_than_8_scored_means_none`, `zero_spread_is_unrankable`, `familiarity_boost_only_lifts`, plus `heads_absent_every_recipe_is_none` (the live degradation).
- [x] Implement `recipe_members`, `recipe_member_count`, `pick_mix_tracks`, `pick_page_tracks`, `pick_station_page` (page source only; order in Task 12), `SCORED_SONGS` on kima tables (`track_moods` joined on `track_embeddings.embedded_at`), readings via the M5 `READING_SQL` equivalent, `PACE_SQL` and `STEADY_BEAT` per Q7 = A (`tempo_confidence >= 2.0/5.32`, ≈ 0.376, one named constant with the derivation in its doc comment; D-262), key scale from `track_analysis.key_mode`.
- [x] GREEN; sqlx prepare; commit `feat(db): recipe ranking and members`.
- Done notes: a station's source is `recipe_members` itself (Task 12 adds `pick_station_page` with the order); the Wildcard source needs `mood_tags` (Task 5) and the station order (Task 12); the familiarity boost is handed in (`RecipeContext.boost`; Task 8 computes it); the page draw's seed is the caller's (upstream `randomUUID` per call). The SQL is built at run time (no `query!`), so `.sqlx` is unchanged. The synthetic-library case is a Rust reference over a deterministic 600-song library (upstream uses its `syntheticLibrary` fixture), all 44 recipes.

### Task 5: Mood tags writer and the moods-changed hook

**Files:** Modify `crates/kima-ident/src/stages/moods.rs`, `crates/kima-db/src/vibe/moods.rs`; create `crates/kima-ident/tests/mood_tags.rs`.
- [x] **RED:** port `tags.test.ts` 6 + `moodTags.integration.test.ts` 17: `tags_follow_defined_ctes`, `untagged_song_has_empty_array`, `tags_rewritten_when_scores_change`, `no_heads_no_tags_written` (degradation), `moods_changed_drops_mood_dependent_mix_caches_only` (energy band + familiarity kept, `cacheKeys.ts:11-13`).
- [x] Implement the writer inside the existing moods pass (same transaction discipline as M5), subscribe the mix cache (Task 6) to `subscribe_changes`; GREEN; commit `feat(ident): write mood tags and drop stale mix caches`.
- Done notes: the writer is `kima_db::mixes::tags::MoodTagger` (it reuses the recipes' definition SQL), run by the kima-ident mood pass after the statistics; tests `kima-db/tests/mixes_tags.rs` (10) and `kima-ident/tests/mood_tags.rs` (2). As upstream's `hashInputs`/`changeSignal`, the statistics' fingerprint now also hashes tempo, its confidence and the key's scale and the signal reads `track_analysis` (the driving definition reads the pace; the tag pass is skipped while the fingerprint stands). An analysis re-run clears the tags (upstream `resetAnalysis`). `moods_changed_drops_mood_dependent_mix_caches_only` moves to Task 6, which builds the cache it drops. Not ported: the two Prisma deadlock cases and the corrupt-song case (no `corrupt`, D-261).

### Task 6: Listener state store (replaces Redis; per Q1)

**Files:** Create `crates/kima-mixes/src/store.rs` (Q1 = A: in-process, no tables), `crates/kima-mixes/tests/store.rs`; wire into `AppState` (`crates/kima-api/src/state.rs`).
- [x] **RED:** `mix_list_cache_per_profile_and_zone_ttl_1h_max_4_zones`, `refresh_replaces_only_its_zone`, `station_sessions_3_kept_12h`, `familiar_scan_single_flight_waits_then_scans_itself`, `energy_band_survives_mood_drop`, `drop_all_and_drop_mood_dependent`, `cover_pick_kept_until_local_day_end_no_cover_600s`, and from Task 5 `moods_changed_drops_mood_dependent_mix_caches_only` (the store subscribed to `moods::subscribe_changes`).
- [x] Implement; GREEN; commit `feat(mixes): in-process listener state store`. D-260 (amend, Q1 = A).
- Done notes: `kima_mixes::store` (+ `kima_mixes::familiar::FamiliarBoost`, re-exported by `kima_db::mixes::recipe`), `AppState.listeners`, the mood-drop task wired in `kima-server/src/main.rs`; tests `kima-mixes/tests/store.rs` (9). Methods take the time they act at. The familiarity scan's caller loop (take lock, wait, scan) is Task 8; `drop_all` after core/full enrichment (upstream `clearMixCache`) is wired with the mix list's first consumer (Task 9).

## Phase B: Mixes

### Task 7: Non-recipe generators

**Files:** `crates/kima-mixes/src/generators/{era_genre,discovery,analysis}.rs`, `crates/kima-db/src/mixes/generators.rs`, `crates/kima-db/tests/mix_generators.rs`.
- [x] **RED:** port the non-recipe half of `mixGenerators.integration` 33, `mixCutoffs` 4, `deepCutsOwnPlays` 2, `randomDiscoveryEligibility` 1, `unplayedTracks` 3, `mixPick` 4: `era_needs_mixable_decade`, `genre_top20_min5_two_per_artist`, `top_tracks_from_own_listens`, `rediscover_unplayed_recently`, `artist_similar_uses_library_similar_artists`, `deep_cuts_unplayed_then_three_or_fewer`, `key_journey_needs_completed_analysis_and_key`, `tempo_flow_bands_100_130`, `vocal_detox_instrumentalness_075` (CLAP reading), `minor_key_arousal_above_median`, `profile_isolation_of_plays`.
- [x] Implement with `listen_rows` (existing `kima_db::stats::listens`); GREEN; commit `feat(mixes): era, genre, discovery and analysis mixes`.
- Done notes: pure parts in `kima_mixes::mix` (`ProgrammaticMix`, colours, cover selection, Key Journey and Tempo Flow arrangements), SQL in `kima_db::mixes::generators` (one function per generator plus the shared seeded/capped draws and `mix_songs`); tests `kima-db/tests/mix_generators.rs` (16). More Like takes a `SimilarArtists` resolver (kima-db cannot call `kima_ident::similar`; the API wires the real one in Task 9). Column mapping and the stored similar list: D-272.

### Task 8: Recipe mixes, day mix, night plays, familiarity

**Files:** `crates/kima-mixes/src/generators/{recipe,night,familiar}.rs`, `crates/kima-db/src/mixes/{night,familiar}.rs`, `crates/kima-db/tests/{night_plays,familiarity}.rs`.
- [x] **RED:** port `nightPlays` 28, `nightMixEligibility` 5, `savedMoodMix` 3, recipe half of `mixGenerators`: `night_is_22_to_04_in_requested_zone`, `bad_zone_falls_back_to_server_zone` (reuse `safe_zone`), `cold_start_tops_up_with_vibe_matches`, `cold_start_without_probe_uses_own_plays_only`, `recipe_mix_seed_is_recipe_day_profile`, `day_mix_only_sunday_and_monday`, `familiar_seeds_200_lookback_365`, `familiar_cache_3h_empty_60s`.
- [x] Implement via `matching_context`/`top_scores`; GREEN; commit `feat(mixes): recipe, day, night and familiar mixes`.
- Remaining `recipes.integration` cases (Task 4 handed the boost in; 16 of the 29 not yet ported), the scan itself: "gives a listener with no plays exactly the ranking of no listener", "lifts an unheard song that sounds like the listener's most-heard song above an unheard one that does not", "gives the listener's own most-heard song the full boost, no smaller than any song's", "lets a song just under the line in when it sounds like what the listener plays, and keeps the one that does not out", "counts only plays marked as heard, and only those of the last 365 days", "keeps the closest songs in Redis for the same seeds, and builds them again when the seeds change", "keeps its result when the listener plays a seed song again, and rebuilds only when a new song becomes a seed", "runs one scan for callers that ask at once", "waits for another process that holds the scan lock and uses its result, without scanning", "scans itself when the lock holder gives no result within the wait, and takes then releases the lock otherwise", "breaks ties between equally heard songs by the most recent listen, not by id", "never writes no-boost over a boost already kept for the same seeds", "frees the scan lock only while it still holds this caller's token", "gives no boost, and keeps that for a minute at most, while the similarity data is not built", "gives no boost, rather than failing, when the scan throws, and keeps that for a minute at most", "still boosts the unheard songs that sound like what was played, and gives every played song the full boost" (large-share library).
- Done notes: `kima_db::mixes::recipe_mixes` (the 28 recipe Home mixes and the two day mixes, `recipe_mix`/`day_mix`, Unrankable = no mix), `kima_db::mixes::night` (`night_zone`, `night_songs`, `night_mix_tracks`, `night_mix`, `LATE_NIGHT`/`MIDNIGHT_DRIVE`; energy 0.5 per Q8 = A, replaced 2026-11-20 by the upstream band from CLAP arousal: `energy_band`, `night_songs_ranked_by_energy`, `NightInputs.store`), `kima_db::mixes::familiar` (`seeds_of`, `signature_of`, `familiar_boost` over the store's cache and scan lock); tests `kima-db/tests/night_plays.rs` (10) and `familiarity.rs` (4), seeding in `tests/mix_seed`. Of the 16 familiarity cases listed above, ported: no plays = no boost, the closest lifted by rank, the seeds' full boost, heard plays of 365 days, kept for the same seeds and rebuilt for a new seed, one scan shared (wait for the holder), scan itself after the wait, no-boost never over a boost, the lock freed only by its holder, no boost (kept briefly) without similarity data; the ranking effect of a boost stays covered by Task 4's `familiarity_boost_only_lifts`. Not ported: the large-share library case and the scan that throws (no injectable failure; a failed scan logs and keeps no boost, as the probe-less path shows); the Late Night station cases go to Task 13. D-273.

### Task 9: Daily mix list and `/api/mixes`, `/api/mixes/{id}`, refresh, save

**Files:** `crates/kima-mixes/src/daily.rs`, `crates/kima-api/src/routes/mixes.rs`, `crates/kima-api-types/src/mixes.rs`, `crates/kima-api/tests/mixes.rs`.
- [x] **RED:** port `mixesTimeZone` 17, `mixesSave` 5, `mixRotationFailures` 3: `get_mixes_returns_five_plus_saved_mood_mix`, `failed_generator_is_replaced_by_next_untried_type`, `same_day_same_list_refresh_differs`, `zone_keyed_cache`, `get_mix_tracks_in_mix_order_present_only`, `get_mixes_refresh_is_404_mix_not_found_not_405`, `save_creates_playlist_atomically_private_name_or_custom`, `save_twice_409_with_playlist_id_and_name`, `concurrent_saves_one_playlist`, `save_all_missing_422`, `save_announces_playlist_event`, `sibling_profile_cannot_see_or_save`, `mix_track_rows_carry_every_upstream_column` (Q6 = B: the key set of each `tracks[]` element, of its `album` and of `album.artist` equals Appendix D exactly; absent data is JSON `null`, never a missing key; mapped values read back from seeded `track_analysis`/`track_clap_readings`/`track_stages` rows).
- [x] Implement the Appendix D row (one shared `kima_db::mixes::track_rows` select + `kima-api-types` DTO used by Tasks 9 and 10); GREEN; commit `feat(api): mixes routes`. D-rows (amend): D-264 track rows (Q6 = B), D-265 cover URLs as native tokens.
- Done notes: pure rotation `kima_mixes::daily` (`GENERATORS` in upstream order, `assemble`; tests `kima-mixes/tests/daily.rs` 3: a failed or empty pick replaced by the next untried generator of a new type, all failing = empty list, five picks need no fill); `kima_db::mixes::daily::generate_all_mixes` (day seed `"<day>-<profile>"`, refresh seed `"<profile>-<ms>-<random>"` and day `"<day>-<ms>"`, every generator wired by name, unit test `every_generator_is_wired`; the familiarity boost is asked only for a recipe with a familiarity share while the heads are available), `mood_mix::user_mood_mix` (saved mood mix appended; Task 10 extends the module), `track_rows` (Appendix D), `save::save_mix_playlist` (D-274); routes `kima-api/src/routes/mixes.rs` (`GET /mixes/refresh` routed to the `/:id` handler; More Like resolves stored similar artists through `kima_ident::similar`); DTO `kima_api_types::mixes::MixTrack`. API tests `kima-api/tests/mixes.rs` (13): the five plus the saved mood mix (read again, missing songs dropped; none for a sibling profile or another account), the fill from non-recipe generators with no heads, same day same list and a refresh that differs and is then served, per-zone lists (an unknown zone reads the server's), `GET /mixes/{id}` rows in mix order without missing songs, 404 bodies, `GET /mixes/refresh` 404 not 405 and 401 without a session, save (private, mix name or custom name, items in order, `mix_id`), 409 with the playlist, three concurrent saves = one playlist and two 409, 422 with nothing written, one `playlist:updated` event for the profile, sibling isolation, every Appendix D key present in upstream's model order with values read back from seeded analysis/reading/stage rows. Not ported as separate cases: upstream's Redis-error paths of `mixesTimeZone` (no Redis, D-260) and its cases that drive `generateAllMixes` through mocks (the zone reaching the night generators is covered by Task 8's `night_plays`).

### Task 10: Mood buckets and the saved mood mix

**Files:** `crates/kima-db/src/mixes/mood_mix.rs`, extend `routes/mixes.rs`, `crates/kima-api/tests/mood_buckets.rs`.
- [x] **RED:** port `mixesMoodServe` 4, `mixesValidation` 7, `moodBucketService` 13, `moodBucketMixPool` 8: `presets_five_moods_counts_and_pending`, `presets_zero_when_heads_absent`, `invalid_mood_400_with_valid_moods`, `not_enough_tracks_400_with_suggestion`, `served_ids_skipped_while_8_remain`, `record_served_update_only`, `save_with_track_ids_keeps_order_drops_missing`, `save_none_playable_400`, `save_validation_messages`, `save_drops_mix_cache`, `saved_mix_reread_on_home_list`, `bucket_track_rows_use_the_appendix_d_shape` (GET and save responses' `tracks[]`, Q6 = B).
- [x] Implement; GREEN; commit `feat(api): mood bucket mixes`.
- Remaining `recipes.integration` case (1 of the 29): "shows 0 on a mood preset whose page recipe has fewer than 8 members, the count its page plays" — ported as `presets_show_0_when_the_page_recipe_has_fewer_than_8_members`.
- Done notes: `kima_db::mixes::mood_mix` (`MOODS` with upstream's ids/fields/recipes/names/colours/icons, `page_song_count`, `mood_mix` (page recipe draw with a fresh seed per call, `mood-<mood>-<ms>`), `save_user_mood_mix` (upsert per profile, ids cut to whole ms so the saved id and `generatedAt` agree), `remembered_track_ids`, `record_served` (update only), `user_mood_mix` from Task 9); routes `GET /mixes/mood/buckets/presets`, `GET /mixes/mood/buckets/{mood}`, `POST /mixes/mood/buckets/{mood}/save` in `routes/mixes.rs` (track rows of Task 9 serialised straight, so Appendix D key order holds; a save drops the profile's cached Home lists). Tests `kima-api/tests/mood_buckets.rs` (14) on a library from `kima-db/tests/mixes_common` (shared by `#[path]`) with test heads installed as the engine's: presets (counts = page members, pending from the snapshot, 0 and 0 without heads, D-275), fewer than 8 members = 0 and no mix, invalid mood 400 with `validMoods` (also `presets` as a save target), not enough tracks 400 with the suggestion (also a save without or with an empty `trackIds`), the page mix = 15 distinct members, the served ids skipped while 8 remain and the start-over when too few, per-profile memory, update-only recording, save with ids (order kept, unknown/non-UUID/missing dropped, `your-mood-mix-<ms>` = `generatedAt`), none playable 400 keeping the old mix, the validation messages (plus unit test `save_track_ids_validation_matches_upstream`), the save shown on Home at once and read again there, Appendix D rows on GET and save. A mutation serving without the memory failed `served_ids_skipped_while_8_remain`. `moodBucketMixPool`'s 3,000-track draws per mood ("reaching nearly all members") are not ported: kima has no synthetic-library fixture of that size; the even draw is Task 4's `draw` with its goldens.

## Phase C: Radio

### Task 11: Decades, station list, genres parity

**Files:** `crates/kima-api/src/routes/library/radio.rs`, `crates/kima-db/src/radio.rs`, `crates/kima-api/tests/radio_lists.rs`.
- [x] **RED:** `decades_effective_year_display_original_year_min15_newest_first`, `stations_list_14_with_subtitles`, `library_genres_matches_upstream_min15_top20` (port `genreSpellings` 10 as far as the existing genres route is concerned; record any existing difference in its row).
- [x] Implement; GREEN; commit `feat(api): library decades and radio station list`.
- Done notes: `kima_db::radio::library_decades` (`COALESCE(display_year, year)`, present tracks, ≥ `MIN_STATION_SONGS` 15, newest first; D-276), routes `GET /library/decades` and `GET /library/radio/stations` in `kima-api/src/routes/library/radio.rs` (struct replies, upstream key order; 500 `Failed to get decades`). The genre list's row is **D-114** (with D-166 (2) for the key): `kima_db::genres::genre_totals` now ports v1.12.3 `computeGenreTotals` (self-name tag dropped per artist by upstream's `genreKey`, not every artist-named genre; the spelling with most songs; UTF-16 ties; 500 `Failed to get genres`); spellings still group by kima's key (open option of D-166 (2)), recorded in D-114. Tests `kima-api/tests/radio_lists.rs` (6): 401s, decades, empty decades, stations (14, key order), the `genreSpellings` `GET /genres` cases on kima's key, the 500 messages; `genres.rs::library_genres_drop_only_an_artists_own_name` replaces the v1.11 rule's test; unit tests `upstream_key_folds_spellings`, `totals_count_an_artist_once_per_genre_and_drop_its_own_name`. `genreSpellings`' `GET /radio?type=genre` cases go to Task 13.

### Task 12: Station play order

**Files:** `crates/kima-mixes/src/station_order.rs`, `tests/station_order.rs`, goldens from `stationOrder.ts` via `tools/mix-goldens`.
- [x] **RED:** port `stationOrder.test.ts` 20: `same_seed_same_order_while_pool_unchanged`, `offset_pages_and_next`, `pass_restarts_without_last_artist_or_seam_window_songs`, `lost_session_rebuilt_by_replay_passes`, `replay_bounds_1000_passes_200k_songs`, `three_stations_kept_per_profile`, `seed_pattern_rejected`.
- [x] Implement over the Task 6 store; GREEN; commit `feat(mixes): station play order`.
- Done notes: `kima_mixes::station_order` (`station_page` generic over the pool and `playable` futures, `replay_passes`, `replay_first_pass`, `seed_is_valid`, `all_playable`; constants `SEAM_WINDOW` 100, `CHUNK` 1000, `REPLAY_PASSES`, `REPLAY_SONGS`, `SEED_PATTERN`; session rules from the Task 6 store, which gains `held_stations`), `kima_db::mixes::recipe::{pick_station_page, pick_wildcard_page}` (`recipe:<id>` / `wildcard`, the request's `playable` passed through for Task 13). Goldens: `tools/mix-goldens/upstream.mjs` now also loads `stationOrder.ts` (Redis and logger stubbed) and writes `stationOrder` (constants, 6 pools, 50 `replayPasses` cases incl. a pass past `REPLAY_PASSES`); the rest of `draw_goldens.json` is byte-identical. Tests: `kima-mixes/tests/station_order.rs` (20: the 18 upstream cases that have a counterpart, the two Redis-failure and the Redis-clock cases replaced by store-replacement and TTL/LRU cases, plus `replay_passes_match_upstream_goldens`), `kima-db/tests/mixes_station.rs` (12: the 12 `recipes.integration` cases above; the corrupt and unfinished-analysis Wildcard songs left out, D-261/D-186; "a song joins" is a library song whose calm score rises, not a new row). D-277.
- Remaining `recipes.integration` cases (12 of the 29), on the station order and the Wildcard station: "plays the scored songs without a tag, one title by one artist once", "follows the stored tags: a song whose tags were cleared plays until a pass over new statistics", "refills up to the limit, keeps playing while the statistics are rebuilt, and has nothing before any score", "plays every member, the big artist in runs of 1 or 2 (30 songs in 16 runs), each other song alone between", "hands out one seed's order a refill at a time: each member once, in the same order as one long refill", "never ends: after every member it starts over in a new order, not opening with the last artist or songs", "keeps a session's order when songs join or leave the pool mid-session; the next pass takes the pool as it is", "rebuilds a forgotten session, later passes included, or one Redis cannot serve, in the same order", "counts each song on a Various Artists album as its own artist, as the mixes' cap does", "names the artist for the other radios as the SQL does: the song itself on a Various or Unknown Artist album", "orders Wildcard the same way, a seed's order a refill at a time", "refills a station with members in a fresh order each call".

### Task 13: `GET /api/library/radio`

**Files:** `crates/kima-db/src/radio.rs`, `routes/library/radio.rs`, `crates/kima-api-types/src/radio.rs`, `crates/kima-api/tests/radio.rs`.
- [x] **RED:** port `radio.integration` 33, `radioArtistRule` 2, `radioDiscoveryPlayable` 1, `radioFavoritesPlayable` 1, `radioOwnPlays` 12: `type_required_400`, `discovery_unplayed_then_least_played`, `favorites_weighted_by_own_listens_else_random`, `decade_default_2000`, `genre_every_spelling_and_containing`, `mood_retired_200_message`, `mood_unknown_400`, `station_bad_seed_or_offset_400`, `workout_station`, `artist_40_60_split_vibe_weighted`, `artist_missing_400`, `all_random_300_sample`, `non_station_spread_by_artist`, `next_only_with_seed`, `heads_absent_mood_stations_empty`, `profile_plays_isolated`.
- [x] Implement; GREEN; commit `feat(api): library radio`. D-rows: `energy`/`danceability` absent in artist scoring (Q8).
- Done notes: selection `kima_api::radio::select_radio_tracks` (shared with Task 14's covers; `RadioQuery` reads repeated keys as Express does, `station_request` = `parseStationRequest`), DB reads `kima_db::radio::{playable_ids, sample_track_ids, decade_track_ids, genre_track_ids, listens_of, favorite_track_ids, discovery_track_ids, artist_songs, present_songs_of, library_artist_genres, other_library_artist, stored_similar, radio_rows}`, pure parts `kima_mixes::radio` (`parse_limit`, `js_parse_int`, `shuffle`, `draw_weighted`, `artist_sound`, `closeness`, `artist_mix`), Late Night `kima_db::mixes::night::{night_station_pool, pick_night_station_page}` (`STATION_FILL` 100; `similar_songs`' `counted` now optional as upstream's default), DTO `kima_api_types::radio`. Tests: `kima-api/tests/radio.rs` 21 (the upstream suites' cases on small exact libraries, incl. `genreSpellings`' radio cases on kima's key), `kima-mixes/tests/radio_draws.rs` (the draws' statistics over fixed random streams). D-278; D-263 artist half implemented.

### Task 14: `GET /api/library/radio/row`, covers and the warm job

**Files:** `crates/kima-mixes/src/covers.rs`, `routes/library/radio.rs`, `crates/kima-server` (job registration), `crates/kima-api/tests/radio_row.rs`.
- [x] **RED:** port `radioRow` 15: `scope_default_home_400_otherwise`, `home_6_genres_4_decades_all_lists_all`, `workout_and_mood_tiles_need_vibe_embeddings_and_mood_statistics` (Q2 = B: hidden while the heads are blocked even when `vibeEmbeddings` is true; listed once mood statistics exist; D-266), `cover_stable_for_local_day`, `no_cover_retried_after_600s`, `warm_job_picks_for_profiles_seen_within_7_days`, `warm_job_skips_deleted_profile`.
- [x] Implement; schedule `radio-covers` (first after 60 s, then 5 min, single-flight); GREEN; commit `feat(api): radio row with daily covers`.
- Done notes: `kima_mixes::covers` (`RowScope`, `row_stations`, `seconds_left`, `cover_station_key`, `COVER_SAMPLE` 20, `COVER_CONCURRENCY` 4, `HOME_GENRES` 6, `HOME_DECADES` 4, `RADIO_COVERS_FIRST`/`EVERY`), the store's `keep_cover_zone`/`cover_zones`/`forget_cover_zone` (`COVER_ZONE_TTL` 7 days) and `covers_warmed`/`mark_covers_warmed`, `kima_db::radio::{local_day, profile_exists, first_cover}`, `kima_api::radio::{radio_row, station_covers, warm_radio_covers, spawn_radio_covers}` (covers read through `select_radio_tracks`, Task 13), route `GET /api/library/radio/row`, the job spawned in `kima-server` with the shutdown token. Tests: `kima-api/tests/radio_row.rs` 12 (13 of the 15 upstream cases; the Redis-failure and Discover-album cases have no counterpart, D-260/D-261), `kima-mixes/tests/covers.rs` 5, `kima-mixes/tests/store.rs` +2. D-266 implemented; D-260 annotated.

## Phase D: What's-new and recommendations

### Task 15: Release catalog and window routes

**Files:** `crates/kima-db/src/releases.rs`, `crates/kima-mixes/src/catalog.rs` (Q3 = B: kima-rs's own catalog, empty `RELEASES` in production; the mechanism takes the catalog as a parameter so tests inject entries), `crates/kima-api/src/routes/whats_new.rs`, `crates/kima-api-types/src/releases.rs`, `crates/kima-api/tests/whats_new.rs`.
- [ ] **RED:** port `releases.test.ts` 29, `whatsNewChangelog` 6, `whatsNewReopen` 4 against an injected test catalog (Q3 = B), and of `whatsNewGpuChoice` 5 only the unknown-key path (Q5 = A: the GPU apply, 403 non-admin and linux/x86_64 400 cases are not ported, D-268); plus `production_catalog_is_empty` (pending `[]`, current 404, changelog `{releases:[],openCount:0}`, open `{count:0,items:[]}`), `choice_gpu_accelerate_is_unknown_choice_400`, `pending_is_newest_until_dismissed`, `refile_item_dropped_once_refiled`, `current_404_without_release`, `current_answers_and_done`, `changelog_newest_first_open_count`, `open_lists_owed_items`, `choice_unknown_release_404`, `choice_unknown_key_400`, `choice_zod_400_with_details`, `dismiss_records_defaults_except_explicit`, `dismiss_skips_admin_choices_for_non_main_profile`, `dismiss_is_one_transaction`, `post_mood_mixes_choice_reaches_version_route_404`.
- [ ] Implement per Q3 = B / Q5 = A / Q9 = A; GREEN; commit `feat(api): whats-new release window`. D-rows (amend): D-267, D-268, D-269.

### Task 16: Mood-mix question

**Files:** `crates/kima-db/src/releases.rs`, `routes/whats_new.rs`, `crates/kima-api/tests/whats_new_mood_mixes.rs`.
- [ ] **RED:** port `whatsNewMoodMixes` 28: `state_null_without_mix`, `retired_mood_named_from_legacy_table`, `keep_refused_for_retired_400`, `rebuild_and_remove_clear_and_drop_cache`, `decision_repeatable`, `save_creates_or_reuses_playlist_by_mix_id`, `save_adds_only_missing_tracks`, `concurrent_saves_reuse_winner`, `save_unknown_mix_404`.
- [ ] Implement; GREEN; commit `feat(api): whats-new mood mixes`.

### Task 17: Update status

**Files:** `crates/kima-net` source `github` (rate limit, deny_real), `crates/kima-api/src/update_check.rs`, `tests/update_status.rs` with a loopback replay.
- [ ] **RED:** `compare_versions_ignores_v_prefix`, `latest_404_means_none`, `cached_in_settings_and_single_flight`, `disabled_reports_no_update`, `network_failure_keeps_last_known`.
- [ ] Implement per Q4 = B: `GET https://api.github.com/repos/d1mmu-dev/kima-rs-hub/releases/latest` through `kima-net` (loopback replay in tests), upstream's `updateChecksEnabled` switch in `settings_kv`; test `asks_kima_rs_hub_releases_not_upstream`; GREEN; commit `feat(api): whats-new update status`. D-270 (amend).

### Task 18: `GET /api/recommendations/for-you`

**Files:** `crates/kima-api/src/routes/recommendations.rs`, `crates/kima-db/src/recommendations.rs`, `crates/kima-api/tests/recommendations.rs`.
- [ ] **RED:** `no_plays_empty_artists`, `top3_seed_artists_from_last_50_plays`, `ten_similar_each_by_match_deduped`, `library_artists_filtered_out` (upstream "owned"), `album_count_by_release_group`, `cover_from_artist_image_token_else_null`, `limit_parse_like_parseint` (`"abc"` → NaN → `slice(0,NaN)` = `[]` [V `:16,104`]), `profile_isolation`.
- [ ] Implement; GREEN; commit `feat(api): recommendations for you`. D-271 (amend): similar-artist storage, cover token, "owned" = in library until M7.

## Phase E: Wire parity, flag, close-out

### Task 19: Wire fixtures

**Files:** `tools/capture-api-fixtures.sh` (set `m5b`), `crates/kima-api-types/tests/fixtures/m5b/*.json`, `crates/kima-api-types/tests/m5b.rs`, `tools/check-m5b-upstream-keys.py`.
- [x] Parent runs `KIMA_CAPTURE_SET=m5b tools/capture-api-fixtures.sh` from a clean committed tip (synthetic library with SQL-seeded `track_moods`/`mood_state` so recipe paths produce non-empty arrays; a second capture with heads absent).
- [x] **RED→GREEN:** key sets of all 22 endpoints against upstream handlers (not frontend types); the mix/mood-bucket `tracks[]` elements against the full Appendix D inventory (Q6 = B), not only the keys the frontend reads; cross-ids (mix → playlist, mood mix → whats-new state), populated optional arrays; grep fixtures for tokens.
- [x] Commit `test(api-types): m5b wire fixtures`. [Reconciled 2026-11-22 against the git history and the Phase E evidence: `81ee883` capture set and checks, `b4549b9` fixtures (46 files), `4e72c56` JS numbers, `62e1b7a` loud checks, `b526202` recapture; key script 475 keys, 0 problems vs `9450c94b`.]

### Task 20: Flags, divergences, docs

- [x] `routes/system.rs`: `"mixes": true`; `changelog` stays `false` while kima-rs's own catalog is empty (Q3 = B; a test pins that it flips with the first entry). Tests `features_reports_mixes`, `features_changelog_off_while_catalog_empty`.
- [x] D-260.. rows final; README; roadmap row M5b; `docs/STATUS.md`; deployment note `TZ=Europe/Warsaw` (Q11 = A) in the README/compose docs.
- [x] Commit `docs(m5b): divergences, flags and status`. [Reconciled 2026-11-22: `af15de6` flags (RED 3 failed → GREEN), `c8f1ee6` docs.]

### Task 21: Verification and hand-off

- [x] Fresh parent-made container; full bounded workspace pass (fmt, clippy stable ±pg-publish, sqlx check, all test binaries in batches); upgrade path 0062 → 0063 on a copy with M5/M-PROF data; independent review until 0 Critical/Important; hosted CI on the pushed tip after the owner's go-ahead. [Reconciled 2026-11-22 from the Phase E evidence: full pass on `62e1b7a` 3806 ok / 49 ignored / 0 failed / 0 missing on the guarded isolated cluster (not a fresh container); upgrade 0061/0062 → 0065 on a restored copy of the populated app dump; review 0 Critical/Important; hosted CI run 45007673232 green; merged as `v0.2.0` (`7114062`).]
- [ ] Hand-off to M10: page list above, fixtures path, D-rows. [Open 2026-11-22: no separate M10 hand-off note found in the Phase E evidence; the page table above, `crates/kima-api-types/tests/fixtures/m5b_*` and D-260..D-282 are its inputs.]

### Phase F: thin kima-web slice (Q10 = B, owner 2026-11-19)

Task 22 kima-web API client + `/radio` page [done locally on `feature/m5b-web` 2026-11-22, D-282; real-server browser/playback run pending]; Task 23 home "Made for you" row + `/mix?id=`; each with `vertigo build`, CSS `--check`, e2e via `tools/verify-kima-web-e2e.mjs` (follow the skill's `references/kima-web-pages.md`). MoodMixer, what's-new window, changelog page, home radio row, for-you artists and the artist radio button stay M10.

## Ordering, size and risk

T0 → T1 → T2/T3 (parallel) → T4 → T5/T6 → T7 → T8 → T9 → T10 → T11 → T12 → T13 → T14 → T15 → T16 → T17 → T18 → T19 → T22 → T23 → T20 → T21. Estimate: T2–T4 3 d (goldens + recipe SQL are the risk), T5–T8 3 d, T9–T10 2 d, T11–T14 3 d, T15–T18 2.5 d, T19–T21 2 d, Phase F (T22–T23) ~3 d ≈ **18.5 working days (~3.5–4 weeks with review rounds)**, above the roadmap's "~1 week" (the roadmap predates the recipe engine inventory). Biggest risks: recipe SQL parity (63 upstream integration tests), station order seam/replay rules, and that every recipe output stays unobservable on real data until the mood heads are unblocked.

## Definition of done

1. All 22 endpoints served, each with its upstream test family ported; `"mixes": true`.
2. Goldens from the pin for draws, LCG, station order; vendored recipe/definition files hash-pinned.
3. Degradation matrix proven by tests with heads absent (the live state).
4. Profile isolation tests for every personal table and cache.
5. Wire fixtures for all 22 endpoints, keys checked against upstream handlers.
6. D-260.. rows written; full local workspace pass; independent review 0 Critical/Important; hosted CI green before merge.
7. Phase F (Q10 = B): `/radio`, home "Made for you" row and `/mix?id=` in kima-web, `vertigo build` + CSS `--check` + browser e2e green.

## Rozstrzygnięte (owner 2026-11-19; formerly "Otwarte pytania")

Questions kept as asked (recommendation first, upstream behaviour recorded as default); the binding answer follows each one and is repeated in "Owner decisions" 6–16.

1. **Q1 Redis state.** Upstream keeps mix lists (1 h, per zone), station sessions (12 h, 3 per listener), familiarity scans (3 h, lock), the energy band and radio covers (per local day) in Redis. kima has no Redis (D-003). Options: (A) in-process TTL store in `AppState`, lost on restart (upstream already rebuilds a lost session with `replayPasses` and a lost list from the day seed; only a manual refresh's list reverts to the day's list after a restart), like D-014; (B) Postgres tables (survive restarts, one more migration, write traffic on every radio refill). **Recommendation: A**, D-row. **Decision (owner 2026-11-19): A** — in-process TTL store in `AppState` (D-260).
2. **Q2 Mood and Workout radio tiles while mood heads are blocked.** Upstream shows them whenever `vibeEmbeddings` is true (`radioRow.ts:87-91`); with no heads each plays an empty list. Options: (A) upstream gate (tiles visible, empty stations); (B) kima gate on "mood statistics available" (tiles hidden until heads are unblocked, D-row). **Recommendation: B** (no dead tiles; becomes identical to upstream once heads exist). **Decision (owner 2026-11-19): B** — tiles only while mood statistics are available (D-266).
3. **Q3 Release catalog content.** Upstream's `RELEASES` (`catalog.ts:112-539`) is the upstream maintainer's own notes for upstream versions 1.10.1–1.12.3 (engine wipe, re-file, GPU consent, social/commons choices); `9450c94b` already changed its text. Options: (A) port verbatim and refresh on each re-pin; (B) port the mechanism exactly but ship kima-rs's own catalog, starting empty (`pending` → `[]`, `current` → 404, `changelog` → `{releases:[],openCount:0}`), `changelog` feature flag off until the first kima-rs entry; (C) skip what's-new except the mood-mix routes. **Recommendation: B** (D-row); upstream texts describe actions kima-rs users never needed. **Decision (owner 2026-11-19): B** — same mechanism, kima-rs's own catalog, empty at the start; `changelog` flag off until the first entry (D-267).
4. **Q4 Update check target.** Upstream asks `api.github.com/repos/Chevron7Locked/kima-hub/releases/latest`. Options: (A) same URL (reports upstream versions to a kima-rs user); (B) `d1mmu-dev/kima-rs-hub` releases through `kima-net` with upstream's enable switch; (C) always `enabled:false`. **Recommendation: B.** **Decision (owner 2026-11-19): B** — `d1mmu-dev/kima-rs-hub` releases through `kima-net`, upstream's on/off switch (D-270).
5. **Q5 GPU choice (`gpu.accelerate`).** kima's CLAP provider is env-configured (`KIMA_ONNX_PROVIDER`, D-252); there is no runtime toggle. Options: (A) no catalog item asks it, so `/choice` with that key answers 400 `Unknown choice` naturally; (B) implement a runtime toggle. **Recommendation: A** (D-row), revisit with M12. **Decision (owner 2026-11-19): A** — no GPU choice, `gpu.accelerate` → 400 `Unknown choice`; revisit with M12 (D-268).
6. **Q6 Track rows in `GET /mixes/{id}` and mood-bucket replies.** Upstream sends full Prisma `Track` rows with `album` (+ `artist{id,name,mbid}`) — dozens of columns kima does not have. Options: (A) reuse the existing kima library track DTO (as `/library/tracks`) plus `album.artist{id,name,mbid}`; (B) mirror every upstream column, null where absent. **Recommendation: A**, keys the frontend reads checked by Task 19, D-row. **Decision (owner 2026-11-19): B, not the recommendation** — every upstream Prisma `Track` column (and the included `album` and `album.artist{id,name,mbid}`), null where kima has no data; inventory and mapping in Appendix D, D-264. The "Recommendation: A" above is historical only.
7. **Q7 Steady-beat threshold.** Upstream counts a tempo only when Essentia's `bpmConfidence ≥ 2` (scale 0–5.32); kima's `track_analysis.tempo_confidence` is 0–1 from a different estimator. Options: (A) linear rescale (≥ 2/5.32 ≈ 0.376); (B) calibrate on the owner's library (fraction of tracks passing should match upstream's typical share) before fixing the number; (C) ignore confidence. **Recommendation: A now with a D-row, B as a follow-up measurement** — this is genuinely undecided and changes Dance Floor, Tempo Flow and night pace. **Decision (owner 2026-11-19): A** — `≥ 2/5.32 ≈ 0.376` (D-262); calibration on the owner's library is a noted follow-up.
8. **Q8 Missing `energy`/`danceability`.** Used by artist radio scoring and the night energy band. Options: (A) treat as absent (upstream's null paths: term skipped, band 0.5), as M5 did for the map; (B) substitute CLAP `arousal_raw` for energy. **Recommendation: A** (faithful null path, D-row). **Decision (owner 2026-11-19): A** — absent, upstream's null paths (D-263). **Correction (owner 2026-11-20):** Q8 premise corrected; the night energy band uses CLAP arousal like upstream (it reads `Track.arousal`, not `energy`); D-263's band half resolved.
9. **Q9 Release acknowledgements/choices scope.** Upstream is per user. Options: (A) per profile (M-PROF rule for personal data; admin-only choices only for the main profile); (B) per account. **Recommendation: A.** **Decision (owner 2026-11-19): A** — per profile; admin-only choices → main profile (D-269).
10. **Q10 kima-web pages.** Roadmap puts mixes/radio pages in M10; M6 precedent pulled podcast pages forward so the owner could use them. Options: (A) M10 as planned (M5b is invisible in the app until then, nothing in Subsonic uses it); (B) optional Phase F in M5b: `/radio` page + home "Made for you" row + mix page (~3 d), the rest (MoodMixer, what's-new window, changelog) in M10. **Recommendation: B** if the owner wants to use radio/mixes before M10, otherwise A. **Decision (owner 2026-11-19): B** — Phase F in M5b (`/radio`, "Made for you", mix page); the rest M10.
11. **Q11 Mix day zone.** Upstream's mix day is the **server's** calendar date (`mixDay.ts:8-11`); kima's container runs in UTC unless `TZ` is set, so mixes would roll over at 01:00/02:00 Polish time. Options: (A) upstream (server local, document setting `TZ`); (B) use the request's `?tz=` for the day too. **Recommendation: A** (faithful; deployment note to set `TZ=Europe/Warsaw`). **Decision (owner 2026-11-19): A** — server calendar date; deployment note `TZ=Europe/Warsaw`.

## Appendix C: Not verified [?]

- `currentReleaseFor`/`changelogFor` bodies (`releases/index.ts:68-271`) were read only for their signatures and return shapes; Task 15 reads them in full.
- `stationCovers.ts:41-145`, `familiarity.ts:56-219`, `nightPlays.ts:60-331`, `stationOrder.ts:79-399`, `discoveryMixes.ts`, `genreMixes.ts` bodies were read by header/exports only.
- REST API keys: D-073 says `X-API-Key` on the REST API is M8 ("App tokens work on `/rest` only until M8"), so `requireAuthOrToken` vs `requireAuth` is not observable yet; Tasks 9/15 use `AuthUser` and cite D-073 (the `api_key_auth_allowed`/`jwt_only_auth` tests become M8 hand-off notes, not M5b tests).
- The existing `/library/genres` artist-name filter's divergence row (candidates D-114/D-166) — Task 11 confirms.

## Appendix D: track row column map (Q6 = B, D-264)

Upstream [V `routes/mixes.ts:93-109` `loadTracksInMixOrder`, used by `GET /mixes/:id`, `GET /mixes/mood/buckets/:mood` and `POST /mixes/mood/buckets/:mood/save`]: `prisma.track.findMany({ where: { id: { in }, ...LIBRARY_PLAYABLE }, include: { album: { include: { artist: { select: { id, name, mbid } } } } } })`. Prisma returns every scalar field of the model and omits `Unsupported` ones (`searchVector`) and relations that are not included. Inventory read from `backend/prisma/schema.prisma` at `c7527577` (`model Track` `:671-760`, `model Album` `:10-46`); the reference checkout `9450c94b` has no `prisma/` change. JSON keys keep upstream's names and order; a key is **never omitted**: where kima has no data its value is `null` (also for upstream's non-nullable columns, e.g. `essentiaGenres`, `scanStatus`, `primaryType`). Upstream cuid ids are kima UUIDs (D-016). "stage(x)" = the `track_stages` row of stage `x` (`analysis`, `embedding`); status map `pending`/`queued` → `"pending"`, running → `"processing"`, done → `"completed"`, failed → `"failed"` (Task 9 confirms kima's exact stage status values and records the map in D-264).

**Track (58 scalar keys + `album`)**

| Upstream | kima-rs | | Upstream | kima-rs |
|---|---|---|---|---|
| `id` | `tracks.id` | | `danceabilityMl` | `track_analysis.musicnn_danceability` (unprovisioned → null) |
| `albumId` | `tracks.album_id` | | `moodTags` | `tracks.mood_tags` (0063, Task 5) |
| `title` | `tracks.title` | | `essentiaGenres` | null |
| `trackNo` | `tracks.track_no` | | `analysisStatus` | stage(analysis) status (map above); null without a row |
| `duration` | `tracks.duration_secs` | | `analysisVersion` | null |
| `mime` | null (kima stores `codec`, not a MIME type; Task 9 may map it through an existing codec→MIME helper if one exists, recorded in D-264) | | `analysisMode` | null |
| `filePath` | `tracks.path` (relative to the music root, as upstream) | | `analyzedAt` | stage(analysis) finish time when done, else null |
| `fileModified` | `tracks.mtime` | | `analysisError` | stage(analysis) last error, else null |
| `fileSize` | `tracks.size` | | `analysisRetryCount` | stage(analysis) attempts, else null |
| `bpm` | `track_analysis.tempo_bpm` | | `updatedAt` | `tracks.updated_at` |
| `bpmConfidence` | `track_analysis.tempo_confidence × 5.32` (the Q7 = A linear map back to upstream's scale) | | `displayTitle` | `tracks.display_title` |
| `beatsCount` | null | | `displayTrackNo` | null |
| `key` | `track_analysis.key_tonic` as a pitch-class name (Task 9 matches upstream's Essentia spelling) | | `hasUserOverrides` | `tracks.display_title IS NOT NULL` |
| `keyScale` | `track_analysis.key_mode` | | `analysisStartedAt` | stage(analysis) start time while running, else null |
| `keyStrength` | `track_analysis.key_strength` (different estimator, D-008) | | `vibeAnalysisStatus` | stage(embedding) status (map above); null without a row |
| `energy` | null (Q8 = A, D-263) | | `vibeAnalysisStartedAt` | stage(embedding) start time while running, else null |
| `loudness` | `track_analysis.integrated_lufs` (upstream: integrated EBU R128, `analyzer.py:598`) | | `vibeAnalysisError` | stage(embedding) last error, else null |
| `dynamicRange` | `track_analysis.loudness_range_lu` (upstream: loudness range, `analyzer.py:599`) | | `vibeAnalysisRetryCount` | stage(embedding) attempts, else null |
| `danceability` | `track_analysis.musicnn_danceability` (unprovisioned → null; Q8 = A) | | `vibeAnalysisStatusUpdatedAt` | stage(embedding) last change time, else null |
| `valence` | `track_clap_readings.valence_raw` (current reading; upstream `syncSharedValenceArousal` COALESCEs to the CLAP raw value when there is no DEAM answer, kima has no DEAM; review I1, D-264) | | `isrc` | `tracks.isrc` |
| `arousal` | `track_clap_readings.arousal_raw` (current reading, as `valence`) | | `isrcSource` | `tracks.isrc_source` |
| `deamValence` | null | | `acoustidFingerprint` | null (kima keeps no Chromaprint string) |
| `deamArousal` | null | | `recordingMbid` | `tracks.recording_mbid` |
| `clapValenceRaw` | `track_clap_readings.valence_raw` (current reading only) | | `corrupt` | `false` (only playable rows are returned; kima has no corrupt flag, D-261) |
| `clapArousalRaw` | `track_clap_readings.arousal_raw` (current reading only) | | `missingSince` | `tracks.missing_since` (always null here: present tracks only) |
| `instrumentalness` | `track_clap_readings.instrumentalness` (current reading only) | | `scanStatus` | null |
| `acousticness` | `track_clap_readings.acousticness` (current reading only) | | `scanError` | null |
| `speechiness` | null | | `scanStartedAt` | null |
| | | | `discNumber` | `tracks.disc` |
| | | | `discSubtitle` | null |


**album (20 scalar keys + `artist`)**

| Upstream | kima-rs | | Upstream | kima-rs |
|---|---|---|---|---|
| `id` | `albums.id` | | `label` | `albums.label` |
| `rgMbid` | `albums.release_group_mbid` (nullable in kima) | | `genres` | null |
| `rgMbidSource` | `albums.release_group_mbid_source` (kima's value set, D-row of 0030) | | `lastSynced` | null |
| `artistId` | `albums.artist_id` | | `location` | `"LIBRARY"` (every kima album is a library album until M7, D-261) |
| `title` | `albums.title` | | `displayTitle` | `albums.display_title` |
| `identityKey` | null | | `displayYear` | `albums.display_year` |
| `sortName` | `albums.sort_name` | | `hasUserOverrides` | any of `display_title`, `display_year`, `user_genres`, user image set |
| `year` | `albums.year` | | `userCoverUrl` | album cover token when the image is the user's (`image_source = 'user'`), else null |
| `coverUrl` | album cover token (album UUID when an image exists, else null; D-265, as D-185/D-193) | | `userGenres` | `albums.user_genres` |
| `primaryType` | null | | `originalYear` | null |
| | | | `artist` | `{id: artists.id, name: artists.name, mbid: artists.mbid}` |
