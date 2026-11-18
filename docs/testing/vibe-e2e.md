# Vibe real-model end-to-end (M5 Task 18 Step 2)

**Status: RUN, PASSED (2026-11-18, D-194)** on the private music CLAP export and the pinned private
feel probe, against an isolated test cluster; mood heads absent (blocked). See "Runs" below. This is
plumbing on real inference over generated audio, not the 20-real-track parity/recall acceptance.

`crates/kima-api/tests/vibe_e2e.rs::real_music_clap_vibe_end_to_end_on_thirty_generated_tracks`
generates 30 WAVs in a temporary library (20 harmonic tones over three octaves, 10 white/low-passed
noises; 48 kHz mono, 12 s; no audio rights question), embeds them with the real native music CLAP
audio encoder, runs coordinator cycles (embedding, mood and similarity phases) and then calls
`/api/system/features`, `/api/vibe/similar`, `/map`, `/mood-summary`, `/mood/:id`, `/search`,
`/alchemy` and `/path` through `kima_api::router`. It requires `vibeEmbeddings: true`, every
track embedded with the exact `MATCHING_SOUND` identity and laid out, 30 native zero-shot readings
(music text model), no mood scores and `/mood/:id` = `unanalyzed` (the mood heads are not
provisioned, so moods degrade on their own), similar results without the seed and similarities in
0..=1, and 200 answers from every route. It lives in `kima-api` (not `kima-ident`) because it
drives the router.

## Fail-closed prerequisites

Checked **before any database connection or creation**; any gap panics (never skips):

- `KIMA_CLAP_MODEL_DIR`: absolute directory with the native music export: `music_audio.onnx`,
  `music_text.onnx` and `tokenizer.json` must each verify against their compiled SHA-256 pins.
- The snapshot store under the generated library must pass the reflink/mount check.
- `KIMA_NET_DENY_REAL=1` (the harness refuses otherwise). This keeps the run's profile consistent
  with CI; it is an environment check, not OS-level network isolation. The router state is built
  with `Net::disabled()`, so no outbound HTTP client is configured, but nothing in the harness
  blocks sockets. Run it in a network-isolated environment if that guarantee is needed.
- `KIMA_VIBE_ASSET_DIR`: absolute directory whose `feel_probe.json` verifies against the compiled
  SHA-256 pin. `mood_heads.json` has no compiled pin, so it must not load (the harness asserts it).
- `ORT_DYLIB_PATH`: trusted ONNX Runtime library.
- The compiled native audio identity must equal `MATCHING_SOUND` (any other identity, such as M4's
  general `hf_audio`, is refused). There is no synthetic or fallback path.
- `DATABASE_URL` on `127.0.0.1` (not the development port), `PGDATABASE` naming the same database,
  and `KIMA_VIBE_E2E_AUTHORIZED_DATABASE` equal to it as explicit consent. The connected
  `current_database()` must match and must be empty (no relation, type or function in any
  non-system schema including `public`, no extra schema, no extension besides `plpgsql`); only
  then a uniquely named `kima_vibe_e2e_<uuid>` database is created and marked with a run comment.
  It is migrated and used, then dropped on success **and** when the test body panics; cleanup
  drops only that name and only while it carries this run's comment (or none). The body's panic
  message is logged before cleanup starts, and a panic during cleanup is logged without replacing
  the body's original panic. Cleanup is **not** guaranteed on SIGKILL, abort, a hang, Ctrl-C, or a
  panic inside a task the body spawned and never joined: the database then stays behind with its
  `kima vibe_e2e run <uuid>` comment and must be removed by hand. No `#[sqlx::test]` harness is used.

The non-ignored `prerequisites_fail_closed_when_environment_is_unset_or_models_are_missing`
proves the empty environment and a model directory without the pinned music files both refuse;
`prerequisites_refuse_without_the_network_deny_switch`, `model_dir_without_pinned_files_refuses`
`cleanup_runs_when_the_body_panics_and_the_panic_is_kept`,
`a_failing_cleanup_never_replaces_the_body_panic` and
`panic_messages_are_readable_for_literal_formatted_and_other_payloads` cover the deny switch, the
model pins and the cleanup path.

## Command

```sh
KIMA_CLAP_MODEL_DIR=<private-music-model-dir> \
KIMA_VIBE_ASSET_DIR=<private-vibe-asset-dir> \
ORT_DYLIB_PATH=<trusted-libonnxruntime> \
DATABASE_URL=<isolated-cluster-url-of-an-empty-database> PGDATABASE=<db> \
KIMA_VIBE_E2E_AUTHORIZED_DATABASE=<db> KIMA_NET_DENY_REAL=1 \
cargo test -j2 -p kima-api --test vibe_e2e \
  real_music_clap_vibe_end_to_end_on_thirty_generated_tracks -- --ignored --exact
```

The authorized database must be empty; on a cluster whose test database carries the SQLx registry
schema, point the harness at the cluster's empty maintenance database after verifying it read-only.

## Runs

- 2026-11-18, D-194 working tree (before commit): **1 passed, 0 failed** in 163.6 s; isolated
  dedicated test cluster, empty maintenance database verified read-only (0 foreign objects) before
  the harness ran; catalog identical before and after (the run's database was dropped).
- 2026-11-18, the D-194 commit's code on a clean tree: **1 passed, 0 failed** in 163.0 s, same
  checks. The M4 stage harness `kima-ident --test embedding_e2e -- --ignored --test-threads=1`
  on the music export, same cluster and guard: **7 passed, 0 failed** (273 s).

Generated tones/noise prove the plumbing on real inference, not musical quality. Still NOT RUN:
Task 14 real recall and Task 17 readings parity on 20 real tracks (no owner-library access).
