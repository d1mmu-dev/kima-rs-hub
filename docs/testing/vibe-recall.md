# M5 Task 14 — real text / real-vector recall gate

**Implemented, not executed.** Ordinary `vibe_recall` tests are explicitly synthetic
infrastructure/logic checks, not real-model acceptance. Task 14 Step 3 stays open.
Since D-194 (2026-11-18) the native audio encoder is the music checkpoint and equals the
engine's `MATCHING_SOUND`; the real harness still rejects any checkpoint mismatch **before
connecting**. Real execution waits for an owner-approved isolated copy of real embeddings. Do not relabel embeddings, edit the manifest
to bypass this, substitute a synthetic encoder, or count an ignored test as a pass.

## Prerequisites and permission

An owner-approved, isolated copy of **real current music-checkpoint embeddings**,
licensed private model files and the completed M4 checkpoint swap are required.
No download, database copy, live-library access or additional licence is authorized
by this document. Approximately 15,000 tracks is a planning input only; the report
records the actual eligible count. At least 20 eligible embeddings are necessary.
The harness does not migrate, seed, rebuild, create or drop anything in the database.
It uses a repeatable-read, read-only transaction and read-only connection default.

Before running, the operator must verify container/cluster ownership and catalogue
collisions independently, then explicitly authorize the copy. Never target the
application/development database. The real test is a plain ignored Tokio test, NOT
`#[sqlx::test]`; therefore SQLx cannot create/drop databases before its safety guard.
The separately named synthetic infrastructure test does use SQLx's test harness
and requires the usual authorized isolated test-cluster preflight.

## Private inputs (all outside Git)

Set these variables explicitly; do not paste credentials into a terminal transcript:

- `DATABASE_URL` and `PGDATABASE`: the exact authorized isolated copy.
- `KIMA_RECALL_DB_AUTHORIZATION`: path to a JSON object with **all** fields below.
  Host must be `127.0.0.1`, port at least 55474, and database must not be `kima`.
  The URL host/port/database/user and `PGDATABASE` must match before connection.
  After connecting, the server's database/user/system identifier and database comment
  must match before any corpus read. The operator obtains the identifier from
  `pg_control_system()` and the copy's comment from `shobj_description`; inability
  to read these is a hard refusal, not permission to weaken the check.

  | Field | Type / meaning |
  |---|---|
  | `host`, `database`, `user` | strings, exact approved identity |
  | `port` | integer, exact approved forwarded port |
  | `system_identifier` | string, independently verified cluster identifier |
  | `database_comment` | nonempty string, exact operator-owned copy marker |
  | `isolated_copy_read_only` | boolean `true`, explicit authorization |

- `KIMA_CLAP_MODEL_DIR`: absolute directory containing the native pinned audio,
  text ONNX and tokenizer assets. The harness uses the **production native text
  adapter**, never the injectable test adapter. Pinned SHA-256 verification and
  owned verified bytes are the existing production loader's responsibility.
  A lazy pool permits native asset verification before the first DB connection.
- `ORT_DYLIB_PATH`: the separately trusted native ONNX Runtime, as for production.
- `KIMA_RECALL_QUERIES`: UTF-8 JSON array of exactly **50 unique real text strings**,
  each 2–500 UTF-16 units after minimum-length whitespace validation (raw maximum
  500). Supply meaningful pre-trimmed queries. Duplicate trimmed strings, invalid
  JSON, missing files and wrong counts fail; there is no generated query fallback.
  Choose the corpus with the owner **before observing results**; record its origin
  and freeze its file hash. Do not cherry-pick queries or rerun only favourable ones.
  This implementation deliberately does not invent an English/Polish semantic
  corpus on the owner's behalf. The original file SHA-256 is reported alongside
  each submitted query's SHA-256; the private input preserves the actual text.
