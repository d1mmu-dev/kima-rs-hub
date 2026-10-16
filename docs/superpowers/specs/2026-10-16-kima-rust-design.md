# kima-rs: Rust rewrite of Kima — Design

Date: 2026-10-16
Status: Approved design, pending spec review
Upstream reference: https://github.com/Chevron7Locked/kima-hub (v1.11.0)
License: GPL-3.0 (derivative of kima-hub)

## 1. Intent

**Goal.** Re-implement Kima's backend (Node/TypeScript, ~76k LOC) and its two ML
analyzer services (Python, ~6k LOC) in Rust, keeping Kima's forked Next.js frontend.

**Why** (all three, equally):
- Performance and footprint: lower RAM, faster scan/enrichment, smaller image.
  The six-process supervisord container (postgres, redis, node backend, next
  server, two Python analyzers) becomes two: Postgres and one `kima` binary.
- Learning: a clean, layered Rust codebase built in incremental milestones.
- Ownership: a type-safe codebase we control and can extend.

**Relationship to upstream.** This is an independent fork, free to diverge. The
HTTP API, DB schema and behaviour may change wherever that simplifies things.
The frontend is patched to match. Upstream code is reference material; upstream
test cases are a source of test cases.

**Divergence log.** Every difference from upstream (behaviour, data, API,
numerics, dropped features) is recorded in `docs/DIVERGENCES.md` in the same
commit that introduces it, with a status (`accepted`, `to-fix`, `to-decide`,
`open`, `resolved`), so it can be revisited later. A milestone is not done
until its divergences are logged.

**Non-goals (v1).**
- Migrating data from an existing Kima install (a possible later add-on).
- Bit-exact parity with Essentia's BPM/key output.
- Rewriting the frontend in Rust/WASM.
- Bull-board-style queue UI (replaced by a simple admin endpoint).

**Success criteria.**
- Each milestone (§8) ships a runnable server that does strictly more than the previous one.
- After M5: library, playback, Subsonic, identification, ML analysis and every vibe
  feature (similar tracks, map, Song Path, Blend, text search, mood mixes) work
  end-to-end from the forked frontend.
- Steady-state RSS of the `kima` process, idle with a 10k-track library, well
  under the upstream Node backend; enrichment memory bounded by the configured
  worker count rather than about 1–1.5 GB per worker.
- CLAP and musicnn embeddings match the upstream Python models (cosine ≥ 0.999).

## 2. Architecture

One Cargo workspace producing one binary, `kima`. Postgres (with pgvector) is
the only external service. ffmpeg is invoked as a subprocess for transcoding only.

```
kima-rs/
├─ crates/
│  ├─ kima-server/        binary: config, startup, router assembly, static frontend
│  ├─ kima-api/           /api/* handlers, auth middleware, SSE
│  ├─ kima-subsonic/      OpenSubsonic API (XML + JSON), token auth
│  ├─ kima-db/            sqlx queries, row types, migrations (module per domain)
│  ├─ kima-jobs/          Postgres job queue, worker pool, cron scheduler
│  ├─ kima-library/       scanner, tags, artwork, streaming, transcoding
│  ├─ kima-ident/         MusicBrainz, AcoustID/chromaprint, track & album matching
│  ├─ kima-ml/            ort sessions, mel frontends, musicnn heads, CLAP audio+text, DSP
│  ├─ kima-vibe/          similarity formula, ranking/caps, UMAP, Song Path, Blend, mixes
│  ├─ kima-integrations/  lidarr, soulseek, slskd, audiobookshelf, listenbrainz,
│  │                      podcasts/rss, deezer, spotify/youtube import (cargo features)
│  └─ kima-core/          shared types, error type, settings encryption, event bus
├─ frontend/              forked Next.js app, built with `output: "export"`
└─ docs/
```

