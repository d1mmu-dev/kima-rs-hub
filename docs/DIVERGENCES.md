# Divergences from upstream Kima

Every place where kima-rs behaves, stores or exposes something differently from
upstream kima-hub (reference: v1.11.0) is recorded here, so it can be revisited
later: fixed to match, kept on purpose, or rethought.

**Rule:** any change that makes kima-rs differ from upstream adds an entry in the
same commit. Entries are never deleted, only given a new status.

Status: `open` (not yet reviewed) · `accepted` (keep, intended) ·
`to-fix` (should match upstream later) · `to-decide` (needs thought) · `resolved`

| ID | Area | Upstream | kima-rs | Why | Status |
|---|---|---|---|---|---|
| D-001 | Data | Prisma schema, 57 models, 117 migrations | Fresh sqlx schema; tables redesigned per domain | Clean design, free to diverge | accepted |
| D-002 | Data | — | No import from an existing Kima install in v1 | Scope; possible later importer | to-decide |
| D-003 | Jobs | BullMQ on Redis, bull-board UI | Postgres job queue (SKIP LOCKED + LISTEN/NOTIFY), simple admin endpoint | Drop Redis; one less service | accepted |
| D-004 | Events | Redis pub/sub feeding SSE | In-process broadcast bus feeding SSE | Single process | accepted |
| D-005 | Auth | bcrypt password hashes | argon2id | Modern default; bcrypt verify only needed if importing (see D-002) | accepted |
| D-006 | Runtime | supervisord running postgres, redis, node, next, 2 Python services | Postgres + one `kima` binary | Footprint | accepted |
| D-007 | Frontend | Next.js server (`next start`) | Static export served by axum | No Node runtime; features that need a server get patched to client-side | accepted |
| D-008 | ML / DSP | Essentia RhythmExtractor2013 (BPM) and KeyExtractor | Own Rust onset/autocorrelation tempo and chroma + Krumhansl key | No Essentia in Rust; numbers will differ | to-decide |
| D-009 | ML | Two Python services, each decoding audio separately | One in-process decode shared by CLAP and musicnn | Memory/speed | accepted |
| D-010 | API | ~288 route handlers + Subsonic | Only the endpoints the frontend calls get ported, per milestone | Avoid porting dead endpoints; list dropped endpoints here as found | open |
| D-011 | Frontend | Dynamic path routes (`/album/[id]`, `/artist/[id]`, `/playlist/[id]`, `/mix/[id]`, `/podcasts/[id]`, `/podcasts/genre/[genreId]`, `/audiobooks/[id]`, `/audiobooks/series/[name]`, `/browse/playlists/[id]`, `/share/[token]`) | Query-param pages (`/album/?id=…`, `/artist/?id=…`, `/playlist/?id=…`, `/mix/?id=…`, `/podcasts/show/?id=…`, `/podcasts/genre/?genreId=…`, `/audiobooks/book/?id=…`, `/audiobooks/series/?name=…`, `/browse/playlists/view/?id=…`, `/share/?token=…`); axum permanently redirects the old path form (also `/library` → `/collection/`, `/setup` → `/login/`, `/import/spotify` → `/import/playlist/`), but serves real export files under those prefixes (RSC payloads such as `album/index.txt`, `__next.*.txt` segment files) | Static export can't serve unknown IDs on path segments | accepted |
| D-012 | Frontend | `/share/[token]` and `/library` are server-rendered (share page can emit link-preview metadata) | Client-rendered only; share links show no rich preview | Static export has no server rendering | to-decide |
| D-013 | Auth | Also accepts an express-session cookie (`req.session.userId`) | Bearer JWT, API key, or `?token=` only | No cookie sessions in kima-rs; the frontend uses bearer tokens anyway | accepted |
| D-014 | Events | SSE tickets stored in Redis (`sse:ticket:*`, 30 s) | Held in an in-memory map with a 30 s TTL | No Redis; single process | accepted |
| D-015 | Auth | User cache with 30 s TTL in the auth middleware | Version check hits the DB on every request (indexed PK lookup) | Simpler; add a cache only if profiling shows a need | open |
| D-016 | IDs | cuid string IDs | UUIDv7 | Native Postgres `uuid` type, time-ordered | accepted |
| D-017 | Config | No `KIMA_DATA_DIR`, `KIMA_FRONTEND_DIR`, `KIMA_BIND_HOST` (paths fixed by the image; Next.js serves the frontend) | New env vars `KIMA_DATA_DIR` (default `/data`), `KIMA_FRONTEND_DIR`, `KIMA_BIND_HOST` (default `0.0.0.0`) | One binary serves everything and must be runnable outside Docker | accepted |
| D-018 | Secrets | Entry script generates `SESSION_SECRET` / `SETTINGS_ENCRYPTION_KEY` (persisted in the all-in-one image; regenerated every start in split images when unset) | The binary generates both once into `<KIMA_DATA_DIR>/secrets.json` (mode 0600); env vars override; a corrupt file stops startup instead of being replaced | Same behaviour on every deployment; never silently loses the encryption key | accepted |
| D-019 | Users | Usernames unique and matched case-sensitively | Unique and matched case-insensitively (`lower(username)`); login with "ANA" finds "Ana" | Prevents look-alike duplicate accounts | open |
| D-020 | Users | First-user check (`count == 0`) not serialised; two concurrent first sign-ups could both become admin | Registration runs under a Postgres advisory lock | Race fix | accepted |
| D-021 | Health | `/health` reports db, redis, embedding write path, Lidarr and analyzer liveness | Reports `{"status","checks":{"db"}}` only; more checks are added as subsystems land | Only the DB exists in M0 | open |
| D-022 | API errors | Validation failures return `details` as a zod issue array | `details` is a single human-readable string | No zod; simpler | open |
| D-023 | Auth | Login does not trim the username; register does not trim either | Both trim surrounding whitespace | Avoids "ana " accounts that can never log in from a phone keyboard | open |
| D-024 | Auth | 2FA login step, change-password, admin user management, stream tickets, API keys exist | Not in M0: user management + stream tickets arrive in M1, 2FA + API keys in M8 | Milestone order | open |
| D-025 | Onboarding | `/onboarding/lidarr`, `/audiobookshelf`, `/soulseek`, `/enrichment` steps; `complete` re-initialises Lidarr/Audiobookshelf and syncs audiobooks | Only `status`, `register`, `complete` | Those integrations arrive in M6/M7 | open |
| D-026 | Frontend | `/manifest.webmanifest` generated by a Next.js route handler | Static file in `public/`; served without the `Cache-Control: public, max-age=3600` header the route handler sent | Static export | accepted |
| D-027 | Frontend | `next/image` optimisation via the Next server | `images.unoptimized: true`; images served as-is | Static export | open |
| D-028 | Image | Bundles Redis, Node, Next.js server, Python analyzers; supervisord | Bundles Postgres + kima only; tini + a bash entrypoint; Postgres skipped when `DATABASE_URL` is set | Footprint (see D-006); external Postgres supported | accepted |
| D-029 | Image | Postgres listens inside the container with a password | Bundled Postgres trusts local connections on 127.0.0.1 only | Never exposed outside the container; no password to manage | to-decide |
