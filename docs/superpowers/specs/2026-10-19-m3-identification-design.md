# M3: Identification — Design

Date: 2026-10-19
Status: Draft, controller calls C1–C19 folded in, pending builder check and owner approval
Parent spec: `docs/superpowers/specs/2026-10-16-kima-rust-design.md` (§2 `kima-ident`, §3 `track_stages`, §4 jobs,
§8 row M3, §10 outbound HTTP and stage failures, §11 testing)
Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` (row M3, "Notes carried forward")
Builds on: M0, M1 and M2 as merged on `main` (HEAD `a0b2959`): crates kima-core, kima-db, kima-jobs, kima-library,
kima-api, kima-subsonic, kima-server; migrations `0001`–`0014`; the forked Next.js frontend as a static export.
Upstream reference: kima-hub @ `12faf1cd` (`backend/src/services/{musicbrainz,acoustidService,acoustidIdentification,
recordingMatch,albumIdentity,artistIdentity,artistCorrection,wikidata,fanart,artwork,artistSimilarity,listenBrainz,
lrclib,fpcalc,enrichmentErrorCodeClassifier}.ts`, `backend/src/workers/{unifiedEnrichment,artistEnrichment,
stageRetry}.ts`, `backend/src/routes/{enrichment,systemSettings,library/tracks,library/artists}.ts`,
`backend/src/routes/subsonic/{library,search,lyrics,artistInfo,playback}.ts`).

Calls marked **(controller's call)** are the binding calls C1–C19 and the builder-check rulings R1–R4, all listed in §28; they are choices, not
upstream parity. Everything else is either upstream parity (cited) or a call this design makes, listed in §28.

## 1. Intent

After M2 the library plays in the browser and in Subsonic apps, but kima-rs knows only what the file tags and folder
names say. Upstream kima fills the gaps from the open music databases: it identifies each track as a MusicBrainz
recording, shows artist biographies, photos and similar artists, lists genres, and shows synced lyrics. M3 brings
that to kima-rs, and then goes one step further than upstream: a beets-style **autotagger** that matches a whole
album against a MusicBrainz release, shows the proposed changes, and on approval writes correct tags into the files
and re-organizes them.

The owner's library is the use case. A typical folder holds tracks ripped years ago, some with Picard tags, most
with hand-typed tags or none. After M3a, without the owner doing anything, the artist pages show a photo, a bio and
similar artists; the lyrics panel shows synced lyrics when LRCLIB has them; Subsonic apps show genres and artist
info; and every track that can be identified carries its MusicBrainz recording id. After M3b, the owner opens
**Settings > Library Tools > Match albums**, picks an album flagged in the needs-attention report, sees "MusicBrainz
release *X*, distance 0.02 (strong)" with a per-track diff, clicks Accept, and the files carry Picard-compatible
tags and sit at their organized paths. One click undoes it.

Why this matters:
- Identification is the foundation of everything after it: M4's analysis and M5's vibe engine use MusicBrainz genres;
  M7's integrations (Lidarr, ListenBrainz) need MBIDs.
- Upstream never writes tags (`B/` has no tag writer), so a messy library stays messy. The autotagger is the owner's
  request (roadmap M3 notes) and is the main thing kima-rs adds over upstream here.
- Nothing in M3 may make the library less safe than M1 left it: the read-only default stays read-only (§3).

Success:
- M3a: on a fixture library with Picard-tagged and untagged tracks, every tagged track has `recording_mbid` with
  source `file_tag` without any network request; untagged tracks are identified by ISRC, name search or AcoustID
  against recorded fixtures (AcoustID only with a configured key, see below); the Library Enrichment panel shows real progress, failures and working pause/resume/
  stop; an artist page shows bio, image and similar artists; `GET /api/library/tracks/:id/lyrics` returns synced
  lyrics; Subsonic `getGenres`, `getSongsByGenre`, `getArtistInfo2`, `getLyricsBySongId` and artist `getCoverArt`
  return real data; with `KIMA_IDENTIFY=off` not one outbound request is made.
- M3b: an untagged copy of a recorded MusicBrainz release is matched with a strong recommendation; Accept writes the
  tags (round-trip verified for MP3, FLAC, Ogg/Opus and M4A); the files are re-organized; undo restores every touched
  tag value and path; the audio stream of every file is byte-identical before and after.

## 2. Goals

From the parent spec's M3 row: MusicBrainz, AcoustID via rusty-chromaprint, artist metadata enrichment, genres,
lyrics (LRCLIB), enrichment stage tracking. Split into two parts **(controller's call, C1)**, written as one design
and executed as two plans in order:

**AcoustID key consequence (controller's call, C8, pending the owner registering a kima-rs key).** The key order is
setting, then env `ACOUSTID_CLIENT_KEY`, then a built-in default. Upstream's default is the key AcoustID issued to
kima-hub, which kima-rs does not reuse, so kima-rs ships with an **empty** built-in default until the owner registers a
kima-rs application (§27 Q1). Until an admin enters a key in Settings > Artwork (or sets the env var), the AcoustID
rung is skipped (no request is made; the recording stage closes "no match" with `evidence.acoustid = "skipped"`,
reason "no AcoustID key"), and untagged tracks that ISRC and name search cannot place stay unidentified. Entering a key later and
pressing "Retry What Didn't Work" reopens exactly those recordings (§10.7). Fingerprints are still computed meanwhile.

**M3a, upstream parity:**
- An outbound HTTP layer with per-host rate limits, a Postgres response cache and upstream's outcome type (§7).
- Scanner reads MusicBrainz ids, ISRC, genres and embedded lyrics (§8.1).
- Track identification: upstream's ladder and gates exactly (§8).
- In-process chromaprint fingerprinting (§9).
- The per-track stage ledger `track_stages`, its state machine, retries and failure classes, and the enrichment
  coordinator behind upstream's `/api/enrichment/*` controls (§10).
- Job lease heartbeat, delayed enqueue and non-retryable job errors (§11).
- Artist enrichment: MBID resolution, bio, image, MusicBrainz genres, similar artists; album cover fallback (§12).
- Genres (§13), lyrics (§14).
- Settings: an encrypted key-value store, the AcoustID and fanart.tv keys, `test-fanart`, the identification switch
  (§15).
- The routes the frontend calls in upstream's shapes (§16), the `enrichment:progress` event (§17), the Subsonic
  fill-ins (§18).
- The M2 leftovers (§25) and parked M1 minors 3–4 (§11.4).

**M3b, the autotagger:**
- Album matching against MusicBrainz releases with a beets-modelled distance (§19.2–§19.4).
- An admin review page with candidates, proposed changes and a track mapping diff (§19.5, §21).
- Apply: tag writing (files), tagging the organized copy (copy) or database-only, journalled with undo (§19.6–§19.9).
- The organizer rework, including parked M1 minors 2 and 5–8 (§20).

## 3. Non-goals and anti-goals

Non-goals (M3):
- Automatic album identification in M3a. Like upstream, the automated pipeline sets album release and release-group
  MBIDs only from file tags; the autotagger is how albums get matched **(controller's call, C12)**.
- Merging or splitting artists by MBID. Artists stay keyed by `name_key` (D-039); an MBID is an attribute.
- Last.fm. Upstream removed it (`B/services/releases/catalog.ts:226`) **(controller's call, C8)**.
- Discovery pages (`/api/artists/discover/*`, `/api/artists/album/*`), artist discography from MusicBrainz for unowned
  albums, Deezer previews: they belong to discovery, which stays off (D-050, `features.discover:false`).
- Similar songs (`getSimilarSongs*`), radio and the `SimilarArtist` relation table: M5 **(controller's call, C11)**.
- Audio analysis and embeddings: M4. Their stage names are reserved in `track_stages` (§6.4).
- Admin notifications (upstream raises one after a source has been paused 24 h, and a completion alert): kima-rs has
  no notification system until M8.
- Reading `.lrc` sidecar files as lyrics (upstream does not). The organizer moves them (§20).
- Spotify-import track matching (`B/services/trackMatching.ts`): not identification (survey §1.5); M7.
- A vertigo (`kima-web`) page. M9 does not exist yet; the M10 rule does not apply **(controller's call, C16)**.

Anti-goals (things M3 must not do):
- **Never write to a read-only library.** Tag writing to library files happens only when the library root passes the
  writability probe, and only on an explicit admin action (or the off-by-default auto-apply setting). Copy mode
  writes to the organized copies only; database-only writes no file **(controller's call, C14)**.
- **Never change audio.** Every tag write is checked by hashing the audio packets before and after (§19.8); a
  mismatch aborts that file and leaves it untouched.
- **Never hit external APIs in tests.** Every outbound call goes through `kima-net`, whose base URLs are injected; CI
  runs with a guard that fails any test touching a real host (§24.1).
- **Never block playback on enrichment.** Page views and streams make no outbound request and take no lock the
  pipeline holds. The only request-time lookup is lyrics, bounded to 5 s, answered from storage when it times out
  (§14.2). Fingerprinting runs at most half the cores (§9.3).
- **Never record a failure as an absence.** A source that did not answer leaves the question open (upstream's rule,
  `B/utils/outcome.ts`); only an answered "no" is cached or closes a stage.
- **Never cache a failure.** Only answers (30 days) and absences (7 days) are cached **(controller's call, C6)**.
- **Never send a placeholder or local id to a source.** Every lookup takes a parsed `Mbid` (a `uuid::Uuid`), never a
  string, so a non-MBID cannot reach a URL.
- **Never echo a stored secret.** API keys are write-only (§15.2).
- **Never fill album identity fields from the pipeline.** Album title, artist and year are identity (D-039); only the
  autotagger changes them, in place on the same rows and never by merging (§19.7).
- **Never purge a track whose only fault is being a symlink** (parked minor 2, §20.6).

## 4. The M3a/M3b split

**(controller's call, C1)**. One design, two plans, executed in order. M3b starts after M3a is merged.

| | M3a | M3b |
|---|---|---|
| Scope | §7–§18, §25, §11 | §19, §20, the autotag pages in §21 |
| Migrations | `0015`–`0020` (§6) | `0021`–`0022` (§6.8, §6.9) |
| New crates | `kima-net`, `kima-ident` | none (adds `kima_ident::autotag`, `kima_library::tagwrite`) |
| Depends on | M2 | M3a's MusicBrainz client, recording MBIDs, `track_stages`, heartbeat, scanner MBID columns |
| Divergences | D-106–D-118, D-124 | D-119–D-123 |
| Done when | §1 M3a success, CI green, rows logged | §1 M3b success, CI green, rows logged |

Why this order: M3b's candidate search uses the recording MBIDs M3a computes (tracks whose recording is known pull
in the releases that contain them), its tag writer is driven by the same MusicBrainz client, and its long apply
jobs need the lease heartbeat.

## 5. Architecture and crate layout

```
kima-core     (cipher, events, config)               unchanged API; config gains §22 vars
kima-db       (sqlx queries + migrations)            + settings_kv, http_cache, source_pauses, stages, genres,
                                                       lyrics, identity columns, autotag tables
kima-jobs     (queue, worker, scheduler)             + heartbeat, enqueue_at, JobError::Permanent (§11)
kima-net      NEW: outbound HTTP                     reqwest+rustls client, per-source limits, cache, Outcome
kima-library  (scanner, artwork, organizer)          + reads MBIDs/ISRC/genre/lyrics; tagwrite (M3b);
                                                       organizer rework (M3b); artist image serving
kima-ident    NEW: identification + enrichment       MusicBrainz, AcoustID, chromaprint, matching, stage
                                                       ledger, coordinator, artist enrichment, lyrics, autotag
kima-api      /api handlers                          enrichment, lyrics, genres, settings, autotag routes
kima-subsonic /rest                                  genre/lyrics/artist-info fill-ins
kima-server   wiring                                 Net built once; enrich + autotag queues; scheduler entries
```

Crate split **(controller's call, C6: "the builder picks")**; this design picks two crates:
- **`kima-net`** holds only the HTTP layer (§7): it depends on `kima-db` (for the cache and pause tables) and
  `kima-core`. It knows sources and limits but nothing about music. Separate because M7's integrations reuse it
  without pulling in identification.
- **`kima-ident`** holds every music-domain lookup and decision: source clients (`musicbrainz`, `acoustid`,
  `wikidata`, `fanart`, `deezer`, `listenbrainz`, `lrclib`, `coverartarchive`), `chromaprint`, `text` (folding and
  agreement), `matching` (the ladder), `stages` (the ledger), `coordinator`, `artist` (enrichment), `lyrics`,
  `autotag` (M3b matching). It depends on `kima-net`, `kima-db`, `kima-jobs`, `kima-library`, `kima-core`, never on
  axum (parent spec §2 boundary rule).
- File writing stays in **`kima-library`** (`tagwrite`, M3b), next to lofty reading, the `IgnoreSet` and the
  organizer, so `kima-library` never depends on `kima-ident`. The autotag apply job is a `library` queue job
  (§19.9): it reads the chosen proposal from the database, so it needs no ident code.

Process wiring (`kima-server/src/main.rs`):
- `Net::new(cfg, pool, identify_switch)` is built once and shared (`Arc<Net>`) by `AppState` (new field `net`) and
  the ident job context.
- `IdentCtx { pool, net, cipher, events, library: LibraryCtx, settings: SettingsCache }` is the job context. It
  carries the cipher (the survey's hazard 7: jobs had none).
- Queues: `library` (existing; gains `autotag-apply`/`autotag-undo` kinds), `enrich` (concurrency 1, §10.6),
  `autotag` (concurrency 1, M3b search jobs, §19.2).
- The scheduler is restructured so it always runs (today it is built only when `KIMA_LIBRARY_SYNC_CRON` is set,
  survey §4): entries `library-sync` (optional), `http-cache-prune` (`17 3 * * *`, always), `enrich-kick`
  (`*/10 * * * *`, always: enqueues a cycle if none is queued, a backstop for a lost self-reschedule).

## 6. Schema (migrations from `0015`)

The highest existing migration is `0014_user_settings.sql`. M3a adds `0015`–`0020`, M3b `0021`–`0022`. MBIDs are
stored as `uuid` (validated and compact; a placeholder cannot be stored). Timestamps are `timestamptz`.

### 6.1 `0015_settings_kv.sql`

```sql
-- Key-value system settings; secrets encrypted with kima_core::cipher (AAD "settings_kv:" || key).
CREATE TABLE settings_kv (
    key        text PRIMARY KEY CHECK (key ~ '^[a-zA-Z][a-zA-Z0-9]{0,63}$'),
    value      jsonb,          -- plain settings
    secret_enc bytea,          -- encrypted settings
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(value, secret_enc) = 1)
);
```

Keys and types are fixed in code (`kima_db::settings_kv::Key`), not free-form: §15.1 lists them. Absent row = the
default. `system_settings` (single row, `registration_open`, `library_root`) is left as it is.

### 6.2 `0016_http_cache.sql`

```sql
CREATE TABLE http_cache (
    key        text PRIMARY KEY,               -- "<source>:" || hex(sha256(canonical request))
    source     text NOT NULL,                  -- musicbrainz | acoustid | wikidata | fanart | deezer |
                                               -- listenbrainz | coverartarchive
    request    text NOT NULL,                  -- canonical request, for debugging (secrets stripped)
    outcome    text NOT NULL CHECK (outcome IN ('answered', 'absent')),
    body       jsonb NOT NULL,                 -- the parsed answer, or the absence evidence
    fetched_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX http_cache_expires_idx ON http_cache (expires_at);

-- A source that rate-limited us; survives restarts.
CREATE TABLE source_pauses (
    source       text PRIMARY KEY,
    paused_until timestamptz NOT NULL,
    reason       text NOT NULL,                -- e.g. "HTTP 503", "Retry-After 20"
    consecutive  int NOT NULL DEFAULT 1,       -- consecutive rate-limited outcomes
    since        timestamptz NOT NULL DEFAULT now()
);
```

### 6.3 `0017_identity.sql` (identity columns and user overrides)

```sql
ALTER TABLE artists
    ADD COLUMN mbid                uuid,
    ADD COLUMN mbid_source         text CHECK (mbid_source IN ('tag', 'search', 'user', 'autotag')),
    ADD COLUMN bio                 text,          -- Wikipedia summary (upstream Artist.summary)
    ADD COLUMN image_path          text,          -- relative to <data_dir>/artist-images
    ADD COLUMN image_source        text CHECK (image_source IN ('local', 'wikidata', 'fanart', 'deezer', 'user')),
    ADD COLUMN similar             jsonb,         -- [{name, mbid|null, match}] (upstream similarArtistsJson)
    ADD COLUMN enrichment_status   text NOT NULL DEFAULT 'pending'
        CHECK (enrichment_status IN ('pending', 'enriching', 'completed', 'failed', 'unresolvable')),
    ADD COLUMN enrichment_failures smallint NOT NULL DEFAULT 0,
    ADD COLUMN last_enriched_at    timestamptz,
    ADD COLUMN last_error          text,          -- last enrichment failure, for the failures list
    ADD COLUMN last_error_code     text,
    ADD COLUMN display_name        text,          -- user overrides (MetadataEditor)
    ADD COLUMN user_bio            text,
    ADD COLUMN user_genres         text[];
CREATE INDEX artists_mbid_idx ON artists (mbid) WHERE mbid IS NOT NULL;
CREATE INDEX artists_enrichment_idx ON artists (enrichment_status, last_enriched_at);

ALTER TABLE albums
    ADD COLUMN release_mbid       uuid,           -- from tags, the autotagger or a user (upstream stores none)
    ADD COLUMN release_mbid_source text CHECK (release_mbid_source IN ('tag', 'autotag', 'user')),
    ADD COLUMN release_group_mbid uuid,           -- upstream Album.rgMbid
    ADD COLUMN release_group_mbid_source text CHECK (release_group_mbid_source IN ('tag', 'autotag', 'user')),
    ADD COLUMN label              text,
    ADD COLUMN image_path         text,           -- downloaded cover, relative to <data_dir>/album-images
    ADD COLUMN image_source       text CHECK (image_source IN ('deezer', 'coverartarchive', 'fanart', 'user')),
    ADD COLUMN display_title      text,
    ADD COLUMN display_year       int CHECK (display_year BETWEEN 1 AND 9999),
    ADD COLUMN user_genres        text[];
CREATE INDEX albums_release_group_idx ON albums (release_group_mbid) WHERE release_group_mbid IS NOT NULL;
CREATE INDEX albums_release_idx ON albums (release_mbid) WHERE release_mbid IS NOT NULL;
-- albums.cover_path (0004, never used) is dropped:
ALTER TABLE albums DROP COLUMN cover_path;

ALTER TABLE tracks
    ADD COLUMN tag_recording_mbid     uuid,       -- as read from the file (§8.1)
    ADD COLUMN tag_release_track_mbid uuid,
    ADD COLUMN tag_release_mbid       uuid,
    ADD COLUMN tag_release_group_mbid uuid,
    ADD COLUMN tag_artist_mbids       uuid[] NOT NULL DEFAULT '{}',
    ADD COLUMN tag_album_artist_mbids uuid[] NOT NULL DEFAULT '{}',
    ADD COLUMN disc_total             int,        -- read from tags (organizer naming, §20.5)
    ADD COLUMN track_total            int,
    ADD COLUMN isrc                   text CHECK (isrc ~ '^[A-Z]{2}[A-Z0-9]{3}[0-9]{7}$'),
    ADD COLUMN isrc_source            text CHECK (isrc_source IN ('id3', 'musicbrainz')),
    ADD COLUMN recording_mbid         uuid,       -- the identification answer (upstream Track.recordingMbid)
    ADD COLUMN recording_source       text CHECK (recording_source IN
        ('file_tag', 'tracklist', 'isrc', 'search', 'acoustid', 'autotag'));
CREATE INDEX tracks_recording_mbid_idx ON tracks (recording_mbid) WHERE recording_mbid IS NOT NULL;
CREATE INDEX tracks_isrc_idx ON tracks (isrc) WHERE isrc IS NOT NULL;

-- The AcoustID fingerprint lives apart from tracks: ~3.4 kB per row, read only by the recording stage.
CREATE TABLE track_acoustid (
    track_id      uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    fingerprint   text NOT NULL,                  -- compressed, base64url (AcoustID's format)
    duration_secs int NOT NULL CHECK (duration_secs > 0),
    computed_at   timestamptz NOT NULL DEFAULT now()
);
```

Notes:
- **Not unique.** Upstream's `Artist.mbid` and `Album.rgMbid` are unique. kima-rs artists are keyed by name, and two
  rows ("Beyonce", "Beyoncé") can legitimately carry the same MBID from tags; a unique index would make the scanner
  fail. The `PUT .../metadata` routes still answer upstream's 409 when an admin types an MBID another row holds
  (§16.3). D-117.
- **Name collision avoided.** `tracks.fingerprint` (M1) is the cheap content hash; the audio fingerprint is
  `track_acoustid.fingerprint`.
- `artists.sort_name` stays generated from `name`; a `display_name` override does not re-sort (D-118).
- The artist named "Various Artists" (`names::canonical_various`) gets `mbid = 89ad4ac3-39f7-470e-963a-56509c546377`
  (MusicBrainz's VA id) and `enrichment_status = 'completed'` in the migration and on creation; it is never looked up.

### 6.4 `0018_track_stages.sql`

```sql
CREATE TABLE track_stages (
    track_id        uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    stage           text NOT NULL CHECK (stage IN
                        ('fingerprint', 'recording', 'analysis', 'embedding', 'similarity')),
    status          text NOT NULL CHECK (status IN ('pending', 'working', 'done', 'failed')),
    attempts        smallint NOT NULL DEFAULT 0,  -- reported failures; rate limits never count
    redelivered     smallint NOT NULL DEFAULT 0,  -- lapsed claims re-offered
    source          text,                         -- ffmpeg | file_tag | tracklist | isrc | search | acoustid
    last_error      text,                         -- raw message, <= 2000 chars
    error_code      text,                         -- TIMEOUT, HTTP_503, RATE_LIMITED, NO_CLIENT_KEY, LOST_WORKER, …
    error_class     text CHECK (error_class IN ('DAMAGED', 'UNSUPPORTED', 'MULTICANNEL', 'INCONCLUSIVE',
                                                'PERMANENT', 'FILE_MISSING', 'DEPENDENCY_CLOSED')),
    evidence        jsonb,
    next_attempt_at timestamptz,
    started_at      timestamptz,
    finished_at     timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (track_id, stage)
);
CREATE INDEX track_stages_due_idx ON track_stages (stage, status, next_attempt_at);

-- The coordinator's control state (upstream's Redis key enrichment:state). One row.
CREATE TABLE enrichment_state (
    id            boolean PRIMARY KEY DEFAULT true CHECK (id),
    status        text NOT NULL DEFAULT 'idle' CHECK (status IN ('idle', 'running', 'paused', 'stopping')),
    halted        boolean NOT NULL DEFAULT false,   -- set by stop, cleared by sync (§10.7)
    current_phase text CHECK (current_phase IN ('artists', 'tracks', 'fingerprint')),
    started_at    timestamptz,
    paused_at     timestamptz,
    stopped_at    timestamptz,
    last_error    text,
    last_activity timestamptz NOT NULL DEFAULT now()
);
INSERT INTO enrichment_state DEFAULT VALUES;
```

C5 lists `(track_id, stage, status, attempts, last_error, error_class, next_attempt_at, updated_at)`
**(controller's call)**. This design adds `redelivered`, `source`, `error_code`, `evidence`, `started_at`,
`finished_at`: each is read by an upstream behaviour M3a ports (redelivery cap, the failures list `errorCode` and
`metadata.source/evidence`, "closed records carry start stamp"). The `status` and `next_attempt_at` meanings are in
§10.2. The parent spec §3 called the stage `metadata`; it is `recording`, upstream's name, which the frontend's
failure mapping expects.

`error_class` keeps upstream's spelling `MULTICANNEL` (`B/services/enrichmentErrorCodeClassifier.ts:8`) because the
failures list returns it to the UI verbatim.

### 6.5 `0019_genres.sql`

```sql
CREATE TABLE genres (
    id   uuid PRIMARY KEY,
    name text NOT NULL,                             -- as MusicBrainz or the tag gives it, trimmed
    key  text NOT NULL UNIQUE                       -- lower(name), whitespace collapsed
);
CREATE TABLE track_genres (
    track_id uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    genre_id uuid NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
    source   text NOT NULL CHECK (source IN ('musicbrainz', 'file')),
    rank     smallint NOT NULL,                     -- 0 = most voted / first in the tag
    PRIMARY KEY (track_id, source, genre_id)
);
CREATE INDEX track_genres_genre_idx ON track_genres (genre_id);
CREATE TABLE album_genres  (album_id  uuid NOT NULL REFERENCES albums (id)  ON DELETE CASCADE,
                            genre_id uuid NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
                            source text NOT NULL CHECK (source IN ('musicbrainz', 'file')),
                            rank smallint NOT NULL, PRIMARY KEY (album_id, source, genre_id));
CREATE TABLE artist_genres (artist_id uuid NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
                            genre_id uuid NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
                            source text NOT NULL CHECK (source IN ('musicbrainz', 'file')),
                            rank smallint NOT NULL, PRIMARY KEY (artist_id, source, genre_id));
CREATE INDEX album_genres_genre_idx ON album_genres (genre_id);
CREATE INDEX artist_genres_genre_idx ON artist_genres (genre_id);

-- Effective genres: MusicBrainz when it has any for the entity, else the file's (§13.2).
CREATE VIEW track_genres_effective AS
SELECT g.track_id, g.genre_id, g.rank FROM track_genres g
WHERE g.source = 'musicbrainz'
   OR NOT EXISTS (SELECT 1 FROM track_genres m WHERE m.track_id = g.track_id AND m.source = 'musicbrainz');
-- album_genres_effective and artist_genres_effective: the same shape.
```

Unused genres are deleted by the daily `http-cache-prune` entry (`DELETE FROM genres WHERE NOT EXISTS (...)`).

### 6.6 `0020_lyrics.sql`

```sql
-- Upstream track_lyrics (schema.prisma:1069-1080), same column names.
CREATE TABLE track_lyrics (
    track_id      uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    plain_lyrics  text,
    synced_lyrics text,                             -- LRC text
    source        text NOT NULL CHECK (source IN ('embedded', 'lrclib', 'none')),
    lrclib_id     int,
    fetched_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
```

### 6.7 Jobs

No migration. Heartbeat renews `jobs.locked_until`; delayed enqueue writes `jobs.run_at`; a permanent error sets
`state='failed'` directly (§11).

### 6.8 `0021_autotag.sql` (M3b)

```sql
CREATE TABLE autotag_albums (
    album_id         uuid PRIMARY KEY REFERENCES albums (id) ON DELETE CASCADE,
    state            text NOT NULL CHECK (state IN ('queued', 'searching', 'proposed', 'no_candidates',
                                                    'failed', 'applying', 'applied', 'skipped')),
    recommendation   text CHECK (recommendation IN ('strong', 'medium', 'none')),
    best_candidate   uuid,                  -- FK added below (the tables reference each other)
    chosen_candidate uuid,
    error            text,
    searched_at      timestamptz,
    applied_at       timestamptz,
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX autotag_albums_state_idx ON autotag_albums (state, recommendation);

CREATE TABLE autotag_candidates (
    id                 uuid PRIMARY KEY,
    album_id           uuid NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    release_mbid       uuid NOT NULL,
    release_group_mbid uuid NOT NULL,
    origin             text NOT NULL CHECK (origin IN ('tag', 'search', 'recording', 'manual')),
    distance           double precision NOT NULL CHECK (distance BETWEEN 0 AND 1),
    recommendation     text NOT NULL CHECK (recommendation IN ('strong', 'medium', 'none')),
    penalties          jsonb NOT NULL,   -- {"album":0.0,"artist":0.02,"year":0.0,"tracks":0.01,…} weighted shares
    mapping            jsonb NOT NULL,   -- [{trackId, medium, position, releaseTrackMbid, distance}]
    extra_tracks       jsonb NOT NULL,   -- local track ids with no release track
    missing_tracks     jsonb NOT NULL,   -- release tracks with no local track: [{medium, position, title, length}]
    release            jsonb NOT NULL,   -- the release as fetched (§19.2)
    release_group      jsonb NOT NULL,   -- snapshot: {id, title, primaryType, firstReleaseDate, genres:[{name,count}]}
    fetched_at         timestamptz NOT NULL DEFAULT now(),  -- snapshot age (§19.2, R3)
    created_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (album_id, release_mbid)
);
ALTER TABLE autotag_albums
    ADD FOREIGN KEY (best_candidate)   REFERENCES autotag_candidates (id) ON DELETE SET NULL,
    ADD FOREIGN KEY (chosen_candidate) REFERENCES autotag_candidates (id) ON DELETE SET NULL;

-- Album identity set by the autotagger (§19.7, R1).
ALTER TABLE albums
    ADD COLUMN identity_source  text NOT NULL DEFAULT 'scan' CHECK (identity_source IN ('scan', 'autotag')),
    ADD COLUMN applied_identity jsonb,     -- {artistKey, titleKey, year} while identity_source = 'autotag'
    ADD CHECK ((identity_source = 'autotag') = (applied_identity IS NOT NULL));

CREATE TABLE autotag_runs (
    id               uuid PRIMARY KEY,
    album_id         uuid NOT NULL REFERENCES albums (id) ON DELETE CASCADE,  -- stable across apply/undo (R1)
    candidate_id     uuid REFERENCES autotag_candidates (id) ON DELETE SET NULL,  -- display only; apply copies it
    applied          jsonb,          -- the §19.6 values used, copied from the candidate at apply (undo needs no candidate)
    album_before     jsonb,          -- the album row's identity fields, MBIDs and sources before apply
    artist_action    text CHECK (artist_action IN ('none', 'reparented', 'created', 'renamed')),
    old_artist_id    uuid REFERENCES artists (id) ON DELETE SET NULL,
    new_artist_id    uuid REFERENCES artists (id) ON DELETE SET NULL,
    old_artist_name  text,           -- for undoing a rename in place
    target           text NOT NULL CHECK (target IN ('files', 'copy', 'db')),
    reorganize       boolean NOT NULL DEFAULT false,
    state            text NOT NULL CHECK (state IN ('queued', 'writing', 'organizing', 'done', 'failed',
                                                    'undoing', 'undone', 'undo_failed')),
    organizer_run_id uuid REFERENCES organizer_runs (id) ON DELETE SET NULL,
    copy_run_id      uuid REFERENCES organizer_runs (id) ON DELETE SET NULL,  -- copy target's run
    created_by       uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    finished_at      timestamptz,
    summary          jsonb,
    error            text
);

CREATE TABLE autotag_journal (
    id          bigserial PRIMARY KEY,
    run_id      uuid NOT NULL REFERENCES autotag_runs (id) ON DELETE CASCADE,
    track_id    uuid REFERENCES tracks (id) ON DELETE SET NULL,
    root        text NOT NULL CHECK (root IN ('library', 'copy')),
    path        text NOT NULL,                  -- relative to that root
    before_sha  bytea,                          -- whole file, before (NULL for target 'db')
    after_sha   bytea,                          -- whole file, after the write
    audio_md5   bytea,                          -- §19.8; equal before and after by construction
    old_tags    jsonb NOT NULL,                 -- native key -> [values] for every touched key; [] = absent
    new_tags    jsonb NOT NULL,
    id3_version text CHECK (id3_version IN ('v3', 'v4')),  -- the file's ID3v2 version before the write (R4)
    state       text NOT NULL CHECK (state IN ('planned', 'written', 'skipped', 'failed', 'undone',
                                               'undo_failed')),
    note        text,
    applied_seq bigint
);
CREATE SEQUENCE autotag_applied_seq;
CREATE INDEX autotag_journal_run_idx ON autotag_journal (run_id, applied_seq);

-- Database-only matches survive rescans: the scanner merges these over the file's tags (§19.7).
CREATE TABLE track_tag_overrides (
    track_id   uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    run_id     uuid NOT NULL REFERENCES autotag_runs (id) ON DELETE CASCADE,
    fields     jsonb NOT NULL,                  -- the RawTags fields to replace, camelCase (§19.6)
    created_at timestamptz NOT NULL DEFAULT now()
);
```

### 6.9 `0022_organizer_scope.sql` (M3b)

```sql
ALTER TABLE organizer_runs ADD COLUMN album_ids uuid[];   -- NULL = whole library (M1 behaviour)
ALTER TABLE organizer_runs ADD COLUMN origin text NOT NULL DEFAULT 'user'
    CHECK (origin IN ('user', 'autotag'));
ALTER TABLE organizer_runs DROP CONSTRAINT organizer_runs_state_check;   -- name as generated in 0008
ALTER TABLE organizer_runs ADD CONSTRAINT organizer_runs_state_check CHECK (state IN
    ('planning', 'previewed', 'applying', 'done', 'failed', 'undoing', 'undone', 'superseded'));
```

`superseded`: a `previewed` run whose plan a later autotag apply made stale (§19.7).

## 7. Outbound HTTP (`kima-net`)

**(controller's call, C6)** One shared client wrapper with per-host limits, timeouts, retries, a User-Agent and a
response cache, returning upstream's outcome type.

### 7.1 Client

- `reqwest` 0.12 with features `rustls-tls`, `json`, `gzip`; the workspace dependency gains them (today it has no TLS
  feature, survey §5). `ca-certificates` is already in the image (Dockerfile:13).
- User-Agent: `kima-rs/<CARGO_PKG_VERSION> ( <KIMA_CONTACT> )`, where `KIMA_CONTACT` defaults to
  `https://github.com/d1mmu-dev/kima-rs-hub` **(controller's call, C6)**. MusicBrainz's rules ask for exactly this
  shape. Sent on every request, including image downloads (upstream: `Kima/<v> (<repo>)`, `B/config.ts:79`).
- Redirects: at most 3, same scheme or http→https only. Response bodies capped at 4 MiB for JSON, 15 MiB for images.
- One `Net` per process: `Net::get_json::<T>(Req) -> Outcome<T>`, `Net::post_form`, `Net::get_bytes` (images), each
  taking a `Source` and a request description. Callers never see `reqwest` types.

### 7.2 Outcome

```rust
pub enum Outcome<T> {
    Answered(T),                              // a real answer; cacheable (30 d by default)
    Absent(Evidence),                         // the source answered "no such thing"; cacheable (7 d)
    RateLimited { until: OffsetDateTime },    // pause the source until then; costs no attempt
    Transient(Failure),                       // timeout, network, 5xx, 408, bad JSON; retried by the ledger
    Permanent(Failure),                       // 4xx (not 404-as-absent, 408, 429), missing key; never retried
}
pub struct Failure { pub code: String, pub message: String }   // HTTP_502, TIMEOUT, NETWORK, BAD_RESPONSE, …
pub struct Evidence(pub serde_json::Value);
```

Classification (`musicbrainz.ts:119-152`, generalised): 429, or 503 from MusicBrainz, or a body/message matching
`/rate.?limit|too many requests/i` → rate limited; 404 where the request says `not_found_means_absent` → `Absent`;
other 4xx except 408 → `Permanent("HTTP_<status>")`; 5xx, 408, timeout, connect/reset, body too large, unparsable
JSON → `Transient`. Callers that see an empty-but-valid answer decide themselves whether it is an absence (a search
with no hit is `Absent`; §7.4).

When identification is switched off (§15.3) every call returns `Transient(Failure{code:"DISABLED"})` without
touching the network. The coordinator never calls while off, so no stage records it; this is defence in depth.

### 7.3 Per-source limits

From upstream `B/services/rateLimiter.ts` and the service files. One `governor` rate limiter plus one `Semaphore`
(concurrency) per source; the semaphore is acquired first, then a limiter cell, then the request is sent. A stage's
30 s budget (§10.4) starts after the cell is acquired: waiting in line is not a timeout.

| Source | Hosts | Rate | Concurrency | Timeout | Rate-limit retries in client |
|---|---|---|---|---|---|
| `musicbrainz` | `MUSICBRAINZ_BASE_URL` (default `https://musicbrainz.org/ws/2`) | 1 per 1.1 s | 1 | 10 s | 3 |
| `acoustid` | `api.acoustid.org` | 3 per s | 1 | 10 s | 1 |
| `wikidata` | `query.wikidata.org`, `www.wikidata.org`, `en.wikipedia.org` | 5 per s | 3 | 10 s | 2 |
| `fanart` | `webservice.fanart.tv` | 2 per s | 1 | 10 s (test: 5 s) | 1 |
| `deezer` | `api.deezer.com` | 25 per 5 s | 5 | 10 s | 1 |
| `listenbrainz` | `labs.api.listenbrainz.org` | 1 per 1.1 s | 1 | 10 s | 1 |
| `lrclib` | `lrclib.net` | 1 per 3 s | 1 | 5 s | 1 |
| `coverartarchive` | `coverartarchive.org` (+ its archive.org redirect) | 2 per s | 2 | 15 s | 1 |
| `images` | any host an image URL names (SSRF-guarded, §12.6) | 4 per s | 2 | 30 s | 0 |

- `enrichmentConcurrency` (1–5, §15.1) multiplies each source's concurrency, never above its rate's burst (so
  MusicBrainz stays 1), as upstream `updateConcurrencyMultiplier` (`rateLimiter.ts:277-296`).
- Retries inside the client happen only for rate limits: wait `Retry-After` when it is a positive number of seconds
  (capped 60 s), else `2 s × 2^attempt + jitter(0..1 s)` capped 60 s (`rateLimiter.ts:250-266`). When retries are
  used up the call returns `RateLimited { until: now + pause }` and upserts `source_pauses`: pause 30 s
  (`DEFAULT_PAUSE_WINDOW_MS`); after 5 consecutive rate-limited outcomes the pause doubles per further one, capped
  at 60 s (the circuit breaker, `rateLimiter.ts:231-246`). An answered call resets `consecutive`.
- While a source has an unexpired `source_pauses` row, every call to it returns `RateLimited { until }` at once. The
  pause rows and the cache survive a restart, so a restart cannot burst a source that just limited us
  **(controller's call, C19)**.
- Transient errors are not retried in the client; the stage ledger owns retries (§10.4). The parent spec §10's
  "retries with backoff" is met by the two together.

### 7.4 Response cache

- Key: `<source>:` + hex SHA-256 of the canonical request: method, URL with query parameters sorted, and for POST
  the SHA-256 of the body. Secrets (`api_key`, `client`) are removed from the canonical form before hashing, so a key
  change does not orphan the cache and no secret lands in `request`.
- TTLs **(controller's call, C6: answers 30 days, absences 7 days, failures never)**, with upstream's shorter
  per-source TTLs kept where upstream has them:

| Source | Answered | Absent | Upstream |
|---|---|---|---|
| musicbrainz | 30 d | 7 d | `musicbrainz.ts:74-82` |
| acoustid | 30 d | 7 d | not cached upstream (kima-rs: keyed by fingerprint+duration) |
| wikidata | 30 d | 7 d | `wikidata:{mbid}` 30 d |
| fanart | 7 d | 7 d | hits 7 d |
| deezer | 24 h | 24 h | `deezer:artist:{name}` 24 h |
| listenbrainz | 7 d | 1 d (empty list) | `lb:similar:v2` 7 d / 1 d |
| coverartarchive | 30 d | 7 d | — |

- Which empty answers are absences: MusicBrainz search with zero hits, ISRC lookup 404 or zero recordings, a 404 on
  an entity lookup; AcoustID `results: []`; Wikidata SPARQL with no binding; Deezer search with no verified hit;
  ListenBrainz empty list; fanart 404; CAA 404. LRCLIB is not cached here: `track_lyrics` is its cache (§14).
- Reads go through the cache first; a hit costs no limiter cell. Writes are `INSERT ... ON CONFLICT (key) DO UPDATE`.
- Upstream caches some composite method results (`getAlbumCatalogue`'s two requests under one key). kima-rs caches
  per HTTP request, which gives the same hit behaviour.
- `http-cache-prune` (daily) deletes rows with `expires_at < now()`, and unused `genres`.
- `POST /api/system-settings/clear-caches` is not routed (upstream clears Redis; the frontend's "Clear caches" button
  keeps answering 404, as in M2). `wipe-and-rerun` keeps the cache, as upstream keeps its Redis `mb:*` keys
  (§10.7); a rerun after a wipe therefore costs no MusicBrainz requests for answers still fresh.

## 8. Track identification

**(controller's call, C3)** Upstream's ladder and gates exactly, with its tests ported. Source:
`B/workers/unifiedEnrichment.ts:1892-2184` (`enrichSingleTrack`), `B/services/recordingMatch.ts`,
`B/services/albumIdentity.ts:195-204`, `B/services/musicbrainz.ts`, `B/services/acoustidService.ts`.

### 8.1 Scanner input (C4)

**(controller's call, C4)** The scanner reads more tags with lofty and stores them. `RawTags` (`tags.rs:16`),
`ResolvedTrack` (`kima-db/src/library.rs:31`) and `upsert_track` gain the fields; `scan_index` is unchanged (decisions
still use size and mtime).

| Field | lofty `ItemKey` | ID3v2 | Vorbis | MP4 |
|---|---|---|---|---|
| `tag_recording_mbid` | `MusicBrainzRecordingId` | `UFID` owner `http://musicbrainz.org` | `MUSICBRAINZ_TRACKID` | `----:com.apple.iTunes:MusicBrainz Track Id` |
| `tag_release_track_mbid` | `MusicBrainzTrackId` | `TXXX:MusicBrainz Release Track Id` | `MUSICBRAINZ_RELEASETRACKID` | `…:MusicBrainz Release Track Id` |
| `tag_release_mbid` | `MusicBrainzReleaseId` | `TXXX:MusicBrainz Album Id` | `MUSICBRAINZ_ALBUMID` | `…:MusicBrainz Album Id` |
| `tag_release_group_mbid` | `MusicBrainzReleaseGroupId` | `TXXX:MusicBrainz Release Group Id` | `MUSICBRAINZ_RELEASEGROUPID` | `…:MusicBrainz Release Group Id` |
| `tag_artist_mbids` | `MusicBrainzArtistId` (all values) | `TXXX:MusicBrainz Artist Id` | `MUSICBRAINZ_ARTISTID` | `…:MusicBrainz Artist Id` |
| `tag_album_artist_mbids` | `MusicBrainzReleaseArtistId` (all) | `TXXX:MusicBrainz Album Artist Id` | `MUSICBRAINZ_ALBUMARTISTID` | `…:MusicBrainz Album Artist Id` |
| `isrc` | `Isrc` (first value, split on `;`/`,`) | `TSRC` | `ISRC` | `…:ISRC` |
| genres | `Genre` (all values; each value also split on `;`) | `TCON` | `GENRE` | `©gen` |
| disc/track totals | `DiscTotal`, `TrackTotal` | `TPOS`/`TRCK` "n/t" | `DISCTOTAL`/`TOTALDISCS`, `TRACKTOTAL`/`TOTALTRACKS` | `disk`/`trkn` |
| lyrics | `UnsyncLyrics`, `Lyrics`; ID3 `SYLT` via `Id3v2Tag` frames | `USLT`, `SYLT` | `LYRICS`, `UNSYNCEDLYRICS` | `©lyr` |

(These are lofty 0.25.4's mappings, `src/tag/item.rs`.) Rules:
- An MBID value that does not parse as a UUID is dropped (upstream `isMbid`, `artistIdentity.ts:77-82`). The
  `acoustid_id` tag is never read as a recording id (`acoustidIdentification.ts:96`).
- ISRC goes through `normalize_isrc` (strip spaces and hyphens, upper-case, must match
  `^[A-Z]{2}[A-Z0-9]{3}\d{7}$`; `recordingMatch.ts:12-15`), and is stored with `isrc_source='id3'` (upstream's name
  for "from the file", whatever the format; `musicScanner.ts:903-917`). A file ISRC always wins over a MusicBrainz one
  (`storeIsrc` priority `id3 > … > musicbrainz`).
- **Album MBIDs** (`albums.release_mbid`, `release_group_mbid`): the mode of the tracks' tag values among present
  tracks, recomputed in `process_folder` the way the album year is (scan.rs:158-171); ties go to the smallest UUID.
  Each value has a source, `tag | autotag | user`, with precedence user > autotag > tag **(controller's call, R2)**: a
  rescan writes a value (with source `tag`) only when the stored value is NULL or its source is `tag`. An autotag value
  (§19.7) or a release group typed in the MetadataEditor (§16.3, source `user`) wins over tags until the user clears
  it: the editor's Reset (`POST /enrichment/albums/{id}/reset`, §16.3) sets a `user` value and its source to NULL, and
  the next rescan fills it from tags again. The artist MBID below follows the same rule with `mbid_source`.
- **Artist MBID** (album artist only, kima-rs has no track-artist rows): from `tag_album_artist_mbids` when the album
  artist came from the AlbumArtist tag and exactly one id is present; from `tag_artist_mbids` when the album artist
  came from the Artist tag and the count is `1 + featured.len()` (upstream `primaryArtistMbid`,
  `musicScanner.ts:29-32,764-781`). Mode over the artist's tracks; set when `artists.mbid` is NULL, with
  `mbid_source='tag'`; never replaces a `user` or `autotag` value.
- **Genres from files** are stored as `track_genres` rows with `source='file'`, replaced wholesale per read. Upstream
  ignores file genres; kima-rs keeps them as a lower-precedence source (§13) **(controller's call, C4)**.
- **Embedded lyrics** (`musicScanner.ts:921-987`): ID3 `SYLT` → LRC lines `[mm:ss.cc] text` (centiseconds); `USLT` and
  Vorbis/MP4 lyrics → synced if the text matches `^\[\d{2}:\d{2}` on its first non-empty line, else plain. Upsert
  `track_lyrics` with `source='embedded'` when either exists. A re-read whose file no longer has lyrics deletes a row
  whose source is `embedded` (upstream leaves it; D-115). An `lrclib` or `none` row is replaced by `embedded` when the
  file gains lyrics.
- **Changed files** (the scanner's "changed" class, not a restore): the track's `track_stages` rows are reopened to
  `pending` with `attempts=0`, `evidence={"reason":"file changed"}`, and its `track_acoustid` row is deleted (the
  audio may have changed; upstream `reopenStagesForChangedFile`, `musicScanner.ts:36-60`). A forced re-read caused by
  an autotag write (§19.7) does not reopen anything (`ReadCause::TagWrite`).

### 8.2 Text rules (`kima_ident::text`)

Ported function by function, each with its upstream tests (§24.2):
- `fold_identity(s)`: NFKD, drop combining marks (`\p{Mn}`), NFKC, then a fixed table for Latin letters NFKD does not
  decompose (`ß→ss`, `æ→ae`, `Æ→AE`, `œ→oe`, `Œ→OE`, `ø→o`, `Ø→O`, `đ→d`, `Đ→D`, `ł→l`, `Ł→L`, `þ→th`, `Þ→TH`,
  `ð→d`, `ı→i`, `ĸ→k`, `ŋ→n`, `ŧ→t`, `ﬁ→fi`, `ﬂ→fl`). Upstream uses a 1613-entry table generated from Postgres'
  `unaccent` (`pgTextRules.ts`) so that keys match database-computed ones. kima-rs never compares a Rust key with a
  database key for identification, so the PG-parity test is dropped and the ported `titlesAgree`,
  `artistIdentity` and `albumIdentity` cases decide (D-106 notes it).
- `normalize_artist_name(s)` = trim, `fold_identity`, lowercase, `\s*&\s*` → ` and `, collapse whitespace
  (`artistIdentity.ts:45-56`); `artist_identity_key(s)` = that with every non-letter/number removed
  (`:63-65`, "Dead Mau5" → "deadmau5").
- `fold_title(s)`: NFKD, drop Latin combining marks only (U+0300–U+036F), NFKC, lowercase, keep only
  `\p{L}\p{M}\p{N}`; empty never agrees.
- `normalize_for_search(s)` (`musicbrainz.ts:555-568`): drop `(...)` and `[...]`; drop
  ` - [year] (deluxe|remastered|remaster|edition|version|expanded|bonus|explicit|clean|single|radio edit|remix|
  acoustic|live|mono|stereo) [year] [edition|version|mix]`; drop a trailing ` - YYYY`; collapse whitespace.
- `titles_agree(a, b)` (`albumIdentity.ts:195-204`): equal after NFKC-lower-trim (non-empty); or equal after
  `fold_title` (non-empty); or equal after `normalize_for_search` then `fold_title`.
- `version_of(title)`: the sorted set of `VERSION_WORDS` = remix, mix, dub, edit, live, acoustic, demo, instrumental,
  reprise, rework, unplugged, session, version, karaoke, cover, found among the NFKC-lowercased title split on
  non-letter/number (`recordingMatch.ts:42-51`).
- `lengths_agree(a_secs, b_secs) -> Option<bool>`: `None` when either is unknown or ≤ 0, else `|a − b| ≤ 5`
  (`LENGTH_TOLERANCE_SEC = 5`, `:40,54-57`).
- `artists_agree(local_names, credits)`: true when any local name's `artist_identity_key` (or the key of its sort
  name) is in the set of the candidate's credit keys; credit names are each credit's `name`, `artist.name`,
  `artist.sort-name`, plus the joined credit line (`creditNames`, `musicbrainz.ts:24-31`; `recordingMatch.ts:59-69`).
  Local names = the track artist split by `names::parse_credit` (primary plus featured) and the album artist.
- `recording_agrees(c, expected)` = `titles_agree(c.title, e.title)` ∧ `version_of` equal ∧ `artists_agree` ∧
  `lengths_agree != Some(false)` (`:72-79`).
- `pick_recording(candidates, expected)`: among agreeing candidates, sort by (known length first), then MusicBrainz
  search `score` descending, then smaller length gap; take the first (`:86-103`).
- `escape_lucene`: backslash-escape `+ - & | ! ( ) { } [ ] ^ " ~ * ? : \ /` (`musicbrainz.ts:547-549`).

### 8.3 The ladder

For one track, in order; the first accepted candidate wins. **Any source that fails to answer (rate limited,
transient or permanent) stops the ladder and defers the stage** (upstream `failedAsk`); only `Absent` falls through.

`expected` = `{ title: tracks.title, artists: §8.2 local names, length: tracks.duration_secs, isrc: tracks.isrc }`.

1. **File tag.** `tracks.tag_recording_mbid` is set → answer, `source=file_tag`, no network
   (`unifiedEnrichment.ts:1944-1947`). If the album also has a release group, rung 2 still runs, looking the
   catalogue entry up by this id, only to collect the recording's ISRCs and genres; otherwise those come from
   `getRecordingIsrcAndGenres` (§8.5).
2. **Album tracklist**, when `albums.release_group_mbid` is set (`:1949-1973`):
   - Fetch the catalogue: `GET /release-group/{rg}?inc=releases+genres`, choose the release with `chooseRelease`
     (official first, then earliest `date` padded with `-99-99` and `9999` for none, then id ascending;
     `musicbrainz.ts:40-47`), then `GET /release/{id}?inc=recordings+isrcs+labels+genres+artist-credits`.
     kima-rs addition: when `albums.release_mbid` is set (a tagged release), that release is fetched directly instead
     of `chooseRelease` (D-117).
   - With a file-tag recording id: the entry with that recording id; else `match_catalogue_recording`
     (`:1841-1861`): candidates whose ISRCs include the track's ISRC first; then `pick_recording` over the tracklist
     with the track's credited names; if none agrees, the "vouched" pass, which assumes the album artist for every
     row, accepted only when exactly one row agrees.
   - `source=tracklist`. A failed catalogue request defers (no fall-through).
3. **ISRC**, when `tracks.isrc` is set: `GET /isrc/{code}?inc=artist-credits` (404 or zero recordings → `Absent`; a
   400 → `Absent`), then `pick_recording` (`:1975-1991`). `source=isrc`.
4. **Name search** (`identifyRecording`, `musicbrainz.ts:865`): with normalised title and the first local artist,
   query 1 `recording:"<t>" AND artist:"<a>"`, then query 2 `<t> AND artist:<a>` (`recordingQueries`, `:540-545`),
   `GET /recording?query=…&limit=50`; a 400 answer means "try the next query"; an empty normalised title asks
   nothing (`Absent`, "nothing to search by"). `pick_recording` over all hits. `source=search`.
5. **AcoustID** (§8.4). Skipped, with `evidence.acoustid = "skipped"` and a reason, when the fingerprint stage closed
   as a permanent failure with no fingerprint stored, when fingerprinting is unavailable (§9.4), when no AcoustID
   key is configured (§2, reason "no AcoustID key"), or when the `acoustid` source is paused. A pause parks the stage `pending` until the pause ends, with no attempt used
   (`:2012-2045, 2073-2082`).

No rung answers → the stage closes `done` with `evidence = {reason:"no match", asked:[…]}`
(`recordStageAbsent`, `:2072-2092`); `recording_mbid` stays NULL.

### 8.4 AcoustID lookup (`acoustidService.ts`)

- `POST https://api.acoustid.org/v2/lookup`, `Content-Type: application/x-www-form-urlencoded`, body built by hand:
  `client=<key>&duration=<secs>&fingerprint=<fp>&meta=recordings%20releasegroups`. The space in `meta` must be `%20`
  (`:84-88,121-134`); a form encoder that writes `+` breaks it, so the body is a formatted string with `key` and `fp`
  percent-encoded and the rest literal.
- `duration` = `track_acoustid.duration_secs` (the file's full duration, as `fpcalc` reports it, not the 120 s
  window). No key → `Permanent("NO_CLIENT_KEY")` (never reached from the ladder, which skips the rung first, §8.3); empty fingerprint or duration ≤ 0 → `Permanent("NO_FINGERPRINT")`
  with no request; `status != "ok"` → `Transient("BAD_RESPONSE")`.
- **Pick** (`pickFingerprintMatch`, `:200-220`): iterate results in order; skip `score < 0.5` (`MIN_SCORE`) and
  results with no `recordings`; within a result keep recordings with `lengths_agree(duration, rec.duration) !=
  Some(false)`; if the track has a title, prefer those whose title agrees (`titles_agree` and equal `version_of`), else
  all; sort by `|rec.duration − duration|` ascending; take the first. No artist check (the audio is the evidence).
  Nothing → `Absent { askedDuration, topScore, reason }`.
- Stored: `recording_mbid` only, `source=acoustid`, evidence `{score, acoustidId}`. The returned artist and
  release-group ids are not written, as upstream (`acoustidIdentification.ts`): artist MBIDs come from the artist
  step (§12.1), album MBIDs from tags or the autotagger **(controller's call, C3, C12)**.
- Key resolution (§15.1): `settings_kv.acoustidApiKey`, then env `ACOUSTID_CLIENT_KEY`, then the built-in default
  **(controller's call, C8)**; see §27 Q1 on the default. `ACOUSTID_DISABLED=true` (upstream env) skips rung 5 as
  "acoustid disabled".

### 8.5 What a successful recording stage writes

In one transaction, after checking the claim is still current (`claimIsCurrent`, §10.3):
- `tracks.recording_mbid`, `recording_source`.
- ISRC: when `isrc_source` is NULL or `musicbrainz`, and the answer did not come from rung 3: the first ISRC of the
  matched recording with `isrc_source='musicbrainz'`, or NULL/NULL when the recording has none
  (`:2105-2110,2142-2146,2186-2193`). On the non-catalogue path the recording's ISRCs and genres come from one request
  `GET /recording/{id}?inc=isrcs+genres+tags` (`getRecordingIsrcAndGenres`, `:901`).
- Track genres: the recording's curated `genres` (not crowd `tags`), top 5 by `count` descending (`topVoted`),
  replace the track's `source='musicbrainz'` rows (`populateTrackGenres`, `trackIdentity.ts:120-138`). `tags` go into
  the stage evidence only.
- Album fill from the catalogue (`fillAlbumFromCatalogue`, `:1865-1886`), only where blank: `albums.label`; album
  MusicBrainz genres (top 5 release-group genres, `source='musicbrainz'`) if the album has none.
  **Album year is never filled from MusicBrainz in kima-rs**: the year is part of the album identity key
  `(artist_id, title_key, COALESCE(year,-1))` (D-039), and the scanner recomputes it from tags on every read, so a
  database-only year would split the album on the next rescan. The autotagger writes the year into the files instead
  (D-117).
- Stage evidence `{recordingMbid, reason, tags?, asked?, fingerprintFrom?}`.

It never writes album title, album artist, album MBIDs, or artist MBIDs, and never moves tracks between albums.

## 9. Fingerprinting

**(controller's call, C2)** In-process with `rusty-chromaprint`; ffmpeg decodes; no `fpcalc` in the image.

### 9.1 Pipeline

1. Resolve the track path under the library root with the M1 rule (only `Component::Normal` parts; survey §2).
2. Spawn `ffmpeg -nostdin -v error -i <abs path> -map 0:a:0 -t 120 -ac 1 -ar 11025 -f s16le -` with stdout piped
   and stderr captured (first 2000 bytes kept). 120 s and 11025 Hz mono match `fpcalc`'s defaults (`-length 120`,
   chromaprint's internal rate), so no resampling happens inside chromaprint.
3. Feed stdout in 64 KiB chunks to `rusty_chromaprint::Fingerprinter` configured with `Configuration::preset_test2()`
   (chromaprint algorithm 2, what AcoustID and `fpcalc` use by default), then `finish()`.
4. Compress and encode the raw `u32` fingerprint as AcoustID expects (chromaprint's `FingerprintCompressor` format,
   base64 URL-safe without padding). If the crate does not ship the compressor, port chromaprint's
   `fingerprint_compressor.cpp` (~120 lines) into `kima_ident::chromaprint::compress` (§26).
5. Store `track_acoustid (fingerprint, duration_secs = tracks.duration_secs)`. A track with `duration_secs ≤ 0` closes
   `failed`, class `PERMANENT`, code `NO_DURATION`.
6. Timeout 30 s (upstream `STAGE_TIMEOUT_MS.fingerprint`): kill ffmpeg, `Transient("TIMEOUT")`. Non-zero exit: the
   stderr text is the error message, classified by §10.4. Zero bytes decoded: `Transient("NO_AUDIO")`, message
   "incomplete decoder output" (upstream's "incomplete fpcalc output").

A file whose path escapes the root, or which is gone (`ENOENT`), closes with class `FILE_MISSING` (§10.4).

### 9.2 Reuse

A track with a `track_acoustid` row closes the stage `done` with `evidence = {reason:"fingerprint already stored",
characters}` without decoding (`unifiedEnrichment.ts:1222-1269`). Upstream also reads a fingerprint from an
`acoustid_fingerprint` file tag (`acoustidIdentification.ts:98`); lofty has no `ItemKey` for it and kima-rs does not
read it (D-106). Every track gets a fingerprint, matched or not, as upstream.

### 9.3 Concurrency

Fingerprinting runs in its own pool inside the cycle (§10.6): `KIMA_FINGERPRINT_WORKERS`, default
`max(1, available_parallelism / 2)` (upstream: all cores, `:1276-1327`). Half the cores leaves room for transcodes,
so playback is never starved by enrichment.

### 9.4 Capability

- `GET /api/system/features` answers `fpcalc: true` when ffmpeg was found at startup (one `ffmpeg -version` probe,
  5 s timeout) and `ACOUSTID_DISABLED` is not `true` **(controller's call, C2)**; the key keeps upstream's name.
- When fingerprinting is unavailable the fingerprint stage is not offered, and the recording stage treats the
  fingerprint gate as settled, skipping rung 5 with `evidence.acoustid = "skipped"`, reason "fingerprinting
  unavailable". Upstream leaves such fingerprint records `pending` (`:1343`), which, with the `settled` gate, would
  leave every recording waiting; kima-rs does not wait on a stage that cannot run (D-108).
  `retry-failed-and-blocked` reopens these skipped recordings once fingerprinting is available (§10.7).

### 9.5 Known-answer test

**(controller's call, C18)** `tools/fixtures/chromaprint/` holds `tone.wav` (30 s, 11025 Hz mono s16, a fixed
multi-tone sequence generated by `tools/make-fingerprint-fixture.sh` with ffmpeg `aevalsrc`) and `tone.fpcalc.json`,
the output of `fpcalc -json tone.wav` from chromaprint 1.5.x on the owner's machine, committed with the fpcalc version
in a comment. The test asserts that `kima_ident::chromaprint::fingerprint_file(tone.wav)` equals the committed string
exactly. Because the fixture is already at 11025 Hz mono, neither path resamples, so equality is expected; a second
test on a 44.1 kHz FLAC of the same signal compares the raw fingerprints by bit error rate < 5 % (AcoustID tolerates
far more). The FLAC test documents the resampler gap rather than gating on bit equality.

## 10. `track_stages`, the state machine, and the coordinator

**(controller's call, C5)** Stages `fingerprint` and `recording` now; `analysis`, `embedding` reserved for M4,
`similarity` for M5. Upstream's retry ladder and failure classes; a rate limit pauses only that source and costs no
attempt; `recording` waits for `fingerprint` to settle.

### 10.1 No row means pending

A present track with no row for a stage is pending for it (upstream `getNextWork` LEFT JOIN, `:168-275`). Rows are
created at the first claim. So a scan of 50 000 new tracks writes no stage rows, and the progress query treats "no
row" as pending.

### 10.2 States

| status | `next_attempt_at` | Meaning |
|---|---|---|
| `pending` | NULL | due now |
| `pending` | t | parked until t (source paused; `error_code = RATE_LIMITED`) |
| `working` | t | claimed; lease ends at t (claim + 15 min, `STAGE_CLAIM_LEASE_MS`) |
| `done` | NULL | answered, including "no match" |
| `failed` | t | failed, retry due at t |
| `failed` | NULL | terminal |

Upstream's `blocked` (a person parked the record) is not modelled: no route the frontend calls can set it
(`resolveFailures` has no HTTP route, survey §7.3). The failures list's `skipped` is always `false` (D-108).

Settled (the `recording` prerequisite): fingerprint `done`, or `failed` with `next_attempt_at` NULL, or fingerprinting
unavailable (§9.4) (`PREREQUISITE_STAGE.recording = {stage:"fingerprint", wait:"settled"}`, `:108-121`).

### 10.3 Claim and report

Claim (per stage, one statement; `$n` = batch size):

```sql
WITH due AS (
  SELECT t.id FROM tracks t
  LEFT JOIN track_stages s ON s.track_id = t.id AND s.stage = $stage
  WHERE t.missing_since IS NULL
    AND (s.track_id IS NULL
         OR (s.status = 'pending' AND (s.next_attempt_at IS NULL OR s.next_attempt_at <= now()))
         OR (s.status IN ('failed', 'working') AND s.next_attempt_at <= now()))
    AND <prerequisite settled, for 'recording'>
  ORDER BY t.mtime DESC, t.id
  LIMIT $n
  FOR UPDATE OF t SKIP LOCKED)
INSERT INTO track_stages (track_id, stage, status, next_attempt_at, started_at, updated_at)
SELECT id, $stage, 'working', now() + interval '15 minutes', now(), now() FROM due
ON CONFLICT (track_id, stage) DO UPDATE SET
  status = 'working',
  redelivered = track_stages.redelivered + (track_stages.status = 'working')::int,
  next_attempt_at = EXCLUDED.next_attempt_at, started_at = now(), updated_at = now()
RETURNING track_id, redelivered, attempts;
```

- Order `mtime DESC` = upstream's `fileModified DESC`: newest files first.
- A claim returned with `redelivered ≥ 3` (`STAGE_REDELIVERY_CAP`) is closed at once: `failed`, terminal,
  `error_code = LOST_WORKER`, class `INCONCLUSIVE`.
- A claim token `(track_id, stage, started_at)` travels with the work. Every report is
  `UPDATE … WHERE track_id=$1 AND stage=$2 AND status='working' AND started_at=$3`; zero rows means the claim lapsed
  and was re-offered, and the late report is dropped (upstream `claimIsCurrent`, `failureMaySettle`). The §8.5 writes
  sit in the same transaction, so a stale success writes nothing.
- Success → `done`, `next_attempt_at` NULL, errors cleared, `source`, `evidence`, `finished_at`. A recording stage
  `done` reopens nothing; a fingerprint `done` (or terminal) makes the recording prerequisite settled.

### 10.4 Failures, retries and classes

From `B/workers/stageRetry.ts`, `B/services/enrichmentErrorCodeClassifier.ts`, `recordStageFailure` (`:575-735`),
`recordOutcomeFailure` (`:801-877`):

- **Rate limited** (`Outcome::RateLimited{until}`, or for ffmpeg never): `status='pending'`,
  `next_attempt_at = until`, `error_code='RATE_LIMITED'`, `attempts` unchanged. The source is paused (§7.3) and the
  coordinator skips stages whose next rung needs that source until the pause ends.
- **Permanent** (`Outcome::Permanent`): `failed`, terminal, class `PERMANENT`, `error_code` = the outcome's code
  (`NO_CLIENT_KEY`, `HTTP_400`, …), `attempts + 1`.
- **Transient or a decode error**: `attempts + 1`; class = `classify(message)`:
  - `MULTICANNEL` if it matches `more\s+than\s+2\s+audio\s+channels?`, `more\s+than\s+2\s+channels?`,
    `multichannel`;
  - else `UNSUPPORTED` for `unsupported\s+codec`, `not\s+capable\s+of\s+decoding`, `unknown\s+demuxer`,
    `decoder\s+.*not\s+found`, `codec\s+.*not\s+found`;
  - else `DAMAGED` for `truncated`, `\bcrc\b`, `over[hr]ead`, `missing\s+keyframe`, `non[- ]?monotonou[sd]\s+dts`,
    `error\s+while\s+decoding`, `cannot\s+decode`, `could\s+not\s+decode`, `failed\s+to\s+decode`,
    `stretched\s+frame`, `invalid\s+start`, `invalid\s+end`, `invalid\s+\S+\s+frame`,
    `corrupt(?:ed|ion)?\s*(?:bitstream|audio|data|stream|file)`, `malformed\s+(?:audio|bitstream|data|stream|file)`,
    `decode[_\s]frame\b`, `\bheader\s+missing\b`, `invalid\s+dts`;
  - else `INCONCLUSIVE`. All case-insensitive; precedence as listed.
  - Retry only when `INCONCLUSIVE` and `attempts < 4` (`STAGE_RETRY_CAP`): `failed`,
    `next_attempt_at = now + [1 min, 10 min, 1 h][attempts − 1]`. Otherwise terminal.
  - ffmpeg downmixes with `-ac 1`, so `MULTICANNEL` never arises from kima-rs's own decoding; the class exists for M4
    and for the UI.
- **File missing** (path escapes the root, `ENOENT`): `failed`, terminal, class `FILE_MISSING`. A scan that restores
  or repoints the track reopens its `FILE_MISSING` rows to `pending`. `FILE_MISSING` rows are left out of the failures
  list and counted as `waitingMissing` (upstream).
- **Timeouts**: each rung's request runs inside the stage budget of 30 s (recording and fingerprint,
  `STAGE_TIMEOUT_MS`), counted from the moment a limiter cell is held (§7.3); a blown budget is
  `Transient("TIMEOUT")`.
- `DEPENDENCY_CLOSED` is reserved for M4's analysis-on-fingerprint dependencies; M3 never writes it.

### 10.5 Artist enrichment state

Artists keep upstream's per-artist status (`artistEnrichment.ts:283-297`), not stage rows:
- picked when `enrichment_status IN ('pending','failed')`, or `unresolvable` with `last_enriched_at < now() − 7 days`,
  and the artist has a present track; batch 10, ordered by `last_enriched_at NULLS FIRST, id`;
- claimed by `UPDATE artists SET enrichment_status='enriching' … WHERE id = ANY($1) AND enrichment_status <> 'enriching'
  RETURNING id`;
- outcome: any source failed (similar, Wikidata, MusicBrainz) → `failed`, `enrichment_failures + 1`, and at `≥ 4`
  `unresolvable`; only rate-limited → `pending`; else `completed`, `enrichment_failures = 0`. `last_enriched_at=now()`
  in every case;
- crash residue: at startup `enriching` → `pending` (upstream resets when the artist queue is empty, `:2234-2311`).

### 10.6 The coordinator (`enrich` queue)

Upstream runs a cycle loop with BullMQ jobs per item. kima-rs uses `track_stages` itself as the work queue and the
job table only for the cycle:

- Queue `enrich`, concurrency 1, one job kind `{"kind":"cycle"}`, `max_attempts` 3, heartbeat-renewed (§11).
- A cycle:
  1. Read `enrichment_state`. If `paused`, or `halted`, or `stopping`: set `stopping` → `idle` if stopping, re-enqueue
     nothing, end. (A paused or halted pipeline is woken by resume or sync.)
  2. Set `status='running'`, `started_at` if it was idle.
  3. Run **fingerprint** (batch 20 per claim, the §9.3 pool) concurrently with the network phases below, so CPU and
     network overlap.
  4. If identification is enabled (§15.3): **artists** (batch 10, items concurrently up to `enrichmentConcurrency`),
     then **tracks** = the recording stage (batch 20, up to `enrichmentConcurrency` in flight). A phase whose next
     source is paused is skipped this cycle. The source sequencing is upstream's (`:1514-1800`: artists, tracks,
     fingerprint); fingerprint runs alongside here.
  5. Between items, re-read the state (every 2 s at most): `paused` or `stopping` ends the phase after in-flight items
     finish.
  6. `current_phase` and `last_activity` are updated when counts change (`progressSignature` honesty: phase changes
     alone do not stamp activity).
  7. At the end: did any item → enqueue the next cycle with `run_at = now + 5 s`; nothing to do → `status='idle'`,
     next cycle `run_at = now + 60 s`. A cycle never runs more than once per 10 s (`MIN_INTERVAL_MS`).
- **Trigger**: a reconcile that added or changed tracks, and every control route below, call
  `coordinator::kick(pool)`: if a cycle job is queued, `UPDATE jobs SET run_at = now()`; else enqueue one. Pausing
  does not kick.
- **In-flight caps** (upstream `inFlightCap`, recording 30) are not needed: claims are batched by the coordinator
  itself.
- Every `enrichment:progress` event (§17) is published from here.
- `hung`: `status='running'` and `last_activity < now() − 15 min` (upstream `detectHang`).

### 10.7 Operator controls

Behind the §16.2 routes, with upstream's semantics (`B/services/enrichmentState.ts`, `:2948-2983`,
`:1388-1459`, `B/services/enrichmentRestart.ts`):

- **pause**: only from `running` (else 400 "Cannot pause enrichment in <status> state"); `status='paused'`,
  `paused_at`. Persisted across restarts.
- **resume**: from `paused` (else 400; idempotent when `running`); `status='running'`, kick.
- **stop**: idempotent when `idle`; `status='stopping'`, `halted=true`, `stopped_at`; the running cycle finishes its
  in-flight items and leaves `idle`. While `halted`, no cycle runs: scans and `enrich-kick` do not lift it (upstream:
  `triggerEnrichmentNow` never lifts a stop).
- **sync** ("Sync New", upstream `startEnrichmentNow`): clears `halted` and `paused`, kicks; answers
  `{message:"Incremental sync started", description, result:{artists, tracks, audioQueued:0}}` where `artists` =
  artists now pending, `tracks` = recording stage pending (incl. no row).
- **retry-failed-and-blocked**: first reopens `recording` rows closed `done` with `evidence->>'acoustid' = 'skipped'`
  whose reason no longer holds (the fingerprint is now stored or available, or an AcoustID key is now configured); then, per stage, reopens rows `failed` with a due retry to `pending`
  now (`next_attempt_at=NULL`); kick. Nothing `done` or terminal is touched. `queued` = number reopened.
- **re-decide-everything**: every `track_stages` row → `pending`, `attempts=0`, `redelivered=0`, errors and evidence
  cleared, **except** `recording` rows of tracks whose `recording_mbid` is set (AcoustID dedupe, upstream); then the
  skipped-AcoustID reopen; no deletes; kick. `reopened` = rows changed.
- **wipe-and-rerun** (one transaction): artists → `bio=NULL, similar=NULL, enrichment_status='pending',
  enrichment_failures=0, last_enriched_at=NULL` (keeps `mbid` and the image), delete `artist_genres` with source
  `musicbrainz`; recording rows that are `done` or `working` → `pending` with their tracks' `recording_mbid=NULL`,
  `recording_source=NULL`, `track_genres` source `musicbrainz` deleted, ISRC cleared when `isrc_source='musicbrainz'`;
  the fingerprint stage and `track_acoustid` are kept; answers `{artists, tracks, vectors:0}`; kick after commit.
  User overrides are never touched (upstream).
- **concurrency**: `PUT` stores `enrichmentConcurrency` (clamped 1..5), applies the §7.3 multiplier at once; answers
  `{concurrency, estimatedSpeed:"~{10c} artists/min, ~{60c} tracks/min", artistsPerMin:10c, tracksPerMin:60c}` (the
  same estimate text as upstream, although MusicBrainz's 1 request a second bounds the real rate).

### 10.8 Progress

`GET /api/enrichment/progress` (memoised 5 s), from `track_stages` LEFT JOIN present tracks per stage, and artists by
status. Shape exactly upstream's (`unifiedEnrichment.ts:2827-2941`):

```json
{"source":"TrackStage",
 "artists":{"total","completed","pending","owed","failed","progress"},
 "trackTags":{"total","enriched","pending","waiting","waitingMissing","progress"},
 "audioAnalysis":{"total":0,"completed":0,"pending":0,"queued":0,"processing":0,"failed":0,
                  "permanentlyFailed":0,"waitingMissing":0,"progress":0,"isBackground":true},
 "clapEmbeddings":{… zeros …,"retrying":0,"isBackground":true},
 "fingerprints":{"total","completed","pending","processing","failed","waitingMissing","progress","isBackground":true},
 "similarity":{"total":0,"completed":0,"pending":0,"progress":0,"isBackground":true},
 "coreComplete":bool, "isFullyComplete":bool}
```

- artists: `total` = artists with a present track (VA excluded), `completed` = completed + unresolvable, `pending` =
  pending + enriching, `owed` = pending, `failed`; `progress = round(completed / total × 100)`.
- trackTags = the recording stage over present tracks: `enriched` = done, `waiting` = open but behind an unsettled
  fingerprint, `pending` = open and actionable (no row, pending, working, retrying), `waitingMissing` = open rows of
  missing tracks.
- fingerprints: `failed` = retrying + terminal; `processing` = working.
- `coreComplete` = artists and trackTags have nothing pending or working; `isFullyComplete` = `coreComplete` and the
  fingerprint stage has nothing pending, working or retrying (the M4/M5 parts are zero).

`GET /api/enrichment/status` = the `enrichment_state` row plus `hung` and the counters
`artists/tracks/audio{total,completed,failed(,processing)}` from the same query; a row in `idle` answers
`{status:"idle", currentPhase:null, hung:false, …}` (upstream).

### 10.9 Failures list

`GET /api/enrichment/failures?entityType&includeSkipped&includeResolved&limit=100&offset=0`:
- Track-stage rows with `status='failed'` (retrying and terminal) and class ≠ `FILE_MISSING`, mapped as upstream
  (`enrichmentFailureService.ts:66-93`): `id = "<trackId>__<stage>"` (upstream's row id form), `entityType` =
  `track` (recording) or `scan` (fingerprint), `entityId` = track id, `entityName` = track title, `errorMessage` =
  `last_error`, `errorCode` = `error_code`, `retryCount = attempts`, `maxRetries = 4`, `firstFailedAt` = `started_at`,
  `lastFailedAt` = `updated_at`, `skipped:false`, `skippedAt:null`, `resolved:false`, `resolvedAt:null`,
  `metadata = {stage, source, evidence, errorClass}`.
- Artist rows: artists `failed` or `unresolvable` with `enrichment_failures > 0`: `entityType:"artist"`, `id` = the
  artist id, `entityName` = name, `retryCount = enrichment_failures`, `maxRetries = 4`, `errorMessage`/`errorCode` =
  `artists.last_error`/`last_error_code` (§6.3), `firstFailedAt`/`lastFailedAt` = `last_enriched_at`,
  `metadata:{status}`.
- `entityType` filters; `includeSkipped` and `includeResolved` are accepted and change nothing (no such rows exist).
- `{failures:[…], total}` ordered by `lastFailedAt` desc.
- `GET /failures/counts` → `{artist, track, audio:0, vibe:0, scan, podcast:0, total}`.

## 11. Jobs: heartbeat, delayed enqueue, non-retryable errors

**(controller's call, C7)** The lease heartbeat lands in M3a and every long job uses it; it removes the 6 h
`LIBRARY_LEASE` stand-in.

### 11.1 Heartbeat

- `WorkerConfig.lease` stays 300 s by default; `heartbeat` (new) defaults to `lease / 4`.
- `run_one` spawns, next to the handler future, a renewal loop:
  `UPDATE jobs SET locked_until = now() + $lease WHERE id = $1 AND state = 'running' AND attempts = $2`. One row →
  carry on. Zero rows → the claim is lost (reaped or reclaimed): cancel the job's token and log once.
- Handlers get a context: `Handler = Arc<dyn Fn(Job, JobCtx) -> BoxFuture<'static, Result<(), JobError>>>` with
  `JobCtx { cancel: CancellationToken }`. `cancel` fires on shutdown and on a lost claim. Long handlers check it
  between units of work (the organizer between rows — it already has `checkpoint()`; the scanner between folders; the
  coordinator between items; the autotag writer between files).
- `LIBRARY_LEASE` is deleted; the `library` queue uses the default lease. A job that stops renewing (process frozen,
  database unreachable for > 300 s) is reclaimable as before.
- `kima_jobs::handler(f)` keeps accepting `Fn(Job) -> Fut<Result<(), String>>` through an adapter, so existing
  registrations change only where they want the token.

### 11.2 Delayed enqueue

- `enqueue_at(pool, queue, payload, max_attempts, run_at)`; `enqueue` = `enqueue_at(…, now())`. `pg_notify` is sent
  only when `run_at <= now()`; workers poll every `poll_interval` (5 s), so a delayed job starts within 5 s of its
  `run_at`.
- `reschedule_queued(pool, queue, kind, run_at)`: moves a queued job of that `kind` to `run_at` (the coordinator's
  kick, §10.6).

### 11.3 Non-retryable errors

`enum JobError { Retry(String), Permanent(String) }`, `From<String> for JobError` = `Retry`. `Permanent` sets
`state='failed'` at once, whatever `attempts` is (guarded by the same `state='running' AND attempts=$n` check as
`fail`). Used by: autotag apply refusals (target became unavailable), a candidate that no longer exists, an organizer
run in the wrong state.

### 11.4 Parked M1 minors 3 and 4

**(controller's call, C17: they ride with the heartbeat)** Text of each minor: the M1 final review, `.claude/worktrees/m1-library/.superpowers/sdd/m1/final-review-report.md`, "Minor"
list.
- **Minor 3**: `listen_is_retried_after_failure` (`kima-jobs/tests/worker.rs:171-221`) passes for the wrong reason.
  sqlx 0.8's `PgListener::recv()` reconnects by itself (`listener.rs:222-228` loops on `try_recv`), so killing the
  LISTEN backend never reaches the worker's retry arm (`worker.rs:92-106`). The real gap: notifications sent during the
  reconnect are lost and the worker does not claim again, so jobs wait up to `poll_interval` (5 s). Fix:
  - the test is rewritten to make the reconnect itself fail (through an injectable connect function that fails after
    the first connection), so the retry arm is exercised; it asserts the worker re-establishes LISTEN after
    `listen_retry` (200 ms in the test);
  - the worker runs one immediate claim sweep whenever `recv` returns after a reconnect (sqlx's own or the retry
    arm's); a second test sets `poll_interval` to 1 h, terminates the LISTEN backend, enqueues while it reconnects,
    and asserts the job runs within 2 s.
- **Minor 4**: `file_written_slowly_indexed_once_complete` (`kima-library/tests/watch.rs:359-388`) is timing-sensitive:
  a 300 ms chunk gap against a 500 ms settle fails the `vec![one]` assertion when the scheduler pauses over 200 ms.
  Fix, as the review says: widen the ratio, with a 2 s settle against the 300 ms gap.

## 12. Enrichment sources and precedence

**(controller's call, C9)** Upstream parity: bio from Wikidata/Wikipedia; images from Wikidata, fanart.tv and Deezer,
downloaded into kima-rs's own storage; similar artists from ListenBrainz then Deezer; genres from MusicBrainz.
Upstream `B/workers/artistEnrichment.ts:74-387` is the reference for every step.

### 12.1 Artist MBID

Skipped when `artists.mbid` is set from a tag, a user or the autotagger, except for the guard below.
- **Guard** (`dropMbidOfAnotherArtist`, `:25-57`) for `mbid_source IN ('tag','search')`: `GET /artist/{mbid}?inc=aliases`;
  if MusicBrainz does not know the id (`Absent`), or neither the name, sort-name nor an alias agrees
  (`artists_agree`), drop `mbid`, `bio`, MusicBrainz genres and `similar`; keep a local image. Rate-limited or
  failed → stop, artist `pending`/`failed`.
- **Resolve** (`resolveArtistMbid`, `artistCorrection.ts:152-203`) when no MBID: `strip_artist_suffixes(name)`
  (feat/with brackets; remaster/deluxe/… brackets; a trailing ` - remaster|single|live|acoustic`);
  `GET /artist?query=<q>&limit=10`; keep candidates whose `name` or an alias equals the query under
  `normalize_artist_name`; drop generic album titles (`isGenericAlbumTitle`, list at `albumIdentity.ts:19-36`, ported
  verbatim) from the artist's own album titles; with no usable titles: `Absent{ambiguous}` when > 1 candidate, else
  the single candidate; with titles: for the first 3 candidates (`MAX_CATALOGUE_CHECKS`), page
  `GET /release-group?artist=<id>&type=album|ep|single&limit=100&offset=…` until `release-group-count`, and accept the
  first whose catalogue has a group where `titles_agree(group.title, album title)`; none → `Absent`.
- `Answered` → `mbid`, `mbid_source='search'`. `Absent` → `unresolvable`, done. Failure → `failed` / `pending`.

### 12.2 Bio and Wikidata image (`wikidata.ts:46-94`)

Only with an MBID:
1. `GET https://query.wikidata.org/sparql?query=SELECT ?item WHERE { ?item wdt:P434 "<mbid>" } LIMIT 1&format=json`.
2. `GET https://www.wikidata.org/wiki/Special:EntityData/{Q}.json`.
3. The English Wikipedia sitelink title → `GET https://en.wikipedia.org/api/rest_v1/page/summary/{title}` → `extract`
   (404 → no bio, not a failure).
4. Image = claim `P18`: the `rank:"preferred"` one, else the first `normal`, never `deprecated`; URL
   `https://upload.wikimedia.org/wikipedia/commons/{m[0]}/{m[0..2]}/{file}` with `m = md5(file with spaces → _)`, the
   file name percent-encoded.

No entity → `{}` (answered, cached); a failure keeps the stored bio and marks the step failed.

### 12.3 Artist image precedence

1. `user` (a URL typed in the MetadataEditor, downloaded once; §16.3).
2. `local`: the nearest folder above the album folders, up to the library root, holding `artist.(jpg|png|webp)` or
   `folder.(jpg|png|webp)` larger than 1000 bytes and not a symlink (upstream `imageStorage.ts:119-173`). Checked at
   every artist enrichment, so adding a file takes effect.
3. `wikidata` (§12.2).
4. `fanart`: only with an MBID and a key enabled (§15.1): `GET https://webservice.fanart.tv/v3/music/{mbid}?api_key=`,
   first of `artistbackground`, `artistthumb`, `hdmusiclogo` `[0].url` (a bare file name is prefixed
   `https://assets.fanart.tv/fanart/music/{mbid}/{key}/`); 404 → none.
5. `deezer`: `GET https://api.deezer.com/search/artist?q=<name>&limit=1`, accepted only if the hit's name equals the
   artist's under `normalize_string` (upstream `B/utils/normalization.ts`; punctuation-only names must match exactly);
   `picture_xl || picture_big || picture_medium`.

An existing image of a higher-precedence source is kept; a lower one is replaced only when a higher one is found.
"None" is recorded only when every asked source answered (a failed source never records "none",
`artwork.ts:84-96`).

### 12.4 Genres

`GET /artist/{mbid}?inc=genres`, top 5 `genres` by `count` desc → `artist_genres` source `musicbrainz` (only if
non-empty; else untouched; `:194-212`). MusicBrainz `tags` are not stored. File genres for the artist are derived in
§13.1.

### 12.5 Similar artists (`artistSimilarity.ts`, `listenBrainz.ts`)

- **ListenBrainz** (needs an MBID): `GET https://labs.api.listenbrainz.org/similar-artists/json?artist_mbids=<mbid>&
  algorithm=session_based_days_7500_session_300_contribution_3_threshold_10_limit_100_filter_True_skip_30`; keep
  entries with `artist_mbid`, `name`, `score > 0`, not the artist itself; sort by score desc; top 30;
  `match = 0.5 + 0.45 × score / strongest`.
- **Deezer** (by name, in parallel): the verified search hit (§12.3 rule), then
  `GET /artist/{id}/related?limit=30`; `match` falls linearly from 0.9 to 0.5 by position; the seed is excluded.
- **Merge**: LB has ≥ 30 → LB only; else dedupe by lowercased name, LB winning, cut to 30; both empty → `[]`.
- The library re-rank (`RERANK_MAX_DELTA`) needs M5's matching state; kima-rs ports it as delta 0, which upstream's
  code accepts (survey §3.1.6), and M5 replaces it.
- Stored as `artists.similar = [{name, mbid|null, match}]`, unless a source failed (then the old value is kept and the
  step counts as failed).

### 12.6 Downloads and storage

- `Net::get_bytes(Source::Images, url)`: http/https only; the host must resolve only to public unicast addresses (no
  loopback, private, link-local, CGNAT, multicast, or IPv6 ULA; re-checked on each redirect); `Content-Type: image/*`;
  ≥ 1000 bytes; ≤ 15 MiB; decodes with the `image` crate (the bytes are re-encoded as JPEG q90 at most 1500 px on the
  long side, so nothing but a valid image is ever served).
- Artist images: `<data_dir>/artist-images/<artist_id>.jpg`; album fallbacks:
  `<data_dir>/album-images/<album_id>.jpg`. Not under the evictable artwork cache (D-094), because they cannot be
  rebuilt without the network. Resized variants go into the existing cache (`<data_dir>/artwork/…`) and are
  evictable. `prune_empty` deletes the files with their rows.
- Served by the existing route: `GET /api/library/cover-art/{id}?size=` accepts an artist id as well as an album id
  (UUIDv7s do not collide, M2 §6.5) and serves the artist image with the same size snapping and headers (D-113). The
  artist JSON's `coverArt`, `image` and `heroUrl` are the artist id when an image exists, else null (the frontend's
  `getCoverArtUrl` turns an id into that route).

### 12.7 Album cover fallback

For albums of the artist that have no embedded or folder cover (`enrichAlbumCovers`, `:393-479`), batches of 3:
1. **deezer**: `/search/album?q=artist:"A" album:"T"&limit=5`, fallback `?q=A T`; the first hit whose artist and title
   both agree (`namesAgree`); `cover_xl || cover_big || cover_medium`.
2. **coverartarchive** (needs `release_group_mbid`): `GET https://coverartarchive.org/release-group/{rg}` → the
   `front` image, `thumbnails.1200` or `image`.
3. **fanart** album (needs `release_group_mbid` and a key): `GET /v3/music/albums/{rg}` → `albumcover[0]`, else
   `cdart[0]`.

Downloaded per §12.6; the M1 cover lookup gains this file as its last source (after embedded and folder art, before
"no cover"). A user cover override (§16.3) comes first.

### 12.8 Page views

`GET /api/library/artists/:id` and `/albums/:id` read stored data only; no lookup happens on a page view (upstream
searches similar artists and images live when none are stored, `routes/library/artists.ts:815-870`). Similar artists
are resolved against the library (MBID, then `normalize_artist_name`) at read time, upstream's
`resolveSimilarArtists` shape: `{id: libraryId or name, name, mbid, coverArt: libraryId when it has an image else null,
albumCount:0, ownedAlbumCount, weight: match, inLibrary}`, first 10. Artists not in the library have no image
(upstream fetches one live) (D-113).

## 13. Genres

### 13.1 Sources and precedence

| Entity | Effective genres (first non-empty wins) | Display |
|---|---|---|
| Track | MusicBrainz recording genres (§8.5) → file genres (§8.1) | `trackGenres` |
| Album | MusicBrainz release-group genres (§8.5, autotag) → file genres: top 5 by number of the album's present tracks carrying them, ties by name | `genres` |
| Artist | MusicBrainz artist genres (§12.4) → file genres: top 5 by number of the artist's present tracks, ties by name | `genres` |

- User genres (`artists.user_genres`, `albums.user_genres`) are merged in front for display, unique, as upstream
  `getMergedGenres` (`B/utils/metadataOverrides.ts:13-37`): `[...userGenres, ...genres]`.
- **(controller's call, C4)** File genres are a separate source with lower precedence than MusicBrainz.
- File-derived album and artist genres are recomputed in `process_folder` for the albums and artists it touched
  (`album_genres`/`artist_genres` with `source='file'`).
- Names are stored as given and compared by `genres.key` (lowercase, collapsed whitespace). Genres beginning with `_`
  are skipped everywhere (upstream internal markers, `firstArtistGenre`).

### 13.2 Consumers

- `GET /api/library/genres` (user): upstream's query (`routes/library/tracks.ts:610-657`) over effective artist
  genres (not user genres): for each genre, count DISTINCT present tracks of the artist's albums, `HAVING count >= 15`,
  order by count desc, `LIMIT 20`, drop genres equal to an artist's `normalize_artist_name`. Answer
  `{genres:[{genre: lowercase name, count}]}`. The heuristics are kept for the radio page.
- Artist and album detail: `genres` (merged), `userGenres`, `tags` (= MusicBrainz genres, upstream's alias).
- Track JSON: `trackGenres` where upstream sends it (radio) is not needed; track detail gains `genres` (effective).
- Subsonic: §18.
- Search `genre` filter (D-053) stays out; it belongs to the search rework, not M3.

## 14. Lyrics

**(controller's call, C10)** LRCLIB plus embedded tags, a `track_lyrics` table, the on-demand route with a 30-day
"none" cache, and the Subsonic endpoints.

### 14.1 LRCLIB client

`GET https://lrclib.net/api/get?track_name=<t>&artist_name=<a>&album_name=<al>&duration=<round secs>`; response
`{id, trackName, artistName, albumName, duration, instrumental, plainLyrics|null, syncedLyrics|null}`; 404 → absent;
anything else → failure (`B/services/lrclib.ts`). `track_name` = track title, `artist_name` = the album artist's name
(upstream), `album_name` = album title, `duration` = `duration_secs`.

### 14.2 `GET /api/library/tracks/:id/lyrics`

Upstream flow (`routes/library/tracks.ts:401-512`):
1. Unknown or non-UUID id → 404 `{"error":"Track not found"}`.
2. A row with `source != 'none'` → return it.
3. A row with `source = 'none'` and `fetched_at > now() − 30 days` → `{plainLyrics:null, syncedLyrics:null,
   source:"none"}`.
4. Else, if identification is enabled: ask LRCLIB, waiting at most 5 s for the limiter slot plus the 5 s request
   timeout. Hit → upsert `source='lrclib'`, `lrclib_id`, `fetched_at=now()`; 404 → upsert `source='none'`.
   A failure, a limiter wait over 5 s, or identification off → 200 with the existing row's values or nulls, writing
   nothing.
5. Answer `{plainLyrics, syncedLyrics, source}`.

Missing tracks answer like present ones (the row survives until the purge). No background job fetches lyrics, as
upstream.

### 14.3 Frontend

`useLyricsSync` and `LyricsView` work unchanged against this route (survey §5.3).

## 15. Settings and secrets

**(controller's call, C8)** An encrypted key-value settings store using `kima_core::cipher`; AcoustID and fanart keys;
secrets write-only; `test-fanart` implemented; Last.fm out.

### 15.1 Keys

| Key (`settings_kv.key`) | Type | Default | Secret | Read by |
|---|---|---|---|---|
| `acoustidApiKey` | string ≤ 200 | — | yes | AcoustID (§8.4): setting → env `ACOUSTID_CLIENT_KEY` → built-in default |
| `fanartApiKey` | string ≤ 200 | — | yes | fanart (§12.3): used only when `fanartEnabled`; else env `FANART_API_KEY` |
| `fanartEnabled` | bool | false | no | fanart |
| `autoEnrichMetadata` | bool | true | no | the identification switch (§15.3) |
| `enrichmentConcurrency` | int 1..5 | 1 | no | §7.3, §10.6 (upstream column default 1) |
| `autotagAutoApply` | `"off"` \| `"db"` \| `"files"` | `"off"` | no | §19.5 (M3b) |

- Secret rows: `secret_enc = cipher.encrypt(value, aad = "settings_kv:" || key)`; the format version byte of D-072 is
  reused, so no new construction (the M2 fallback "re-encrypt `api_keys.secret_enc`" is not needed).
- `SettingsCache` in `kima-db` keeps decoded values in memory for 60 s (upstream `getSystemSettings` cache), and a
  write through the API replaces the cache entry at once in this process.
- A secret that fails to decrypt (lost `SETTINGS_ENCRYPTION_KEY`) is treated as unset and logged once an hour,
  naming the key and not the value (as M2 §5.2 does for tokens).

### 15.2 `/api/system-settings`

- `GET` (admin) answers the M1 keys plus, in upstream's names and types: `fanartEnabled`, `fanartApiKey: ""`,
  `hasFanartApiKey`, `acoustidApiKey: ""`, `hasAcoustidApiKey`, `autoEnrichMetadata`, `autoEnrichMetadataLocked`
  (true when `KIMA_IDENTIFY=off`), `enrichmentConcurrency`, `autotagAutoApply`. Secrets are never returned, which
  differs from upstream (it returns them decrypted; `systemSettings.acoustid.test.ts`) (D-110).
- `POST` (admin) accepts any subset. For the two secrets: absent or `""` = unchanged; `null` = clear; a non-empty
  string = trim and store (≤ 200 characters, else 400 `{"error":"Invalid settings","details":"<field> is too long"}`).
  `""` means unchanged because the frontend posts its whole settings object, and after a GET the hidden secret is
  `""` there (survey §8). Other fields are validated by type (400 with upstream's `{"error":"Invalid settings",
  "details": …}`); unknown fields are still ignored (as M1). The response stays M1's (the settings body), and the frontend
  ignores it.
- `POST /api/system-settings/test-fanart` (admin) `{fanartApiKey?}`: the body key, or when absent/empty the stored
  key, or 400 `{"error":"API key is required"}`. Identification off → 409
  `{"error":"Outbound lookups are turned off"}`. `GET /v3/music/b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d?api_key=…`
  (The Beatles; upstream's test MBID), 5 s timeout, no cache. 200 → `{success:true, message:"Fanart.tv connection
  successful"}`; 401/403 → 400 `{"error":"Fanart.tv rejected the API key"}`; anything else → 502
  `{"error":"Fanart.tv did not answer"}`. Upstream answers 500 "Internal server error" for all failures (D-110).

### 15.3 The identification switch

**(controller's call, C19)** Identification and enrichment are on by default, as upstream runs them. One admin system
setting, mirrored by env `KIMA_IDENTIFY=off`, switches off all outbound lookups; the admin UI shows the state.
- The setting is `autoEnrichMetadata`, the key the "Library Enrichment" panel already shows as "Auto enrich
  metadata". Upstream saves it but never reads it (survey §0.8); kima-rs gives it this meaning (D-111).
- Effective = `autoEnrichMetadata && KIMA_IDENTIFY != off`. When off: `Net` refuses every call (§7.2); the coordinator
  runs no network phase; lyrics answer from storage; `test-fanart` answers 409; the autotag search refuses with 409
  `{"error":"Outbound lookups are turned off"}`.
- Fingerprinting is local and keeps running when off; it uses no network.
- `KIMA_IDENTIFY=off` wins over the setting: `GET` reports `autoEnrichMetadata:false, autoEnrichMetadataLocked:true`,
  and a `POST` setting it true is stored but has no effect until the env is removed.
- Turning it on kicks the coordinator.

## 16. API routes

All under `/api`. Auth: "user" = `AuthUser`, "admin" = `AdminUser` (403 `{"error":"Admin access required"}`). Errors
are `{"error": …}` (M1). Ids are UUIDs (D-050): a non-UUID id answers as unknown.

### 16.1 Library, settings, system (M3a)

| Method | Path | Auth | Request | Response (keys) |
|---|---|---|---|---|
| GET | `/library/tracks/{id}/lyrics` | user | — | `{plainLyrics, syncedLyrics, source}`; 404 `{error:"Track not found"}` |
| GET | `/library/genres` | user | — | `{genres:[{genre, count}]}` |
| GET | `/library/artists/{id}` | user | — | M1 shape, now filled: `mbid`, `bio` (= `userSummary ?? summary`), `summary`, `userSummary`, `coverArt`/`image`/`heroUrl` (artist id or null), `userHeroUrl` (null), `genres` (merged), `userGenres`, `tags`, `displayName`, `hasUserOverrides`, `similarArtists` (§12.8). `topTracks`, `listeners` stay as M1 (D-048) |
| GET | `/library/artists` | user | M1 | each artist's `coverArt`/`heroUrl` = its id when it has an image |
| GET | `/library/albums/{id}` | user | — | M1 shape plus `mbid` (= `release_mbid`), `rgMbid`, `genres`, `userGenres`, `label`, `displayTitle`, `displayYear`, `userCoverUrl` (null) |
| GET | `/library/tracks/{id}` and track lists | user | — | `recordingMbid`, `isrc`, `genres` added where upstream returns them |
| GET | `/library/cover-art/{id}` | user | `size` | album or artist id (§12.6) |
| GET | `/system/features` | user | — | `fpcalc` real (§9.4) |
| GET | `/system-settings` | admin | — | §15.2 |
| POST | `/system-settings` | admin | §15.2 | M1 settings body |
| POST | `/system-settings/test-fanart` | admin | `{fanartApiKey?}` | `{success, message}`; 400/409/502 `{error}` |

### 16.2 `/api/enrichment` (M3a; upstream shapes, `B/routes/enrichment.ts`)

| Method | Path | Auth | Request | Response |
|---|---|---|---|---|
| GET | `/enrichment/progress` | user | — | §10.8 |
| GET | `/enrichment/status` | user | — | `{status, startedAt?, pausedAt?, stoppedAt?, currentPhase, lastActivity, hung, artists{total,completed,failed}, tracks{…}, audio{total,completed,failed,processing}}` |
| POST | `/enrichment/pause` | admin | — | `{message:"Enrichment paused", state}`; 400 `{error}` |
| POST | `/enrichment/resume` | admin | — | `{message:"Enrichment resumed", state}`; 400 |
| POST | `/enrichment/stop` | admin | — | `{message:"Enrichment stopping...", state}` |
| POST | `/enrichment/sync` | admin | — | `{message:"Incremental sync started", description, result:{artists, tracks, audioQueued}}` |
| POST | `/enrichment/retry-failed-and-blocked` | admin | — | `{message, description, queued}` |
| POST | `/enrichment/re-decide-everything` | admin | — | `{message, description, reopened}` |
| POST | `/enrichment/wipe-and-rerun` | admin | — | `{artists, tracks, vectors}` |
| GET | `/enrichment/failures` | user | `entityType`, `includeSkipped`, `includeResolved`, `limit` (100), `offset` | `{failures:[EnrichmentFailure], total}` (§10.9) |
| GET | `/enrichment/failures/counts` | user | — | `{artist, track, audio, vibe, scan, podcast, total}` |
| GET | `/enrichment/concurrency` | user | — | `{concurrency, estimatedSpeed, artistsPerMin, tracksPerMin}` |
| PUT | `/enrichment/concurrency` | admin | `{concurrency}` | same; 400 `{error:"Missing or invalid 'concurrency' parameter"}` |
| PUT | `/enrichment/artists/{id}/metadata` | admin | `{name?, bio?, genres?, heroUrl?, mbid?}` | the artist detail plus `albums:[{id,title,year,coverUrl}]`; 404; 409 `{error:"MBID <x> is already in use by another artist"}` |
| PUT | `/enrichment/albums/{id}/metadata` | admin | `{title?, year?, genres?, coverUrl?, rgMbid?}` | album plus `artist{id,name}`, `tracks[{id,title,trackNo,duration}]`; 400 `{error:"year must be a number"}`; 409 |
| POST | `/enrichment/artists/{id}/reset` | admin | — | `{message:"Artist metadata reset to original values", artist}`; 404 `{error:"Artist not found"}` |
| POST | `/enrichment/albums/{id}/reset` | admin | — | `{message, album}`; 404 |

`state` in pause/resume/stop answers is the status object.

### 16.3 Override semantics (MetadataEditor)

Upstream `routes/enrichment.ts:466-833`:
- `name` → `display_name`; `bio` → `user_bio`; `genres` (string array) → `user_genres`; `title` → `display_title`;
  `year` → `display_year` (null or `""` clears; not a number → 400). Empty string or null clears each field.
  `hasUserOverrides` = any override set.
- `mbid` / `rgMbid`: a non-empty value must be an MBID (else 400 `{"error":"Invalid MusicBrainz ID"}`; upstream
  accepts anything). Set with source `user`, no MusicBrainz lookup (as upstream); 409 when another row already has it
  (§6.3). Empty = unchanged (upstream: "leave empty to auto-fetch" means unchanged). An artist MBID change marks the
  artist `pending` so the next cycle re-enriches; an album `rgMbid` change reopens the album's tracks' `recording`
  stage rows that are `done` without a file-tag answer.
- `heroUrl` / `coverUrl`: a non-empty URL is downloaded at once through §12.6 (15 s); success stores it with source
  `user`; failure → 400 `{"error":"Could not fetch that image"}`. Upstream stores the URL and proxies it on each view;
  kima-rs has no URL proxy (D-043), so it keeps the bytes (D-118).
- Overrides do not change identity or sort: `display_title` does not rename the album row, and `sort_name` stays
  generated from `name`/`title` (upstream recomputes `sortName` from the display value) (D-118).
- Reset clears every override of that row (user images deleted, `user_genres = '{}'`, as upstream writes `[]`); MBIDs whose source is `user` are cleared with their source (R2).
- Reset needs admin (upstream: any signed-in user, which looks unintended next to the admin-only PUT) (D-118).

### 16.4 Upstream routes not ported

Routes the frontend never calls are dropped **(controller's call, C9)**, and log D-112: `POST /enrichment/artist/:id`,
`POST /enrichment/album/:id` (gated by the never-set per-user `enrichmentSettings`), `POST /enrichment/retry`,
`PUT /enrichment/tracks/:id/metadata` and `POST /enrichment/tracks/:id/reset` (in `api.ts` but no component mounts a
track editor), `/library/image-backfill/*`, `/library/backfill-genres`, `/library/artist-counts/*`,
`/library/album-cover/:mbid`, `/library/cover-art-colors`, `POST /system-settings/clear-caches`. `getMe`'s
`enrichmentSettings` stays `null`.

### 16.5 Autotag routes (M3b, kima-rs own; all admin)

| Method | Path | Request | Response |
|---|---|---|---|
| GET | `/library/autotag/capabilities` | — | `{identifyEnabled, files:{available, reason}, copy:{available, reason, copyRunId}, db:{available:true}}` |
| GET | `/library/autotag/albums` | `state`, `recommendation`, `q`, `offset` (0), `limit` (50, ≤ 200) | `{albums:[{albumId, artist, album, year, trackCount, state, recommendation, distance, releaseMbid, searchedAt, appliedAt, issues}], total}`; albums with no `autotag_albums` row have `state:"new"` |
| POST | `/library/autotag/search` | `{albumIds?:[uuid], all?:bool, query?:{artist, album}, releaseMbid?}` (`query`/`releaseMbid` need exactly one album id) | 202 `{queued}`; 400; 409 `{error}` when identification is off |
| GET | `/library/autotag/albums/{albumId}` | `candidateId?` | `{album:{id, artist, title, year, tracks:[{id, path, disc, trackNo, title, artist, duration, recordingMbid}]}, state, recommendation, error, candidates:[{id, releaseMbid, releaseGroupMbid, title, artist, date, country, media, label, catalogNumber, barcode, trackCount, distance, recommendation, penalties, origin, fetchedAt, stale}], conflict: null or {albumId, artist, title, year} for the selected candidate, selected:{candidateId, changes:[{trackId|null, field, current, proposed}], mapping:[{trackId, local:{disc, trackNo, title, duration}, release:{medium, position, title, length, recordingMbid}, distance}], missing:[{medium, position, title, length}], extra:[trackId]}}`; 404 |
| POST | `/library/autotag/albums/{albumId}/apply` | `{candidateId, target:"files"\|"copy"\|"db", reorganize?:bool, mbidsOnly?:bool}` | 202 `{runId, jobId}`; 400 `{error}` target unavailable (with the capability reason); 409 busy; 409 `{error, conflictAlbumId}` identity collision (§19.7); `mbidsOnly` = "Apply MBIDs only" (target `db`, identity fields unchanged) |
| POST | `/library/autotag/albums/{albumId}/skip` | — | `{state:"skipped"}` |
| GET | `/library/autotag/runs` | `offset`, `limit` (20) | `{runs:[{id, albumId, album, artist, target, reorganize, state, createdAt, finishedAt, summary, error}]}` |
| GET | `/library/autotag/runs/{id}` | `offset`, `limit` (200) | run plus `rows:[{id, trackId, path, state, note, changes:[{field, old, new}]}]`, `total` |
| POST | `/library/autotag/runs/{id}/undo` | — | 202 `{jobId}`; 409 unless `done` or `failed` with written rows; 409 `{error, conflictAlbumId}` when the old identity is now taken |
| GET | `/library/organizer/runs/{id}/albums` | `offset`, `limit` (20 albums) | `{albums:[{albumId, artist, album, rows:[…M1 row JSON…]}], total}` (§20.5) |
| POST | `/library/organizer/preview` | M1 body plus `albumIds?` | M1 answer |

## 17. SSE events

Through the existing bus (`kima_core::events`), flat `{type, …}` objects, `user_id: None` (every user; the frontend
only invalidates queries).

| `type` | When | Payload |
|---|---|---|
| `enrichment:progress` | coordinator: count changes and state transitions, at most once a second | `{phase: "artists"\|"tracks"\|"fingerprint"\|null, status}` |
| `autotag:progress` | search and apply jobs, at most 4 a second | `{job:"search"\|"apply"\|"undo", albumId?, runId?, done, total, state?}` |
| `library:progress` | unchanged; the autotag-chained organizer run emits its M1 events | M1 |

**(controller's call, C9)** Upstream emits `enrichment:progress` only from the vibe routes with `{phase:"vibe"}`
(`B/routes/analysis.ts:383,529`); kima-rs emits it from enrichment (D-112). The frontend invalidates
`["enrichment-progress"]` on it (`useEventSource.ts:96`), so polling becomes a fallback.

## 18. Subsonic fill-ins

**(controller's call, C11)** Rows refer to the M2 design §4 numbering.

| # | Endpoint | M3 answer |
|---|---|---|
| 4 | `getOpenSubsonicExtensions` | adds `songLyrics` v1 (C10) |
| 14 | `getAlbumList2 type=byGenre` | albums having the genre among their effective album genres (case-insensitive exact on `genres.key`), ordered by `sort_name`; `genre` missing → 10 "byGenre requires genre" (upstream) |
| 19 | `getRandomSongs genre=` | random present tracks having the genre among their effective track genres |
| 20 | `getSongsByGenre` | `genre` required (10); `count` default 50 clamp 1..500, `offset`; present tracks of present albums whose effective track genres contain the genre (case-insensitive exact), ordered `lower(title), title`; `songsByGenre` with `song` |
| 24 | `getGenres` | one `genre` per genre with at least one present track: `songCount` = distinct present tracks with it among their effective track genres, `albumCount` = distinct albums with it among their effective album genres, text = name; sorted by name |
| 32 | `getCoverArt` `ar-<id>`/artist id | the artist image through §12.6 (`size` snapping as albums); 70 "Cover art not found" when the artist has none |
| 58–59 | `getAlbumInfo`, `getAlbumInfo2` | `albumInfo` with `musicBrainzId` = `release_mbid` (else omitted); `notes` omitted (no source, §27 Q4); 70 for an unknown album id, 10 when `id` is missing (upstream: empty stub, no id check) |
| 60–61 | `getArtistInfo`, `getArtistInfo2` | `biography` = `user_bio ?? bio` (omitted when none), `musicBrainzId` = `mbid`, `similarArtist` = `similar` resolved to library artists (MBID then `normalize_artist_name`), first `count` (default 20, max 50), each `{id, name, coverArt:"ar-<id>", albumCount}`; `includeNotPresent` ignored; image URL attributes omitted (clients use `coverArt`; upstream sends the raw `heroUrl`); 70, 10 as M2 |
| 62 | `getLyrics` | `artist`/`title` optional (both empty → empty `lyrics`); first present track whose title contains `title` (case-insensitive) and whose album artist's `normalize_artist_name` equals `artist`'s; `lyrics artist title` text = plain else synced; stored lyrics only, no LRCLIB fetch (upstream `playback.ts:400-437`) |
| 63 | `getLyricsBySongId` | `id` required (10); no track or no row (or `source='none'`) → empty `lyricsList`; synced lines parsed by `^\[(\d{1,2}):(\d{2})(?:\.(\d{1,3}))?\]\s*(.*)$` into `{start: ms, value}` (1-digit fraction ×100, 2-digit ×10, 3-digit as is); entry `{displayArtist, displayTitle, lang:"und", synced:true, line}`; plain split on `\n`, trimmed, non-empty → `synced:false`; `structuredLyrics` list (upstream `lyrics.ts:8-90`) |
| 22–23 | `getSimilarSongs(2)` | stays a stub until M5 |

Song and album mapping: `genre` = the first effective track (album) genre, else the album artist's first effective
genre, omitted when none (upstream: always the artist's first genre). Song `musicBrainzId` = `recording_mbid`, album
`musicBrainzId` = `release_mbid`, artist `musicBrainzId` = `mbid` (OpenSubsonic fields; omitted when null). This
updates D-082 (genre present) and D-086 (genres, lyrics, artist info and artist cover art are real; similar songs,
podcasts, shares unchanged) (D-116). The `artistInfo.route.test.ts` port replaces the M2 stub snapshot (§24.2).

## 19. The autotagger (M3b)

**(controller's call, C13)** beets-style: candidates from MusicBrainz release search and from the tracks' recording
MBIDs; a weighted distance; strong < 0.04, medium < 0.2; an admin review page; nothing applied automatically by
default; apply writes tags with lofty then runs the organizer for those albums; every write journalled for undo.

### 19.1 Input

An album = a kima-rs album row with its present tracks. Local values per track: `title`, `artist_name`, `track_no`,
`disc`, `duration_secs`, `recording_mbid`, `tag_release_mbid`; per album: title, album artist name, year,
`is_compilation`, `release_mbid`, disc count (max `disc`, or `disc_total`).

### 19.2 Candidates (queue `autotag`, one job per album)

Job `{"kind":"search","albumId":…}` (deduped per album id), plus optional `query`/`releaseMbid` for a manual search.
Collected in order, deduplicated by release MBID, at most 10:
1. **Tag**: `albums.release_mbid` (from tags or an earlier apply), origin `tag`.
2. **Manual**: `releaseMbid` from the request (an MBID, or a `musicbrainz.org/release/<id>` URL), origin `manual`.
3. **Search** (beets' query shape): `GET /release?query=release:"<T>" AND artist:"<A>" AND tracks:<n>&limit=5`,
   `T`/`A` Lucene-escaped (§8.2), `n` = local track count; for a compilation the `artist:` clause is replaced by
   `arid:89ad4ac3-39f7-470e-963a-56509c546377`; when that yields fewer than 2 releases, the same query without
   `AND tracks:<n>`. A manual `query` replaces `T` and `A`. Origin `search`.
4. **Recording**: for up to 5 tracks with `recording_mbid` (preferring sources `file_tag`, `tracklist`, `isrc`),
   `GET /recording/{id}?inc=releases`; count each release id across them; keep releases listed for at least
   `ceil(k/2)` of the `k` probed recordings, most-counted first, at most 5. Origin `recording`.
5. Each candidate is fetched in full: `GET /release/{id}?inc=recordings+artist-credits+release-groups+labels+isrcs+
   media+genres` (cached 30 days), stored as `autotag_candidates.release`.
6. **Snapshot (controller's call, R3).** Everything apply needs from MusicBrainz is fetched now and stored on the
   candidate: `GET /release-group/{rg}?inc=genres` gives `autotag_candidates.release_group` (title, primary type,
   first release date, genres with counts); with the release it covers every §19.6 field. Apply makes no network
   request, so `kima-library` (which runs apply) does not depend on `kima-net`, and apply works with identification
   switched off. `fetched_at` records the snapshot's age; older than 30 days, the review page offers "Refresh candidates"
   (a new search job for the album), but apply still works from the old snapshot.

About 1–2 searches, up to 5 recording lookups, up to 10 release fetches and up to 10 release-group fetches: at most ~27
MusicBrainz requests, about 30 s per album at 1 request per 1.1 s. A rate-limit outcome re-enqueues the job at `until` with `enqueue_at` and
returns `Ok`; a transient failure fails the job (3 attempts, M1 backoff); the album's state becomes `failed` with the
error only after the last attempt.

### 19.3 Distance

Modelled on beets' `autotag/match.py` and `hooks.py` (`Distance`), with beets' default weights
(`config_default.yaml: match.distance_weights`):

| Key | Weight | Penalty (each in 0..1) | Added when |
|---|---|---|---|
| `album` | 3.0 | `string_dist(local album title, release title)` | always |
| `artist` | 3.0 | `string_dist(local album artist, release artist credit)`; 0 when both are Various Artists | local not "Unknown Artist" |
| `album_id` | 5.0 | 0 if equal, 1 if not | local `release_mbid` set |
| `year` | 1.0 | 0 if local year ∈ {release year, original year}; else if original year known: `min(1, |local − release year| / max(1, |this year − original year|))`; else 1 | local year and release year known |
| `mediums` | 1.0 | 0 if local disc count = release medium count, else 1 | local disc count > 1 or `disc_total` read |
| `tracks` | 2.0 | each mapped pair's track distance (once per pair) | per mapped pair |
| `missing_tracks` | 0.9 | 1 per release track with no local track | per missing track |
| `unmatched_tracks` | 0.6 | 1 per local track with no release track | per extra track |

Track distance (per local/release pair):

| Key | Weight | Penalty | Added when |
|---|---|---|---|
| `track_title` | 3.0 | `string_dist(local title, release track title)` | always |
| `track_artist` | 2.0 | `string_dist(local track artist, release track artist credit)` | the release is VA, or the track's credit differs from the release credit |
| `track_index` | 1.0 | 0 if local `track_no` equals the release track's position on its medium, or its absolute index on the release; else 1 | local `track_no > 0` |
| `track_length` | 2.0 | `min(1, max(0, |local − release| − 10) / 30)` (beets `track_length_grace` 10 s, `track_length_max` 30 s) | both lengths known |
| `track_id` | 5.0 | 0 if equal, 1 if not | local `recording_mbid` set |

Normalisation (beets `Distance.distance`): `d = Σ(weight × penalty) / Σ(weight)` over the entries added, so a key not
added does not count. Track distances are normalised first, then added once each under `tracks`. The stored
`penalties` are each key's share of the album distance (`weight × penalty / Σ weight`), which sum to `d`.

`string_dist(a, b)` (beets `string_dist`, adapted to keep non-Latin scripts):
1. Lowercase and `fold_identity` both. If a string ends with `, the`, move it to the front (`beatles, the` →
   `the beatles`). Replace `&` with `and`.
2. `basic(a, b)` = remove everything not `\p{L}\p{N}`; both empty → 0; else
   `levenshtein(a, b) / max(len a, len b)` in characters. (beets removes non-`[a-z0-9]` after `unidecode`; kima-rs
   keeps letters of every script, so two Japanese titles are compared, not both emptied.)
3. `base = basic(a, b)`. For each `(pattern, weight)` in order: `(^the , 0.1)`, `([\[\(]?(ep|single)[\]\)]?, 0.0)`,
   `([\[\(]?(featuring|feat|ft)[\. :].+, 0.1)`, `(\(.*?\), 0.3)`, `(\[.*?\], 0.3)`, `((, )?(pt\.|part) .+, 0.2)`:
   remove the pattern from both; if neither changed, continue; `case = basic(a′, b′)`;
   `delta = max(0, base − case)`; if `delta = 0`, continue; else `a, b = a′, b′` and `base = case + delta × weight`.
4. Return `base`.

Track mapping: the assignment minimising the summed track distance, by the Hungarian algorithm
(`pathfinding::kuhn_munkres_min`) on a square matrix padded with cost 1.0 for "unmatched" rows and columns, costs
scaled to integers ×10⁶. A pair whose distance is ≥ 0.5 after assignment is broken into one missing and one extra
track (a bad pair is worse than no pair).

Media and country are **tie-breakers** **(controller's call, C13)**: kima-rs has no local media or country evidence
(the scanner does not read `MEDIA` or `RELEASECOUNTRY`), so a weighted penalty would be a constant. Candidates are
ordered by distance rounded to 3 decimals, then status `Official` first, then media rank (`Digital Media`, `CD`, any
`… CD`, then others), then country rank (`XW`, `XE`, then others), then earliest date, then MBID.

### 19.4 Recommendation

**(controller's call, C13)**: `strong` when `d < 0.04`, `medium` when `d < 0.2`, else `none`. beets uses 0.04 and 0.25.
As in beets (`max_rec.missing_tracks`/`unmatched_tracks: medium`), any missing or extra track caps the recommendation
at `medium`, so auto-apply never fires on an incomplete mapping. The album's recommendation is its best candidate's.

### 19.5 Review flow

- Albums enter the list from: the needs-attention report (a "Match" button per album), the "Match albums" page's
  "Find matches" for a selection, or "Match all unmatched" (every album without `release_mbid`).
- The review page (§21.2) shows, for the selected candidate: the album-level and per-track changes table
  (`field`, current value, proposed value; unchanged fields hidden by default), the track mapping diff, missing and
  extra tracks, and the candidate list with distance, recommendation, origin and the penalty breakdown.
- Actions: **Accept** (apply the selected candidate with a target), **pick** another candidate (the page re-renders its
  changes), **Search** again (artist/album text or a release MBID/URL), **Skip** (state `skipped`; listed under
  "Skipped").
- Nothing is applied automatically by default. Setting `autotagAutoApply` (`"off"` default, `"db"`, `"files"`) makes a
  search job that ends with a `strong` recommendation enqueue an apply with that target (files only when available,
  else nothing) **(controller's call, C13)**.

### 19.6 What apply changes

From the chosen candidate's snapshot (§19.2 step 6, R3): its release (`R`), its release group (`RG`), and the
mapping. No request is made at apply time:

| Field | Value | Scope |
|---|---|---|
| album | `R.title` | album |
| album artist | `R.artist-credit` joined: `name + joinphrase` per credit | album |
| album artist sort | each credit's `artist.sort-name` + `joinphrase` | album |
| album artist MBIDs | each credit's `artist.id` | album |
| release MBID / release group MBID | `R.id` / `RG.id` | album |
| date | `R.date` (YYYY, YYYY-MM or YYYY-MM-DD as given) | album |
| original date | `RG.first-release-date` | album |
| year (database) | year of original date, else of `R.date` | album |
| compilation | true iff the release credit is the VA MBID | album |
| genre | top 3 of `RG.genres` by count, else of `R.genres`; never removes an existing genre when both are empty | album (written to every track) |
| disc number / disc total | medium `position` / number of media | track |
| track number / track total | track `position` / medium `track-count` | track |
| title | release track `title` (not the recording title, as Picard) | track |
| artist | track `artist-credit` joined, else the release credit | track |
| artist sort | track credits' sort names joined | track |
| artist MBIDs | track credits' artist ids | track |
| recording MBID / release track MBID | `track.recording.id` / `track.id` | track |

Extra (unmatched) local tracks get the album-scope fields except the MBIDs, so they stay in the same album, and keep
their own title and numbers. Missing release tracks change nothing.

The database rows are updated in place (§19.7), plus: `tracks.recording_mbid` with `recording_source='autotag'`
(closing the track's `recording` stage `done`, source `autotag`), `albums.release_mbid`/`release_group_mbid` with
source `autotag` (R2: a `user` value is kept and the review page says so), `artists.mbid` with `mbid_source='autotag'`
when the album artist has a single credit and the artist's MBID source is not `user`, album genres
`source='musicbrainz'`.

Fields not written (open for later): ISRC, label, catalog number, barcode, release country, media, release type and
status.

### 19.7 Album and artist identity on apply and undo

**(controller's call, R1)** Apply keeps the album row and updates it in place; nothing is merged silently.

- **The album row is stable.** The new title (`title`, `title_key`), year, `is_compilation`, MBIDs (source `autotag`,
  R2) and album artist are written onto the same `albums` row, so its id does not change. Candidates, runs,
  overrides (`display_title`, `display_year`, `user_genres`), album images, stars, ratings, playlist items (they reference
  tracks) and the review URL all survive. The row gets `identity_source='autotag'` and
  `applied_identity = {artistKey, titleKey, year}`, the identity the scanner would derive from the applied values
  (`names::key` of the album artist and title; the year).
- **Album artist change.** Let `k = names::key(new album artist)`:
  1. an artist row with `name_key = k` exists → re-parent the album to it (`artist_action='reparented'`);
  2. else, if the old artist row would be left with no albums (this album is its only album, missing tracks included)
     → rename the old row in place (`name`, hence `name_key` and the generated `sort_name`; the M1 rename trigger
     re-fires search vectors). It keeps its id, bio, image, stars, ratings and enrichment state
     (`artist_action='renamed'`, `old_artist_name` kept);
  3. else create an artist row for the new name (`'created'`, enrichment `pending`) and re-parent the album.
  An artist row that a re-parent leaves with no albums is removed by `prune_empty`, as in M1.
- **Collision.** If another album row already has the new identity (`albums_identity_key`: `(artist_id, title_key,
  COALESCE(year,-1))` with the target artist), apply is refused. It is checked when the apply is requested (409, §16.5),
  again in the job before any file is touched, and once more under row locks in the database transaction. The error
  names the conflicting album: `Another album already has this identity: <artist> – <title> (<year>)`, with
  `conflictAlbumId`. The review page shows that album with a link and offers "Apply MBIDs only" (the database target
  restricted to MusicBrainz ids and track numbers, identity fields unchanged); "merge" is listed as a later feature,
  shown disabled.
- **Tracks** get their §19.6 fields in the same transaction; then `scan::reread_paths(ctx, paths, ReadCause::TagWrite)`
  refreshes size, mtime and the content hash from the written files (no stage reopen, §8.1), which the rule below keeps
  on the same album row.
- **Scanner rule.** `resolve`/`store` (`resolve.rs:329-427`, `scan.rs` `process_folder`) gain one branch. For a file
  whose existing track row belongs to an album with `identity_source='autotag'`, the scanner derives the identity from
  the file's tags merged with its `track_tag_overrides` as usual; if the derived artist key and title key equal
  `applied_identity` and the derived year equals the applied year or is missing, the track stays on that album row by
  id, without a key lookup, and the album's year is not recomputed from the folder mode (the applied year stands). A new
  file whose tags derive the same identity reaches the row through the normal key lookup, since the row's key equals
  the applied identity. A track whose tags disagree (re-tagged outside kima-rs) is resolved normally and leaves the
  row. When no present track of the album agrees any more, the album returns to `identity_source='scan'`,
  `applied_identity=NULL`, and normal resolution applies from then on.
- **Undo, in reverse.** `autotag_runs.album_before` restores the row's title, year, compilation flag, MBIDs and their
  sources, and `identity_source`/`applied_identity`. The artist step is reversed: `reparented` or `created` →
  re-parent back to `old_artist_id` (if that row is gone, the same three rules apply to the old name); `renamed` →
  rename the row back to `old_artist_name` when that name's key is free, else re-parent to the row holding it. The old
  identity is checked for a collision before any file is touched; when it is taken (another album got it meanwhile),
  undo is refused with the same message and nothing changes.
- **Database-only target**: `track_tag_overrides` hold the §19.6 values as `RawTags` field overrides; `resolve` merges
  them over the file's tags on every read, so the scanner rule sees the applied identity and the match survives
  rescans. Undo deletes them in the same transaction as the row restore.
- A `previewed` organizer run is set to `superseded` (its plan is stale).

### 19.8 Tag writing (`kima_library::tagwrite`)

Writable formats: MP3 (ID3v2.4, or ID3v2.3 when the file has v2.3), FLAC (Vorbis comments), Ogg Vorbis and Opus (Vorbis comments), M4A/MP4 (ilst atoms).
Not writable in M3b: WAV, AIFF, WMA, APE, WavPack, raw AAC; an album containing any is refused for targets `files` and
`copy` with the list ("Tag writing is not supported for .wma files"); `db` remains available.

Mapping (Picard-compatible; lofty 0.25.4 `ItemKey` names in brackets):

| Field | ID3v2.4 | Vorbis comment | MP4 atom |
|---|---|---|---|
| title [TrackTitle] | `TIT2` | `TITLE` | `©nam` |
| artist [TrackArtist] | `TPE1` | `ARTIST` | `©ART` |
| artist sort [TrackArtistSortOrder] | `TSOP` | `ARTISTSORT` | `soar` |
| album [AlbumTitle] | `TALB` | `ALBUM` | `©alb` |
| album artist [AlbumArtist] | `TPE2` | `ALBUMARTIST` | `aART` |
| album artist sort [AlbumArtistSortOrder] | `TSO2` | `ALBUMARTISTSORT` | `soaa` |
| track number / total [TrackNumber, TrackTotal] | `TRCK` = `n/t` | `TRACKNUMBER` = n, `TRACKTOTAL` = t (and `TOTALTRACKS` = t if present) | `trkn` (n, t) |
| disc number / total [DiscNumber, DiscTotal] | `TPOS` = `n/t` | `DISCNUMBER`, `DISCTOTAL` (and `TOTALDISCS` if present) | `disk` (n, t) |
| date [RecordingDate] | `TDRC` | `DATE` | `©day` |
| original date [OriginalReleaseDate] | `TDOR` | `ORIGINALDATE` | `----:com.apple.iTunes:ORIGINALDATE` |
| genre [Genre] | `TCON` (v2.4: multiple values NUL-separated; v2.3: joined with `/`, Picard's v2.3 separator) | `GENRE` (one field per value) | `©gen` (values joined `; `) |
| compilation [FlagCompilation] | `TCMP` = `1` (removed when false) | `COMPILATION` = `1` | `cpil` |
| recording MBID [MusicBrainzRecordingId] | `UFID` owner `http://musicbrainz.org` | `MUSICBRAINZ_TRACKID` | `----:com.apple.iTunes:MusicBrainz Track Id` |
| release track MBID [MusicBrainzTrackId] | `TXXX:MusicBrainz Release Track Id` | `MUSICBRAINZ_RELEASETRACKID` | `----:com.apple.iTunes:MusicBrainz Release Track Id` |
| release MBID [MusicBrainzReleaseId] | `TXXX:MusicBrainz Album Id` | `MUSICBRAINZ_ALBUMID` | `----:com.apple.iTunes:MusicBrainz Album Id` |
| release group MBID [MusicBrainzReleaseGroupId] | `TXXX:MusicBrainz Release Group Id` | `MUSICBRAINZ_RELEASEGROUPID` | `----:com.apple.iTunes:MusicBrainz Release Group Id` |
| artist MBIDs [MusicBrainzArtistId] | `TXXX:MusicBrainz Artist Id` (multiple values) | `MUSICBRAINZ_ARTISTID` (one per value) | `----:com.apple.iTunes:MusicBrainz Artist Id` (one data atom per value) |
| album artist MBIDs [MusicBrainzReleaseArtistId] | `TXXX:MusicBrainz Album Artist Id` | `MUSICBRAINZ_ALBUMARTISTID` | `----:com.apple.iTunes:MusicBrainz Album Artist Id` |

Rules:
- Only the keys above are touched. Every other frame, comment or atom (cover art, ReplayGain, comments, lyrics,
  custom tags) is preserved. Existing values of a touched key are replaced as a whole (all values).
- The concrete tag types are used (`Id3v2Tag`, `VorbisComments`, `Ilst`), not the generic `Tag`, so UFID, TXXX
  descriptions and freeform atoms are written exactly. An MP3 without an ID3v2 tag gets a v2.4 one.
- **ID3v2.3 stays v2.3** **(controller's call, R4)**: the original version is read (`Id3v2Tag::original_version()`) and
  recorded in `autotag_journal.id3_version`; a v2.3 file is saved with `WriteOptions::default().use_id3v23(true)`, so
  lofty writes v2.3 frames (`TDRC` becomes `TYER`/`TDAT`, `TDOR` becomes `TORY`; multi-valued TXXX MBIDs are joined
  with `/`). Undo saves with the recorded version, so the file returns to its original version. An ID3v1 tag is left
  as it is.
- **Safe write**: copy the file to `<same folder>/.kima-tag-<journal id>.<ext>` (hidden, so neither the walker nor the
  watcher indexes it), write tags into the copy, `fsync`, re-read the copy and check every touched key reads back the
  new value, then `rename` the copy over the original (same folder, atomic), `fsync` the folder. The original path is
  held in the `IgnoreSet` for the duration plus the M1 grace (10 s).
- **Audio check**: `audio_md5(path)` = MD5 of the audio packets via `ffmpeg -v error -i <path> -map 0:a:0 -c copy -f md5 -`.
  Computed on the original before and on the written copy before the rename; different → the row fails with "the tag
  writer changed the audio; nothing written", the copy is deleted, the original untouched (§3 anti-goal).
- Paths: the M1 rule (only `Component::Normal` parts); a file that is a symlink, or has a symlinked ancestor under the
  root, is refused for `files`/`copy` with M1's `SYMLINK_NOTE`.

### 19.9 Writable locations, apply and undo

**(controller's call, C14)** Tags are written only where kima-rs may modify files.

| Target | Available when | Writes | Library rows |
|---|---|---|---|
| `files` | the library root passes `apply::writable` (the organizer's probe); else refused: "The library is mounted read-only. Mount it writable, or use copy or database-only." | the library files, in place | re-read (§19.7) |
| `copy` | `KIMA_ORGANIZE_TARGET` writable, and a copy-mode organizer run in state `done`, not switched, whose `done` rows cover every track of the album; else refused: "Copy the library first (Settings > Organize, copy mode), then tag the copy." | the copies at that run's `to_path`s, never the originals | unchanged paths; values updated through overrides (as `db`) so the web app shows the match; a later `switch-root` makes the tagged copies the library and the overrides are deleted at switch |
| `db` | always | nothing | overrides + re-read |

The read-only default stays read-only: the default compose mounts `/music:ro`, so out of the box only `db` and (after a
copy run) `copy` are available (D-120).

Apply (`library` queue, `{"kind":"autotag-apply","runId"}`; serialised with scans and organizer runs):
1. Refuse (`JobError::Permanent`) if the target is no longer available, the album's tracks changed since the
   candidate was computed (a track added or removed), or the new identity collides with another album (§19.7). The
   §19.6 values are copied from the candidate into `autotag_runs.applied`, and `album_before` and the artist plan are
   recorded, before any file is touched.
2. Journal: one `planned` row per file with `before_sha` (whole file SHA-256), `audio_md5`, `old_tags` (every touched
   native key → its current values; `[]` when absent) and `new_tags`, committed before any file is touched.
3. Per file: safe write (§19.8); then in one transaction: row `written` with `after_sha` and `applied_seq`. Checkpoint
   (cancel token) between files.
4. One transaction: the album row, artist step and track fields of §19.7 (plus overrides for `db` and `copy`); then
   `reread_paths` for the album's library files.
5. `reorganize` (files only): create an organizer run (`mode=move`, `album_ids` = `[album id]` (stable, R1),
   `origin='autotag'`), run preview then apply in the same job; store `organizer_run_id`.
6. `autotag_albums.state='applied'`, `autotag_runs.state='done'`, summary
   `{written, skipped, failed, organized}`.

Crash recovery (`resume_interrupted`, as the organizer's): delete leftover `.kima-tag-<id>.*` files of the run; a
`planned` row whose file still hashes to `before_sha` is redone; one whose file's touched keys already equal `new_tags`
is marked `written` with the current hash; otherwise `failed` "changed outside kima-rs".

Undo (`{"kind":"autotag-undo","runId"}`):
1. If the run has an organizer run that is `done`, undo it first (organizer undo; its sha checks see the tagged
   content, which is what it recorded at apply).
2. Rows `written` in reverse `applied_seq`: the file must hash to `after_sha`, else `undo_failed` "changed since the tag
   write" (and the run ends `undo_failed` after the rest); write `old_tags` back through the same safe write and audio
   check; `undone`. Byte identity with `before_sha` is not expected (tag padding may differ); tag-value identity is
   checked by re-reading.
3. One transaction: restore the album row and reverse the artist step (§19.7), delete the run's overrides; then
   `reread_paths`. The collision check for the old identity runs before step 1, so a refused undo touches nothing.
4. `autotag_albums.state` back to `proposed`.

Copy-mode undo order matters: the organizer's copy undo deletes only copies whose SHA still matches its journal, so a
tagged copy survives an organizer undo until its tags are undone (§26).

## 20. Organizer rework (M3b)

**(controller's call, C15)** The autotagger reworks the organizer apply path and fixes the parked M1 minors 5–8 there.

### 20.1 Album scope

`organizer_runs.album_ids`: preview plans only tracks of those albums; `existing` (the collision set) still contains
every file under the source (move) or target (copy), so a scoped run never claims a name another album holds.
`switch-root` refuses a scoped run ("Only a whole-library copy can become the library").

Minors 2 and 5–8 and the resume-fsync nit are quoted from the M1 final review, `.claude/worktrees/m1-library/.superpowers/sdd/m1/final-review-report.md` ("Minor" list, and the
re-review's Task 17 note); the ledger is `progress.md` in the same directory.

### 20.2 Minor 5: copy-mode duplicate planning

The review: a copy-mode preview into a target that already holds an earlier copy plans ` (2)` duplicates of every
file, because `existing = dst_walk` (`apply.rs:262-266`) and `paths::plan` suffixes collisions; applying a second copy
run duplicates the library inside the target (the preview flags them; nothing is lost). Fix, as the review suggests:
when a planned target path already exists in the target with the source's size and SHA-256, the row is `unchanged`
(already copied), not a collision. The SHA is computed only when sizes match.

### 20.3 Minor 6: sidecar undo content check

The review: undoing a move puts sidecar rows back without checking their content. `move_one` checks the track path for
tracks only (`apply.rs:1077-1084`), and a same-filesystem `move_file` does not compare SHA, so whatever file now sits
at a sidecar's `to_path` is moved back to `from_path` (only when `from_path` is free, so nothing is overwritten). Fix:
undo of a sidecar row (`track_id NULL`) moves it back only if it still hashes to the journalled `sha256`; otherwise the
row is `undo_failed` "changed since the move" and the file stays. Track rows keep their path check.

### 20.4 Minor 7 and the resume-fsync nit

- **Minor 7** (the review): the `waiting` set is keyed by `(device, inode)` (`apply.rs:1017-1032`); in a library with
  hard-linked duplicates the inode leaves the set after the first of its rows although another row with the same inode
  is still waiting, which can turn a stage into a refuse or skip (safe: nothing is replaced). Fix: a multiset
  (`HashMap<(u64,u64), usize>`): insert increments, removal decrements, the key is waiting while its count is > 0.
- **Resume-fsync nit** (the re-review's Task 17 note: "on a resume after a process crash (not a reboot), folders
  created by the earlier attempt count as already existing and are not fsynced again"). In a cross-filesystem move,
  `move_file` fsyncs the new folders only up to the deepest folder that existed before `create_dir_all`
  (`apply.rs:558-585`). Fix: when the row was already started (its `sha256` is set, so an earlier attempt may have
  created the folders), the fsync walk goes up to the target root instead of stopping at the first pre-existing folder.

### 20.5 Minor 8: preview grouped by album

The review: the organizer preview is not grouped by album as M1 spec §5.2 says (`PreviewTable.tsx`), although the
backend sends `albumId`, `album` and `artist`. (Minor 8's other half, `OrganizerRow.id` typed `string`, was closed in
the M1 fix wave.) Fix: `GET /library/organizer/runs/{id}/albums` pages by album (20 albums a page, every row of each),
so a group never splits across pages. The frontend's `PreviewTable` becomes `AlbumGroupedPreview` (collapsible album
headers with row counts and issue chips), shared by the organizer page and the autotag review page (which shows the
planned moves of a `reorganize` apply).

`paths::Fields.disc_total` is now filled from `tracks.disc_total` (always 0 before, `apply.rs:216`).

### 20.6 Sidecars and minor 2 (symlinks)

- `paths::is_sidecar` gains `lrc` and `nfo` (now: jpg jpeg png gif webp bmp cue log lrc nfo) **(controller's call,
  C15)**.
- **Minor 2** (the review): the watcher and single-file update follow symlinks and the reconcile does not.
  `update_path` uses `tokio::fs::metadata`, which follows links (`scan.rs:685`), and `expand` lets WalkDir follow a
  symlinked folder root (`watch.rs:546-556`); so owner-made symlinked files or folders are indexed by the watcher, then
  marked missing by the next reconcile, and flip back and forth; the stream endpoint serves the link target
  (`routes/library/stream.rs:265`). The review's fix: use `symlink_metadata` and refuse symlinks in `update_path` and
  `expand`. M2 parked it with a condition (M2 design §9.3): an upgrade path through the needs-attention report, and
  never purging a track whose only fault is being a symlink. M3b does both:
  - `update_path` and `expand` use `symlink_metadata` and refuse symlinks, so no new symlinked file is indexed by any
    path (the walk already does not follow them);
  - a track already indexed through a symlink (file or ancestor folder under the root) whose target exists is treated
    as present by the reconcile and by `update_path` (never marked missing, so never purged), and the needs-attention
    report lists its album under a new issue code `symlink`, so the owner can replace the link with the real file;
  - the stream endpoint keeps serving such tracks (refusing them would break playback of indexed tracks); the
    organizer and the tag writer refuse them (§19.8) (D-123).

## 21. Frontend changes

**(controller's call, C16)** The existing Next.js pages that call these routes work against kima-rs without layout
changes; the autotag review page is new and goes into the Next.js frontend under Settings > Library Tools next to
Organize. Small, non-layout changes are needed where write-only secrets and the env lock meet the existing UI (§27 Q5).

### 21.1 Existing pages

- **Library Enrichment** (`CacheSection.tsx`): works as is against §16.2. Changes: when `autoEnrichMetadataLocked`,
  the "Auto enrich metadata" toggle is disabled with the note "Turned off on the server (KIMA_IDENTIFY=off)"; the
  toggle's description becomes "Look up MusicBrainz, AcoustID, artist info and lyrics online".
- **Artwork** (`ArtworkSection.tsx`): for `fanartApiKey` and `acoustidApiKey`, when `has…` is true the password field
  shows the placeholder "Saved — type to replace" and a "Remove" link that sends `null`; "Test connection" works with an
  empty field (tests the stored key). `types.ts` gains `hasFanartApiKey`, `hasAcoustidApiKey`,
  `autoEnrichMetadataLocked`.
- **Artist page**, **album page**, **MetadataEditor**: unchanged; they now get data.
- **Lyrics** panel and overlay: unchanged.
- **Needs attention** (`app/settings/attention/page.tsx`): the `symlink` code's label "Symlinked files", and a
  "Match" button per album linking to the review page.
- **Organizer** (`app/settings/organize/page.tsx`): `AlbumGroupedPreview` replaces `PreviewTable` (§20.5).
- **Library Tools** (`LibraryToolsSection.tsx`): a third link "Match albums" → `/settings/autotag`.
- `lib/api.ts`: the autotag calls; `getFeatures` type gains `fpcalc`; `EnrichmentFailure` keeps its type.

### 21.2 New pages (static export: query parameters, parent spec §9)

- `app/settings/autotag/page.tsx`: admin-only list with filters (state, recommendation, text), selection, "Find
  matches", "Match all unmatched", capability banner (which targets are available and why not), live progress from
  `autotag:progress`, and the recent runs with Undo.
- `app/settings/autotag/album/page.tsx?albumId=…`: the review page (§19.5): candidate list (distance as a bar,
  recommendation chip, origin, penalties on hover), changes table, mapping diff (local ↔ release, missing and extra rows
  highlighted), search box (artist/album or MBID/URL), target selector with the capability reasons, "Re-organize after
  writing" checkbox (files only), Accept / Skip. When the selected candidate collides with another album (§19.7) the page
  names and links that album, disables Accept, offers "Apply MBIDs only", and shows "Merge (later)" disabled. A
  candidate older than 30 days shows "Refresh candidates".
- Components in `features/autotag/` (`CandidateList`, `ChangesTable`, `MappingDiff`, `TargetPicker`), reusing
  `features/organizer/components/{IssueChip,ProgressBar,NotAvailable}` and the new `AlbumGroupedPreview`.

### 21.3 Frontend tests

- Playwright (CI): open Settings > Library Tools > Match albums against a seeded database with a stored candidate (no
  network), open the album, see the strong recommendation, Accept with target "Database only", see the album title
  change on the album page, Undo, see it back.
- `features/autotag` unit tests for the changes table's hide-unchanged and the mapping diff's missing/extra rows.

## 22. Configuration

New env vars (`kima_core::config`, README):

| Var | Default | Meaning |
|---|---|---|
| `KIMA_IDENTIFY` | `on` | `off` turns off every outbound lookup (§15.3) |
| `KIMA_CONTACT` | `https://github.com/d1mmu-dev/kima-rs-hub` | contact in the User-Agent (§7.1) |
| `MUSICBRAINZ_BASE_URL` | `https://musicbrainz.org/ws/2` | upstream's name; a mirror, or the test replay server |
| `ACOUSTID_CLIENT_KEY` | — | upstream's name; second in the key order (§8.4) |
| `ACOUSTID_DISABLED` | `false` | upstream's name; skips the AcoustID rung, `fpcalc:false` |
| `FANART_API_KEY` | — | upstream's name; used when no key is stored |
| `KIMA_FINGERPRINT_WORKERS` | half the cores, at least 1 | §9.3 |

Base URLs for the other sources are not env-configurable; tests inject them through `NetConfig` (§24.1). An invalid
`KIMA_IDENTIFY` value fails startup with a clear message.

## 23. Divergences to log

Each row goes into `docs/DIVERGENCES.md` in the commit that introduces it. Status `open` unless noted. M3a: D-106–D-118
and D-124; M3b: D-119–D-123.

| ID | Area | Upstream | kima-rs |
|---|---|---|---|
| D-106 | Identification | `fpcalc` binary run by Node; fingerprint also read from an `acoustid_fingerprint` tag; `features.fpcalc` from a `fpcalc -version` probe; text keys built with a 1613-entry `unaccent` table matching Postgres | In-process `rusty-chromaprint` on ffmpeg's 11025 Hz mono s16le output (first 120 s); no `fpcalc` in the image; the tag fingerprint is not read; `fpcalc` true when ffmpeg is present and `ACOUSTID_DISABLED` is not set; text folding is NFKD plus a 20-letter table, tested with upstream's cases (C2) |
| D-107 | Library | Scanner reads recording/artist/release-group MBIDs and ISRC; ignores genres; reads embedded lyrics | Also reads release, release-track and album-artist MBIDs, disc/track totals and genres; file genres stored as a separate source below MusicBrainz; album MBIDs are the mode over the album's tracks (C4) |
| D-108 | Enrichment | `TrackStage` with `blocked`, `retryable`, decider columns; BullMQ jobs per item; Redis state; a stalled fingerprint stage (no `fpcalc`) holds recordings back; 24 h "waiting" admin notification; completion alert | `track_stages` (C5) is itself the work queue for one coordinator job; no `blocked` (nothing sets it), `skipped` always false; retry due and terminal told apart by `next_attempt_at`; state in Postgres; when fingerprinting is unavailable the gate counts as settled and AcoustID is skipped; no notifications until M8 |
| D-109 | Outbound HTTP | p-queue limiters, Redis caches with per-service TTLs, UA `Kima/<v> (<upstream repo>)` | One `kima-net` client with per-source limits, a Postgres `http_cache` (answers 30 d, absences 7 d, shorter where upstream is shorter), pauses persisted in `source_pauses`, UA `kima-rs/<v> ( <KIMA_CONTACT> )`; AcoustID answers cached too; transient errors retried by the ledger only (C6, C19) |
| D-110 | Settings | `GET /system-settings` returns secrets decrypted; `test-fanart` needs the key in the body and answers 500 on any failure | Secrets write-only: `""` with `hasFanartApiKey`/`hasAcoustidApiKey`; POST `""` = unchanged, `null` = clear; stored in `settings_kv` encrypted; `test-fanart` uses the stored key when none is sent, answers 400 (rejected key), 409 (lookups off), 502 (no answer) (C8) |
| D-111 | Settings | `autoEnrichMetadata` saved and shown but never read | It is the identification switch; `KIMA_IDENTIFY=off` overrides it and the UI shows the lock (C19) |
| D-112 | Enrichment | `enrichment:progress` only from vibe routes; routes `/enrichment/artist/:id`, `/album/:id`, `/retry`, track metadata PUT/reset, image and genre backfills, artist counts, `album-cover`, `cover-art-colors`, `clear-caches` | Emitted by the coordinator; those routes are not ported (the frontend never calls them) (C9) |
| D-113 | Library | Artist images as `native:` cache paths or external URLs proxied by the cover route; page views fetch similar artists and their images live; `SimilarArtist` relation rows | Images downloaded into `<data>/artist-images`, served by `GET /library/cover-art/<artistId>` and Subsonic `getCoverArt ar-<id>`; page views never call out; similar artists outside the library have no image; no relation table until M5. Amends D-043, D-048 |
| D-114 | Genres | Genres are MusicBrainz only; Subsonic and the genre list read artist genres; `getGenres` adds each artist's whole album and track counts to every genre; `getSongsByGenre`/`byGenre` match the artist's genres by substring | Track, album and artist genres each from MusicBrainz, else file tags (C4); Subsonic counts distinct tracks and albums of the effective track/album genres; genre matches are case-insensitive exact on the genre; song `genre` is the track's first genre; `/library/genres` keeps upstream's heuristics over artist genres |
| D-115 | Lyrics | Embedded rows are never removed | A re-read file without lyrics deletes its `embedded` row; with lookups off or a limiter wait over 5 s the route answers from storage (C10) |
| D-116 | Subsonic API | `getAlbumInfo(2)` empty stub without id check; `getArtistInfo2` sends `heroUrl` as image URLs; songs' `genre` is the artist's first genre | `albumInfo` carries `musicBrainzId`, 70/10 for unknown/missing id; no image URL attributes; `songLyrics` advertised; songs, albums and artists carry `musicBrainzId`; amends D-075, D-082, D-086 (C11) |
| D-117 | Identification | `Album.rgMbid` (unique) only from tags; `Artist.mbid` unique; the tracklist rung picks the earliest official release of the group; `fillAlbumFromCatalogue` sets `Album.year` | Same tag-only album identity (C12), plus `release_mbid` stored; album MBIDs carry a source with precedence user > autotag > tag (R2); MBID columns not unique (rows are keyed by name), the editor still answers 409 for an MBID another row holds; a tagged release is used directly for the tracklist; album year is never filled from MusicBrainz (it is part of the album identity) |
| D-118 | Metadata | Editor PUT admin, reset any user; image overrides stored as URLs; `sortName` follows display names; MBIDs accepted unvalidated | Reset admin-only; image URLs downloaded once and stored; sort names stay generated from the real name; a non-MBID value is 400 |
| D-119 | Library | No album matching, no candidate search UI | Beets-style autotagger: MusicBrainz release candidates (snapshotted, apply offline), album rows updated in place with collisions refused (no merge), weighted distance (beets weights; strong < 0.04, medium < 0.2), admin review page, optional auto-apply of strong matches (off by default) (C13) |
| D-120 | Library | Never writes audio-file tags | Writes Picard-compatible tags (§19.8; ID3v2.3 files stay v2.3) in files mode only to a writable library, in copy mode only to organized copies, never otherwise; database-only mode writes no file; journalled, verified by an audio-packet hash, undoable; the read-only default stays read-only (C14) |
| D-121 | Organizer | (kima-rs feature, D-069) | Album-scoped runs, `superseded` state, preview grouped by album, `.lrc`/`.nfo` sidecars, no duplicate copies on a second copy run, content check on sidecar move undo, hard-link-safe `waiting` set, full fsync walk on resume. Amends D-069, D-070 (C15) |
| D-122 | Frontend | — | New Settings > Library Tools > Match albums pages in the Next.js frontend; ArtworkSection shows saved secrets as "Saved" with Remove; CacheSection shows the env lock (C16) |
| D-123 | Library | (kima-rs, M1 minor 2) | `update_path` and the watcher's `expand` refuse symlinks (`symlink_metadata`), as the walk already does; an already-indexed symlinked track with a live target is never marked missing and is listed in needs-attention (`symlink`); streams still serve it, the organizer and tag writer refuse it. Amends D-068 |
| D-124 | Identification | AcoustID default client key `JaLCkK9Xvt` (upstream's registered app) | kima-rs's own registered default key (§27 Q1) |

## 24. Testing

### 24.1 Fixtures strategy and the network guard

**(controller's call, C18)**
- **No test hits the network.** `NetConfig` carries every source's base URL; tests point them at a local replay server.
  A CI guard: `kima-net` in `cfg(test)` and under the env `KIMA_NET_DENY_REAL=1` (set in CI) refuses any host outside
  `127.0.0.1`/`localhost` with a panic naming the URL.
- **MusicBrainz**: port upstream's recorded fixture `__tests__/fixtures/musicbrainz/lidarr-pipeline.json` and the replay
  helper `__tests__/helpers/musicbrainzReplay.ts` as `kima_net::testing::Replay`: an axum server on an ephemeral port
  that answers recorded `(method, path, sorted query, body sha)` keys, records which were asked, and answers 599 (an
  assertion failure) for anything unrecorded. Request counts are asserted (upstream's "exact increments" tests).
- Recording new fixtures: `tools/record-fixtures` (a small Rust binary using `kima-net` with a recording layer), run by
  hand, never in CI; it refuses to run when `KIMA_NET_DENY_REAL` is set. Fixture files are JSON
  `[{method, url, status, headers:{retry-after?}, body}]` under `crates/kima-ident/tests/fixtures/<source>/`.
- **AcoustID and LRCLIB**: recorded fixtures, captured once with the recorder from real requests for a few public
  tracks, plus hand-written edge cases from upstream's test bodies (`acoustidService.test.ts`: below-floor scores,
  results without recordings, `status:"error"`).
- **Wikidata, fanart, Deezer, ListenBrainz, CAA**: recorded fixtures for one artist each (The Beatles MBID), plus
  hand-written 404 and 429 cases.
- **Library fixture**: ffmpeg-generated tones under a temp root (M1's helper), tagged by a test helper with lofty:
  Picard-style tags (recording MBID, ISRC, genres, USLT and SYLT) on some, nothing on others.
- **Chromaprint**: the known-answer test (§9.5).

### 24.2 Upstream tests ported

| Upstream test | Port |
|---|---|
| `recordingMatch.test.ts` (10) | all cases, `kima_ident::text` |
| `titlesAgree.test.ts` (10) | all cases |
| `albumIdentity.test.ts` | `isGenericAlbumTitle` list and the identity-key cases that apply to `fold_identity`; `resolveAlbum`/`editionlessKey` dropped (kima-rs album identity is D-039) |
| `artistIdentity.test.ts`, `artistIdentityAmpersand.test.ts` | `normalize_artist_name`, `artist_identity_key`; `parseCredit` cases go to `names::parse_credit`; `pgIdentityParity.test.ts` dropped (D-106) |
| `resolveArtistMbid.test.ts` (10) | all; the `getArtistCorrection` cases dropped (acquisition, M7) |
| `musicbrainz.identification.test.ts` (10) | all, against the replay server |
| `musicbrainz.albumCatalogue.test.ts` (6) | all, real replies |
| `rateLimiterBackoff.test.ts` (2) | `Retry-After: 0` falls back to exponential; a real `Retry-After` is honoured |
| `wikidata.outcome.test.ts` (5) | all |
| `acoustidService.test.ts` (16) | all, incl. the exact request body with `%20` and key precedence |
| `acoustidIdentification.test.ts` (8) | path escaping refused; `acoustid_id` tag never a recording id; recording tag asks nothing; stored fingerprint used; the fpcalc and tag-fingerprint cases become decoder cases |
| `fpcalc.test.ts` (3) | decoder: non-zero ffmpeg exit gives code and stderr; stdout never parsed after a failed exit |
| `acoustid-fingerprint.test.ts`, `unifiedEnrichment.acoustidOrder.test.ts` (~14) | the ladder order, deferral on a failed tracklist call, ISRC ordering, MB ISRC replacement |
| `getNextWork.test.ts`, `stageGating.test.ts`, `unifiedEnrichment.fingerprintGate.test.ts` | claim SQL, prerequisite, skip-with-evidence, `waiting` accounting |
| `enrichment-stale-reports.test.ts`, `enrichment-retry-cap.test.ts`, `enrichment-per-source-pause.test.ts`, `enrichment-inconclusive-errors.test.ts` | claim tokens and late reports; ladder 1 min/10 min/1 h, 4th failure terminal; 429 and MusicBrainz 503 pause only that source with the attempt unchanged; the four classes (the 24 h alert case dropped, D-108) |
| `unifiedEnrichment.redecideEverything/retryFailedAndBlocked/noWipe/missingSkipped/progressReadsTrackStage.test.ts` | §10.7 and §10.8 semantics |
| `enrichmentStateMachine.test.ts`, `enrichmentState.heartbeat.test.ts` | pause/resume/stop/sync; `hung`; activity stamped on count changes only |
| `enrichmentRestart.integration.test.ts` (11) | wipe semantics on Postgres |
| `artistFailureCap.test.ts`, `artistEnrichmentArtwork.test.ts` | cap 4 → unresolvable; drops another artist's MBID ("Alexis Ffrench holding Sam Cooke's") |
| `artwork.test.ts` (selected), `deezerVerifiedMatch.test.ts`, `deezerRelatedArtists.test.ts`, `artistSimilarity.tiers.test.ts` | source order, a throw is not "no", verified Deezer names, LB first then Deezer fill, dedupe, never throws |
| `imageStorage.test.ts` (5) | path traversal; plus SSRF cases for §12.6 |
| `enrichmentFailureService.test.ts`, `…trackStages.test.ts`, `enrichment.failuresTrackStages.test.ts` | the failure mapping and counts |
| `enrichmentStatus.route.test.ts`, `enrichmentSortName.route.test.ts` | `hung`; the sort-name cases inverted (D-118) |
| `systemSettings.acoustid.test.ts` (2) | inverted: encrypted at rest, never returned (D-110) |
| `routes/subsonic/__tests__/artistInfo.route.test.ts` | replaces the M2 stub snapshot |
| `frontend/hooks/__tests__/useLyricsSync.test.tsx` | runs unchanged |

### 24.3 New tests

- `kima-net`: limiter spacing (paused clock), concurrency cap, cache hit costs no cell, absence TTL, failures never
  cached, pause persisted and honoured after a new `Net` is built, secrets stripped from cache keys, `DISABLED` when off,
  the network guard.
- Scanner: every §8.1 field per format (MP3, FLAC, Ogg, Opus, M4A), mode rules for album MBIDs, embedded lyrics in each
  form, the stage reopen on a changed file and none on `TagWrite`.
- Heartbeat: a job longer than three leases completes without being reclaimed; a job whose row is reaped sees its token
  cancelled; `JobError::Permanent` fails at once; `enqueue_at` runs after `run_at` and not before; minor 3 and 4 tests
  (§11.4).
- Snapshot tests (`insta`, both formats) for every changed Subsonic endpoint in §18, with the M2 fixture extended by
  MBIDs, genres, lyrics and an artist image.
- Autotagger: distance table tests **(C18)**: `string_dist` against beets' own unit cases (`test_autotag.py`
  `StringDistanceTest`: equal strings, case, leading "the", featuring, parentheses, brackets, "&"/"and", accents,
  empty), plus non-Latin cases; per-key penalty tables (year ratio, length grace and cap, index by medium and absolute
  position); full album cases: a perfect match (d = 0, strong), one renamed track (medium), a missing track (capped
  medium), wrong year, VA compilation, a 2-disc release; candidate ordering tie-breakers; the Hungarian mapping with a
  bad pair broken.
- Tag writing **(C18, end to end)**: for each writable format, on copies in a temp dir: write → re-read → every
  touched key equals the new value, untouched keys (cover art, ReplayGain, a custom TXXX) unchanged, `audio_md5`
  equal; undo → every touched key equals the old value (absent stays absent); a forced lofty failure leaves the
  original byte-identical; a crash injected after the rename and before the journal commit is recovered as `written`.
- Apply targets: `files` refused on a read-only root (chmod'ed temp root); `copy` refused without a done copy run and
  never opens an original for writing (checked by an `inotify`-free mtime assertion on originals); `db` creates
  overrides that survive a forced rescan and are removed by undo; R1 identity: the album id is unchanged after apply and
  undo, a star, a `display_title` and a candidate survive both, the three artist steps (re-parent, rename in place,
  create) and their undo, a collision refuses apply (409 and in the job) and undo without touching a file, a rescan keeps
  the album's tracks on the row while tags agree and releases a re-tagged track; R2: tag < autotag < user precedence on
  rescan; R3: apply runs with `KIMA_NET_DENY_REAL` and a dead replay server (no request made); R4: a v2.3 MP3 stays
  v2.3 after write and after undo, a v2.4 stays v2.4; `reorganize` chains an
  organizer run and undo reverses both in order.
- Organizer minors 5–7 and the fsync nit (§20).

## 25. M2 leftovers (early M3a task)

**(controller's call, C17)** From the roadmap's M3 notes (M2 final review), done first in M3a:
1. The two limiter fold-key regression tests assume an `en_US` database locale. They probe with a wrong password
   instead of relying on locale-specific folding, so they hold under the `C` locale; CI runs them under both.
2. A request on a blocked fold key still charges the per-(IP, user) key, against M2 §5.7 ("a blocked attempt does not
   count as a new failure"). Fix: when either key is blocked, answer blocked and charge nothing; test both orders.
3. Docs: D-104's text says sign-in trims but `/rest` does not; correct it to what the code does. D-105 names bookmark
   `position` among the strict numbers. The D-105 sentence about `size=10.5` says it falls back to the default
   (upstream reads 10); make the row say exactly that.
4. Deferred task minors:
   - a test for `getNowPlaying`'s read-time filter of missing tracks;
   - `stars::set` maps a foreign-key violation (the item was purged between the check and the insert) to 70;
   - `getBookmarks` builds a `HashMap` of tracks instead of the O(n²) lookup, and `kind_of` is batched into one query
     for all starred ids;
   - `known_artist` uses `EXISTS` instead of loading albums;
   - `ffmpeg_guard` moves into a shared dev-dependency crate `crates/kima-test-support` used by the three test crates;
   - `updateUser` with `adminRole` and `password` commits once (one transaction for both).

## 26. Risks

| Risk | Mitigation |
|---|---|
| `rusty-chromaprint` output differs from `fpcalc` (resampling, algorithm preset), so AcoustID matches less | Decode straight to 11025 Hz mono so chromaprint does not resample; the known-answer test against a committed `fpcalc` output (§9.5); preset `test2`; if the crate lacks the compressor, port `fingerprint_compressor.cpp` with its upstream test vectors |
| MusicBrainz at 1 request per 1.1 s makes a first identification of a large library slow (one to three requests per untagged track: about 5–10 h for 20 000 tracks) | Tags first (rung 1 asks nothing); the cache makes reruns free; progress is visible and paused cleanly; `MUSICBRAINZ_BASE_URL` points at a local mirror for big libraries (README) |
| lofty's v2.3 writing (`use_id3v23`) converts frames (dates, multi-values) imperfectly | v2.3 round-trip tests per touched frame; the re-read verification after every write (§19.8) refuses a file whose touched keys do not read back |
| A tag writer bug damages audio | The audio-packet MD5 check on every write (§19.8); writes go to a hidden copy and replace the original only after verification; journal and undo; round-trip tests per format |
| A database-only match drifts from the files | Overrides are merged in the scanner's resolve, so rescans keep them; undo deletes them and re-reads |
| Album identity changes merge or split albums and lose album-level user state | The album row is updated in place and keeps its id (R1, §19.7); a collision refuses the apply instead of merging; the scanner keeps an autotag album's tracks on its row while their tags agree |
| Copy mode leaves tagged copies that the organizer's copy undo refuses to delete | By design (a changed copy is never deleted); the autotag undo runs first and restores tags; the review page says so |
| Wikidata and Deezer name matches attach the wrong photo | Upstream's verified-name rules and the MBID guard (§12.1, §12.3); MetadataEditor overrides; reset |
| Fingerprinting competes with transcodes | Half the cores by default (§9.3); `KIMA_FINGERPRINT_WORKERS` |
| An outbound call in a test leaks to the internet | Injected base URLs, the replay server's 599 for unknown requests, and the CI guard (§24.1) |
| `enrichment:progress` floods the SSE bus during a large run | At most one event per second, and the frontend only invalidates a query |
| Users of upstream expect secrets to be shown | The settings page says "Saved"; write-only is the safer default (D-110) |

## 27. Open questions for the builder

1. **AcoustID default key.** C8 orders "setting, env, built-in default". Upstream's default `JaLCkK9Xvt` is the key
   AcoustID issued to the kima-hub application; reusing another application's key is against AcoustID's terms of use
   for application keys. The owner registers a kima-rs application at `acoustid.org/new-application` and the key goes
   in as the constant; until then the constant is empty and the AcoustID rung is skipped until an admin enters a key
   (§2, controller's call C8). The builder should not copy upstream's key.
2. **`rusty-chromaprint` version.** Confirm the newest published version exposes `FingerprintCompressor` (or plan the
   port in §9.1 step 4) and that `Configuration::preset_test2()` matches chromaprint's default algorithm.
3. **Release-group genres at apply time.** Closed by ruling R3: the release group is snapshotted at search time
   (§19.2 step 6), one request per candidate. If the builder finds the release lookup's `release-groups` include
   already carries the group's genres, that request can be dropped.
4. **`getAlbumInfo` notes.** C11 asks for "notes and MBID". Neither upstream nor M3a has an album description source,
   so `notes` is omitted. A Wikipedia summary for the release group (Wikidata `P436`) would be a new source; left out
   unless the owner wants it.
5. **"Without layout changes" (C16).** Write-only secrets need the ArtworkSection "Saved / Remove" affordance, and the
   env lock needs a disabled toggle with a note (§21.1). Both are small and inside existing rows; flag if the owner
   reads C16 as "no frontend edits at all" (then: secrets would have to be echoed masked, which C8 forbids).
6. **ID3v2.3 files.** Closed by ruling R4: v2.3 files stay v2.3 (`WriteOptions::use_id3v23`), and undo restores the
   original version (§19.8). The earlier premise that lofty writes only v2.4 was wrong.
7. **Track-level user overrides.** The frontend has no track editor mount, so the track PUT/reset routes are dropped
   (§16.4). If a track editor is wanted later, `display_title`/`display_track_no` columns are a small add.
8. **`getSongsByGenre` matching.** Exact, case-insensitive (D-114). If a client relies on upstream's substring match
   (`rock` matching `post-rock`), switch the comparison; it is one predicate.

## 28. Calls index

**Controller's calls C1–C19** (binding), where each lands:

| Call | Subject | Sections |
|---|---|---|
| C1 | M3a/M3b split | §2, §4 |
| C2 | rusty-chromaprint via ffmpeg pipe; `fpcalc` feature true with ffmpeg | §9, §9.4, D-106 |
| C3 | Upstream ladder and gates; recording MBID stored; tests ported | §8, §24.2 |
| C4 | Scanner reads MBIDs, ISRC, genres, lyrics; file genres a lower source | §8.1, §13.1, D-107 |
| C5 | `track_stages` columns, stages, retry ladder, classes, rate limits free, gate | §6.4, §10, D-108 |
| C6 | One HTTP wrapper, per-host buckets, UA, cache TTLs, Outcome | §5, §7, D-109 |
| C7 | Heartbeat, delayed enqueue, non-retryable errors; 6 h lease removed | §11 |
| C8 | Encrypted settings store, key order, write-only secrets, `test-fanart`, no Last.fm | §15, §3, D-110 |
| C9 | Enrichment parity, sources, artist `getCoverArt`, routes the UI calls, `enrichment:progress` | §12, §16, §17, D-112, D-113 |
| C10 | LRCLIB + embedded, `track_lyrics`, 30-day none cache, Subsonic lyrics, `songLyrics` | §14, §18, D-115 |
| C11 | Subsonic fill-ins; similar songs stay stubs; D-086 updated | §18, D-116 |
| C12 | No automated album identity beyond tags | §3, §8.4, D-117 |
| C13 | Autotagger candidates, distance, thresholds, review, apply, journal | §19, D-119 |
| C14 | Writable-location rules; db-only; read-only default | §19.9, §3, D-120 |
| C15 | Organizer rework, minors 5–8, `.lrc`/`.nfo`, minor 2 via attention | §20, D-121, D-123 |
| C16 | Frontend in Next.js; review page under Library Tools | §21, D-122 |
| C17 | M2 leftovers early in M3a; minors 3–4 with the heartbeat | §25, §11.4 |
| C18 | Testing: replay fixtures, recorded AcoustID/LRCLIB, chromaprint KAT, distance tables, tag round trips | §24 |
| C19 | On by default; one switch plus `KIMA_IDENTIFY=off`; limits across restarts | §15.3, §7.3, D-111 |

**Builder-check rulings R1–R4** (controller's calls, binding):

| Ruling | Subject | Sections |
|---|---|---|
| R1 | Apply/undo keep the album row (stable id), artist re-parent / rename-in-place / create, collisions refused, scanner keeps autotag albums by id | §6.8, §19.7, §19.9, §16.5, §21.2, §24.3 |
| R2 | `release_mbid_source`/`release_group_mbid_source`, precedence user > autotag > tag | §6.3, §8.1, §19.6, D-117 |
| R3 | MusicBrainz snapshot on the candidate; apply offline; refresh after 30 days | §6.8, §19.2, §19.6, §27 Q3 |
| R4 | ID3v2.3 stays v2.3 (`use_id3v23`), undo restores the version | §6.8, §19.8, §26, §27 Q6 |

**Calls this design makes** (each is cheap to reverse):
1. Two new crates, `kima-net` and `kima-ident`; tag writing in `kima-library` (§5). If wrong: merge `kima-net` into
   `kima-ident`; mechanical.
2. `track_stages` is the work queue; one coordinator job instead of a job per item (§10.6). If wrong (per-item retries
   in the jobs table are wanted): enqueue per claim; the stage SQL is unchanged.
3. MBIDs as `uuid`, not unique (§6.3).
4. The AcoustID fingerprint in its own table (§6.3).
5. Album year never filled from MusicBrainz (§8.5); the autotagger writes it into files.
6. A tagged release MBID is used directly for the tracklist rung (§8.3).
7. When fingerprinting is unavailable the recording gate counts as settled (§9.4).
8. Fingerprint workers default to half the cores (§9.3).
9. Artist and album fallback images stored outside the evictable cache, served by id through the existing cover route
   (§12.6).
10. Page views make no outbound calls (§12.8).
11. `autoEnrichMetadata` is the identification switch (§15.3).
12. Secret POST semantics `""` = unchanged, `null` = clear (§15.2).
13. Subsonic genres from effective track/album genres with exact matching (§18).
14. Media and country as ordered tie-breakers, not weighted penalties (§19.3); `string_dist` keeps non-Latin letters.
15. Database-only matches are scanner-merged overrides (§19.7).
16. Safe write through a hidden copy plus an audio-packet MD5 check (§19.8).
17. Copy mode tags an existing done copy run rather than starting one (§19.9).
18. Extra local tracks receive album-level fields so the album stays together (§19.6).
19. MetadataEditor image URLs are downloaded once; reset is admin-only (§16.3).