**Boundary rules.**
- Domain crates (`library`, `ident`, `ml`, `vibe`, `integrations`) never depend
  on axum. `kima-api` and `kima-subsonic` are thin adapters over the same domain
  functions.
- `kima-ml` is pure computation with no DB and no HTTP:
  `analyze(&Path) -> Result<Analysis>`, `embed_audio(&Path) -> Result<[f32; 512]>`,
  `embed_text(&str) -> Result<[f32; 512]>`.
- `kima-vibe` math (formula, ranking, UMAP) is pure functions over in-memory
  data. A thin layer loads and stores data through `kima-db`.
- Each integration is a cargo feature; the default build enables all of them.

**Key dependencies.** axum, tower/tower-http, tokio, sqlx (postgres, migrate),
pgvector, jsonwebtoken, argon2, totp-rs, chacha20poly1305,
reqwest, serde, tracing, lofty, symphonia, rubato, realfft, ebur128,
rusty-chromaprint, ort, tokenizers, tokio-cron-scheduler, quick-xml, rss,
fuzzy matching via strsim/rapidfuzz-style crate, image (artwork resize).

## 3. Data model

- Fresh sqlx migrations. Upstream's 57 Prisma models are the starting point.
  Each domain's tables are redesigned as that domain is ported; legacy and
  bookkeeping tables are dropped where the rewrite does not need them.
- Kept: pgvector `vector(512)` for CLAP embeddings with an HNSW index (cosine);
  tsvector columns with triggers and GIN indexes for artist/album/track/podcast/
  audiobook search.
- Enrichment progress is modelled explicitly: a `track_stages` table
  `(track_id, stage, status, attempts, last_error, updated_at)` with status in
  `pending | working | done | failed`. Stages: `metadata`, `fingerprint`,
  `analysis`, `embedding`, `similarity`.
- Queries are compile-checked (`sqlx::query!` / `query_as!`), with the offline
  `.sqlx` data committed so builds do not need a live DB.

## 4. Jobs (replaces BullMQ + Redis)

- `jobs` table: `id, queue, payload jsonb, state, attempts, max_attempts,
  run_at, locked_until, last_error, created_at`.
- Claim: `UPDATE … WHERE id IN (SELECT … FOR UPDATE SKIP LOCKED LIMIT n)`.
  Workers are woken by `LISTEN/NOTIFY` with a polling fallback. A worker that
  dies leaves its job to be reclaimed after `locked_until` expires.
- Per-queue concurrency limits; exponential backoff on retry; dead jobs are kept
  for inspection.
- Cron via tokio-cron-scheduler. Cron entries only enqueue jobs (library sync,
  discovery, cleanup, integrity).
- Admin endpoint: queue counts, failed jobs, retry/discard.

## 5. Auth, sessions, events, secrets

- JWT bearer auth, as upstream does, so the frontend's token handling stays
  unchanged. HS256 signed with `SESSION_SECRET`. Access token 24h (claims
  `userId, username, role, tokenVersion`), refresh token 30d (`type: "refresh"`),
  stream ticket 4h (`scope: "stream"`, valid only on `/stream` paths). Bumping
  a user's `token_version` revokes all of their tokens. Accepted from the
  `Authorization: Bearer` header, `X-API-Key`, or `?token=` on media paths.
  `ALLOWED_ORIGINS` is honoured as upstream does.
- Passwords: argon2id. First registered user is admin (onboarding flow).
- 2FA: TOTP (totp-rs) with QR provisioning.
- API keys and per-app Subsonic tokens (revocable); Subsonic token+salt auth.
- Events: in-process `tokio::sync::broadcast` bus feeds an SSE endpoint. A
  single-use ticket (30 s, held in memory) authenticates SSE, as upstream does.
  At most 5 streams per user.
- Settings secrets encrypted with XChaCha20-Poly1305. `SESSION_SECRET` and
  `SETTINGS_ENCRYPTION_KEY` are generated into `/data` on first start if unset.

