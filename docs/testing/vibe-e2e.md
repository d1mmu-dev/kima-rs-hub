# Vibe real-model end-to-end (M5 Task 18 Step 2)

**Status: IMPLEMENTED, NOT RUN.** No model download, conversion, private-asset provisioning,
owner-library access or database creation was authorized or performed for Task 18.

`crates/kima-api/tests/vibe_e2e.rs::real_music_clap_vibe_end_to_end_on_thirty_generated_tracks`
generates 30 WAVs in a temporary library (20 harmonic tones over three octaves, 10 white/low-passed
noises; 48 kHz mono, 12 s; no audio rights question), embeds them with the real native music CLAP
audio encoder, runs coordinator cycles (embedding, mood and similarity phases) and then calls
`/api/system/features`, `/api/vibe/similar`, `/map`, `/mood-summary`, `/mood/:id`, `/search`,
`/alchemy` and `/path` through `kima_api::router`. It requires `vibeEmbeddings: true`, every
track embedded and laid out, similar results without the seed and similarities in 0..=1, and 200
answers from every route. It lives in `kima-api` (not `kima-ident`) because it drives the router.

## Fail-closed prerequisites

Checked **before any database connection or creation**; any gap panics (never skips):

- `KIMA_CLAP_MODEL_DIR`: absolute directory with the native music audio/text export; the audio
  model, text model and `tokenizer.json` must each verify against their compiled SHA-256 pins.
- The snapshot store under the generated library must pass the reflink/mount check.
- `KIMA_NET_DENY_REAL=1` (the harness refuses otherwise). This keeps the run's profile consistent
  with CI; it is an environment check, not OS-level network isolation. The router state is built
  with `Net::disabled()`, so no outbound HTTP client is configured, but nothing in the harness
  blocks sockets. Run it in a network-isolated environment if that guarantee is needed.
- `KIMA_VIBE_ASSET_DIR`: absolute directory whose `feel_probe.json`/`mood_heads.json` verify
  against the compiled SHA-256 pins (none are compiled today, so this refuses).
- `ORT_DYLIB_PATH`: trusted ONNX Runtime library.
- The compiled native audio identity must equal `MATCHING_SOUND`. Today's `hf_audio` does not, so
  the harness refuses with "hf_audio is forbidden". There is no synthetic or fallback path.
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

The non-ignored `prerequisites_fail_closed_when_environment_is_unset_or_checkpoint_is_wrong`
proves the empty environment and the current `hf_audio` checkpoint both refuse;
`prerequisites_refuse_without_the_network_deny_switch`, `model_dir_without_pinned_files_refuses`
`cleanup_runs_when_the_body_panics_and_the_panic_is_kept`,
`a_failing_cleanup_never_replaces_the_body_panic` and
`panic_messages_are_readable_for_literal_formatted_and_other_payloads` cover the deny switch, the
model pins and the cleanup path.

## Command (only after the M4 music-checkpoint swap, asset pins and authorization)

```sh
KIMA_CLAP_MODEL_DIR=<private-music-model-dir> \
KIMA_VIBE_ASSET_DIR=<private-vibe-asset-dir> \
ORT_DYLIB_PATH=<trusted-libonnxruntime> \
DATABASE_URL=<isolated-empty-cluster-url> PGDATABASE=<db> \
KIMA_VIBE_E2E_AUTHORIZED_DATABASE=<db> KIMA_NET_DENY_REAL=1 \
cargo test -j2 -p kima-api --test vibe_e2e \
  real_music_clap_vibe_end_to_end_on_thirty_generated_tracks -- --ignored --exact
```

Prerequisites still open: owner questions Q1/Q3 (real-model acceptance, asset rights), M4's native
music audio/text/tokenizer and pipeline swap, compiled SHA-256 pins for the private vibe assets,
and a parent-provided isolated cluster. Generated tones/noise prove the plumbing on real
inference, not musical quality; Task 14 recall and Task 17 readings parity remain separate gates.
