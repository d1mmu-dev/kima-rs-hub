# kima-rs Roadmap

**Spec:** `docs/superpowers/specs/2026-10-16-kima-rust-design.md`
**Divergence log:** `docs/DIVERGENCES.md` (every difference from upstream goes here, in the same commit)
**Upstream pin:** kima-hub @ `12faf1cd34ff53054221818b0ccde58b8e9b0020` (v1.11.0)

Each milestone gets its own detailed implementation plan, written when the
previous milestone has landed, so it builds on the real interfaces rather than
guessed ones. A milestone is done when: the server runs, CI is green, the
frontend works for everything the milestone covers, and its divergences are logged.

| # | Milestone | Plan | Depends on | Notes carried forward |
|---|---|---|---|---|
| M0 | Skeleton: workspace, config, secrets, DB, job queue, JWT auth, onboarding, SSE, static frontend, Docker | `2026-10-16-m0-skeleton.md` ✅ | — | — |
| M1 | Library & playback: user management, scanner, artwork, browse, search, streaming + transcode, plays, playlists, likes, playback state | written after M0 | M0 | Add `?token=` query auth + stream tickets (`POST /api/auth/stream-ticket`) for media paths; add cron scheduler (library sync) to `kima-jobs`; add `AdminUser` extractor<br>Close a user's open SSE streams when their token_version is bumped (change password / log out everywhere)<br>Frontend polls `/api/notifications/downloads/active` and `/api/discover/batch-status` after login (404 in M0): stub them or gate callers on /system/features<br>Legacy `?token=` on media/stream paths (R4)<br>Same-route id→id client navigation (e.g. similar album) must reset page state: verify in Playwright |
| M2 | Subsonic (OpenSubsonic, per-app tokens) | after M1 | M1 | Port upstream `routes/subsonic/__tests__` cases as insta snapshots |
| M3 | Identification: MusicBrainz, AcoustID, artist enrichment, genres, lyrics | after M2 | M1 | Introduces `track_stages`; settings `Cipher` (XChaCha20) arrives when the first stored credential does (AcoustID key) |
| M4 | ML analysis: `kima-ml` (CLAP, musicnn, DSP) | after M3 | M3 | Parity fixtures from upstream `musicnn-answers.npz` + Python-made CLAP vectors; pgvector migration |
| M5 | Vibe engine | after M4 | M4 | Port upstream `services/matching/__tests__` |
| M6 | Podcasts & audiobooks | after M1 | M1 | Can run in parallel with M3–M5 |
| M7 | Integrations | after M3 | M3 | Each integration behind a cargo feature |
| M8 | Hardening: 2FA, API keys, device link, rate limits, share links, notifications, metrics, GPU image, docs | last | all | Share-page link previews (D-012) can be served by axum injecting OG tags into the exported HTML<br>Bundled Postgres: scram password in /data (0600) + non-superuser app role (D-029)<br>X-API-Key auth (R4) |