## 6. ML pipeline (`kima-ml`)

**Decode once.** symphonia decodes the file and rubato resamples it to 16 kHz
mono (musicnn, DSP) and 48 kHz mono (CLAP). Analysis reads the last 90 s (or the
whole track when shorter), as upstream does.

**CLAP sound vector.**
- Mel frontend reimplements upstream `clap_mel.py` (HF ClapFeatureExtractor of
  laion/clap-htsat-unfused): 48 kHz, FFT 1024, hop 480, 64 Slaney mel bands,
  50–14000 Hz, 10 s windows, repeat-pad.
- Five 10 s windows (upstream `compute_window_offsets`) go through `hf_audio.onnx`, then
  mean, then L2-normalise, giving 512-d.
- Text: `tokenizers` (RoBERTa BPE) feeds Xenova `text_model.onnx` (no attention
  mask, pad handling as in upstream `test_text_parity.py`), giving 512-d,
  L2-normalised.
- Parity gate: cosine ≥ 0.999 against Python-produced reference vectors on
  fixture clips.

**musicnn moods/feel.**
- Reimplements Essentia `TensorflowInputMusiCNN`: 16 kHz, frame 512, hop 256,
  96 mel bands, patches of 187 frames every 93, last partial patch dropped.
- `msd-musicnn-1.onnx` produces a 200-d embedding per patch. The heads are
  mood_acoustic/aggressive/electronic/happy/party/relaxed/sad, danceability,
  deam and emomusic (valence/arousal). Patches are fed in fixed-size groups.
- Parity gate: upstream ships `musicnn-answers.npz` plus its gate signal;
  cosine ≥ 0.999.

**DSP features (diverges from Essentia).**
- Loudness: `ebur128` (integrated LUFS, loudness range), exact.
- Key: chroma plus Krumhansl–Schmuckler profiles.
- BPM: onset-strength envelope plus autocorrelation/comb tempo estimate.
- Acceptance on a small labelled set: key ≥ 70% (relative major/minor counted
  correct), tempo ≥ 85% within ±4% or a 2×/½× octave. If the tempo target fails,
  bind `aubio` (GPL-3) via FFI.

**Runtime.**
- One shared `ort::Session` per model, loaded at startup (`DISABLE_CLAP` skips
  CLAP). Models are fetched or verified by sha256 manifest.
- Pipeline: N decode tasks feed a bounded channel into inference threads, so
  memory is bounded by the channel depth and not by per-process model copies.
- GPU: `ort` CUDA execution provider behind cargo feature `gpu` plus a runtime
  setting.

## 7. Vibe engine (`kima-vibe`)

Ported from upstream `services/matching/*`, `hybridSimilarity.ts`,
`songPath.ts`, `vibeMap.ts`:
- Score = sound (CLAP cosine, weight 1) + feel (valence/arousal standardised
  against the library, 0.25) + genre (MusicBrainz artist genres, cross-artist
  only, 0.25).
- Result caps: at most 2 per album and 4 per artist.
- Text queries score on sound only.
- Candidate retrieval uses the pgvector HNSW index; re-ranking with the full
  formula happens in Rust.
- Map: UMAP port of upstream `mapLayout.ts` (neighbour graph, principal start,
  fixed seed), with a layout version to force relayout. New tracks are placed
  with the UMAP transform rule and keep their positions.
- Song Path, Blend, mood buckets/mixes, radio stations and made-for-you mixes.

## 8. Milestones

Each milestone ends with a runnable server, a green CI, and a patched frontend
that hides features not yet available through the `features` endpoint.

