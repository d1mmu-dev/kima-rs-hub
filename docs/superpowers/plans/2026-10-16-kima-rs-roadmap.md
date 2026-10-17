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
| M1 | Library & playback: user management, scanner, artwork, browse, search, streaming + transcode, plays, playlists, playback state (track likes dropped: D-071) | `2026-10-16-m1-library-playback.md` ✅ | M0 | Add cron scheduler (library sync) to `kima-jobs`; add `AdminUser` extractor (`require_admin` is still duplicated in `auth.rs` and `system_settings.rs`)<br>Error body: decided in M1, keep upstream's `{error}` (no `code`) |
| M2 | Subsonic (OpenSubsonic, per-app tokens) | after M1 | M1 | Port upstream `routes/subsonic/__tests__` cases as insta snapshots |
| M3 | Identification: MusicBrainz, AcoustID, artist enrichment, genres, lyrics | after M2 | M1 | Introduces `track_stages`; settings `Cipher` (XChaCha20) arrives when the first stored credential does (AcoustID key)<br>Job lease renewal (heartbeat) before long ML/enrichment jobs: 300 s lease otherwise double-runs them<br>Beets-style autotagger on top of the M1 organizer: MusicBrainz album match → review proposed tag fixes → write tags into files → re-organize (owner request) |
| M4 | ML analysis: `kima-ml` (CLAP, musicnn, DSP) | after M3 | M3 | Parity fixtures from upstream `musicnn-answers.npz` + Python-made CLAP vectors; pgvector migration |
| M5 | Vibe engine | after M4 | M4 | Port upstream `services/matching/__tests__` |
| M6 | Podcasts & audiobooks | after M1 | M1 | Can run in parallel with M3–M5 |
| M7 | Integrations | after M3 | M3 | Each integration behind a cargo feature |
| M8 | Hardening: 2FA, API keys, device link, rate limits, share links, notifications, metrics, GPU image, docs | last | all | Share-page link previews (D-012) can be served by axum injecting OG tags into the exported HTML<br>X-API-Key auth (R4)<br>Claims::kind fail-closed on unknown type/scope<br>Media URLs carry the access token in `?token=` (upstream behaviour); move to short-lived stream tokens |
| M9 | Rust/WASM frontend, part 1: new `kima-web` crate on [vertigo](https://crates.io/crates/vertigo) built with `vertigo-cli`, sharing API types with the backend; login, onboarding, library browse, search, player, playlists, settings | after M1 | M1 | Owner request: replace the forked Next.js frontend with Rust compiled to WASM<br>The Next.js export stays the served frontend until M10; the new frontend sits behind a switch, so both can run side by side<br>Port the Playwright specs so the same e2e suite runs against both frontends<br>Log the frontend replacement in DIVERGENCES.md when it lands |
| M11 | Casting: Chromecast and AirPlay. Pick a device from the player and play the queue on it. Chromecast through the Cast Web Sender SDK with the default media receiver. AirPlay through the browser's AirPlay picker on the audio element (Safari/WebKit). Server-side discovery (mDNS) lists devices on the LAN | after M9 | M9 | Owner request<br>The cast device fetches the stream itself, so media URLs need short-lived stream tokens that don't leak the login token (pulled forward from M8) and an address reachable from the LAN (`KIMA_PUBLIC_URL`)<br>Built straight into `kima-web`, the vertigo player; the Next.js frontend gets no casting<br>Upstream has no casting; log it in DIVERGENCES.md when it lands |
| M10 | Rust/WASM frontend, part 2: port the remaining pages (Subsonic tokens, identification, vibe, podcasts/audiobooks, integrations, hardening screens), make vertigo the default and delete `frontend/` | last | M9, M2–M8 | Drop Node from CI and the Docker build<br>Pages for milestones built after M9 go straight into `kima-web`, not the Next.js frontend |