- `KIMA_RECALL_MANIFEST`: JSON object with all fields below. Unknown fields fail.

  | Field | Meaning |
  |---|---|
  | `checkpoint` | exactly `laion-clap-music-onnx-v1` |
  | `audio_model_id`, `audio_sha256`, `pipeline_version` | exact `MATCHING_SOUND` values; also must equal the native audio descriptor and pipeline |
  | `text_sha256`, `tokenizer_sha256` | exact compiled native descriptor pins |
  | `corpus_sha256` | predeclared lowercase SHA-256 of the actual eligible binary corpus, algorithm below |
  | `corpus_origin` | nonempty provenance/revision of the approved real-embedding copy (not a synthetic fixture) |
  | `query_origin` | nonempty provenance/version of the preselected 50-query input |
  | `licensed_private_use_confirmed` | boolean `true`; operator's explicit rights attestation, not a licence grant |

  Corpus digest algorithm: select `t.id, vector_send(te.embedding)` through the
  shared `ELIGIBLE` predicate, ordered by UUID. For every row append UUID's 16 bytes,
  big-endian u64 binary-vector byte length, then pgvector's `vector_send` bytes to
  SHA-256. There are no text float roundtrips. Generation-stale eligible embeddings
  are rejected. Compute this inventory only after authorization; keep the original
  dump/provenance. The harness compares its actual snapshot hash to the manifest.
- `KIMA_RECALL_REPORT`: new absolute JSONL output path outside the repository.
  Its parent directory must exist. Output is mode 0600 and never overwrites a file.
  The report includes vectors and track UUIDs: keep it private like the corpus.

JSON input reads are bounded to 1 MiB, reject nonregular final components and
symlinks, and use nonblocking/no-follow opens. Model verification follows the
production loader's stricter pinned-artifact rules. None of these inputs is
silently skipped or synthesized.

## Commands (only after the above permission/prerequisites)

Normal infrastructure checks (synthetic only):

```sh
SQLX_OFFLINE=true cargo test -p kima-ident --test vibe_recall
```

Real gate, with the required private environment already set:

```sh
SQLX_OFFLINE=true cargo test -p kima-ident --test vibe_recall \
  real_music_checkpoint_recall_at_20 -- --ignored --exact --nocapture
```

Do not execute the real command merely to demonstrate a missing prerequisite.
Its current blocked status is already known; ordinary unit tests cover fail-closed
manifest/identity validation without loading a model or connecting to a live DB.

## Measurement and evidence

For **every one of the 50 actual native text vectors**, compare exact top 20
against **raw HNSW top 20**, using the same SQL builders as production. No exact
short-result fallback is allowed in this measurement: missing neighbours count as
misses. Both paths use the same eligibility and metadata joins in the same snapshot.
The measured index must be valid/ready HNSW cosine, `m=16, ef_construction=200`;
query settings are the binding ef=100, relaxed iterative scan, 40000 tuple budget.
The approximately 15k library would normally use production's exact path; this
harness deliberately measures ANN independently of that threshold.

Planner forcing is **test-connection-local only**: ANN disables sequential,
bitmap and sort plan choices; JIT is off to avoid forced-cost compilation noise.
Each query must additionally have `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` showing
an actually executed scan of `track_embeddings_cosine_idx` with positive rows and
loops. A sequential scan, a different index, or an unexecuted plan fails the run.
Queries are unpersisted so a cached generic plan cannot silently replace the
explained parameterized statement. Production's planner settings are unchanged.

The flushed JSONL records contain model/checkpoint/pipeline pins, input/corpus
hashes and provenance, actual eligible count, PostgreSQL/pgvector versions, index
construction/query settings, actual query vector, exact/ANN UUIDs and distances,
per-query recall, native embedding/exact/ANN wall-clock latency and actual plan.
Latency is instrumented (ANN is warmed by EXPLAIN), **not** production API p95.
A final summary records all 50 queries, mean and minimum recall@20 and the
`mean >= 0.95` gate result. Preserve every low-recall query and failed attempt.
No summary or an interrupted/failed process means **no pass**, even if earlier
queries look good. Changing queries, ef settings, fallback or thresholds to obtain
a pass needs a separate owner decision; it is not an implementation repair.