| # | Milestone | Scope |
|---|---|---|
| M0 | Skeleton | Workspace, config/env, Postgres + migrations, job queue, sessions, users, first-admin onboarding, SSE, static frontend served by axum, Dockerfile |
| M1 | Library & playback | User management (admin create/list users, change password, registration toggle), scanner (lofty), artwork, browse, tsvector search, streaming with range requests, ffmpeg transcode 320/192/128, plays, playlists, likes, playback state |
| M2 | Subsonic | OpenSubsonic API (XML+JSON), per-app tokens, snapshot-tested responses |
| M3 | Identification | MusicBrainz, AcoustID via rusty-chromaprint, artist metadata enrichment, genres, lyrics (lrclib), enrichment stage tracking |
| M4 | ML analysis | `kima-ml` (CLAP, musicnn, DSP) as enrichment stages, worker setting, progress UI |
| M5 | Vibe engine | §7 in full |
| M6 | Podcasts & audiobooks | RSS subscribe/stream/progress, Audiobookshelf |
| M7 | Integrations | ListenBrainz (scrobble, similar artists), Deezer fallback, Lidarr, Soulseek/slskd, playlist import, weekly discovery |
| M8 | Hardening | TOTP 2FA + recovery codes, API keys and device linking, rate limiting, share links, notifications, Prometheus metrics, GPU image, docs |

## 9. Frontend

- Fork upstream `frontend/` into this repo and build it with `output: "export"`.
  axum serves it with an SPA fallback.
- Static export cannot serve arbitrary IDs on dynamic routes (`album/[id]` and
  9 others). Those pages move to query parameters (`/album?id=…`), and axum
  301-redirects the old path form (`/album/<id>` → `/album?id=<id>`), so old
  links and share URLs keep working.
- The Next.js route handlers (`app/api/events`, `app/api/events/ticket`,
  `manifest.webmanifest`) are removed: axum serves `/api/events` directly and
  the manifest becomes a static file.
- In M0, audit the frontend for anything static export breaks (route handlers,
  middleware, rewrites, dynamic server rendering) and replace it with
  client-side equivalents.
- API changes are made in the frontend's `lib/api.ts` in the same change as the
  backend.

## 10. Error handling and observability

- Domain crates use `thiserror` enums. `kima-api` maps them to HTTP status codes
  and a JSON `{ error, code }` body. `kima-subsonic` maps them to Subsonic error
  codes.
- External HTTP calls go through one reqwest client wrapper with timeouts,
  retries with backoff, and per-host rate limits (MusicBrainz is 1 req/s).
- Enrichment failures are recorded per track stage with a classified error
  code, and are retryable from the admin UI.
- `tracing` with JSON or pretty output, controlled by `LOG_LEVEL`. Prometheus
  metrics arrive in M8.

## 11. Testing

- Unit tests in each crate. Pure logic (formula, ranking, UMAP, mel, M3U
  parsing, matching) is tested without I/O.
- DB tests use `#[sqlx::test]` against real Postgres with pgvector
  (testcontainers in CI). The DB is never mocked.
- HTTP tests use `axum-test` against the assembled router. Subsonic responses
  are snapshot-tested with `insta`.
- ML parity tests use small fixture clips plus reference vectors, produced once
  by a Python script kept in `tools/` and committed.
- Upstream Jest suites for matching, trackMatching, m3uParser and soulseek search
  strategies are translated case by case.
- CI runs `cargo fmt --check`, `clippy -D warnings`, tests, and the frontend
  build. From M1 on, a Playwright smoke test is adapted from upstream
  `frontend/tests/e2e`.

## 12. Risks

| Risk | Mitigation |
|---|---|
| Essentia mel reimplementation drifts | Parity gate against upstream `musicnn-answers.npz` before M4 is done |
| BPM/key quality below Essentia | Labelled-set acceptance; aubio FFI fallback |
| Static export breaks parts of the frontend | Audit in M0; patch to client-side |
| Scope creep from 288 upstream routes | Port per milestone only what the frontend actually calls; drop dead endpoints |
| UMAP port numerics | Port upstream unit tests for `mapLayout.ts`; fixed seeds |
| ort/CUDA packaging | GPU is opt-in feature in M8; CPU path is the default |
