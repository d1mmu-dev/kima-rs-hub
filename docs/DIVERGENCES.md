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
