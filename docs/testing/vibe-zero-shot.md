# CLAP zero-shot readings: acceptance and private reference

Task 17 derives instrumentalness, acousticness, valence and arousal from current stored
512-dimensional music-checkpoint vectors, before mood statistics. It never decodes audio.
Missing text capability/checkpoint association skips only readings; learned moods and matching
retain their independent gates. No system feature predicate is enabled by Task 17.

## Checked-in reference (synthetic only)

`tools/vibe-zero-shot-reference.py --upstream <full-history-clone>` reads the immutable
`c7527577d925b20f5adf410c7fd4e0c8ac250f69:services/audio-analyzer-clap/analyzer.py`
with `git show` (the checkout need not match). It AST-extracts all 24 prompt strings and
independently computes a Python stdlib softmax/Platt table and five nonconstant 512-d
synthetic cases. No models, training data, recordings or upstream private outputs enter Git.
The Python source is GPL-3.0 upstream code; prompt provenance is retained in the fixture.

Canonical prompt hash encoding: each group name followed by LF, then each of its three
UTF-8 prompts followed by LF, in analyzer insertion order. SHA-256:
`59fd7be24b1b32ff31f383bfb8c8aa2d5cd8e54402e85ee01d9245d7e6e3c1ea`.
Source SHA-256: `3f8d494b7321ca9ea245cdc3208f68d6ac753c0d49f0cc79d69730a047c03b26`.

The binary64 arithmetic follows spec §8.3, including Python-compatible decimal round-to-three
(the exact fraction is rounded before division, not `round(x*1000)`). Upstream NumPy's float32
normalization includes an epsilon; synthetic binary64 parity does not establish real-model parity.

## Real-model gate — IMPLEMENTED, NOT RUN

No model download, conversion, private-asset provisioning or owner-library access was authorized
or performed. Today's native `hf_audio`/`text_model` adapters are not the music checkpoint:
the production readings gate refuses them even if a directory is named like a music model.
Real execution needs M4's native music audio/text/tokenizer and pipeline swap first. Trusted
music-text SHA-256 comes from pinned upstream `services/audio-analyzer-clap/Dockerfile:15,36`,
not from downloaded bytes: `6dbba074b27f8973d84c5c7ae9acd0b9139a6cd9dd82bea6e87954dc3b835956`.
The native text call still verifies its compiled model/tokenizer pins and uses Task 14's 15-second deadline.

After separate authorization and per-artifact rights approval:

1. Prepare already-licensed local music exports, cached tokenizer and the pinned analyzer's
   Python dependencies in a private environment. No provisioning command is supplied here.
2. Prepare a private JSON audio-list of **20 distinct real recordings** as objects with `path`
   and nonempty `license`. Use explicitly authorized files, never a scan of the owner's library.
3. Run the reference generator explicitly:

   ```sh
   python3 tools/vibe-zero-shot-real-reference.py \
     --upstream <full-history-clone> --model-dir <private-music-model-dir> \
     --audio-list <private-20-audio-list.json> --output <private-reference.json> \
     --licensed-private-use-confirmed
   ```

   It checks the two immutable model SHA-256 pins, extracts only pinned GPL Python source into
   a temporary directory, disables Hugging Face network resolution, runs the actual upstream
   analyzer's audio embedding and zero-shot methods, records each audio hash and reference vector,
   and exclusively creates a mode-0600 output. The generator is not a Rust acceptance result.
4. Run the ignored test (no SQLx setup and no database connections/writes):

   ```sh
   KIMA_CLAP_MODEL_DIR=<private-music-model-dir> \
   KIMA_ZERO_SHOT_REFERENCE=<private-reference.json> \
   KIMA_ZERO_SHOT_LICENSED_PRIVATE_USE=1 \
   cargo test -j2 -p kima-ident --test readings_real \
     real_music_readings_match_upstream_python_on_twenty_tracks -- --ignored --exact
   ```

The harness fails, rather than skips/passes, on missing input, unsupported native checkpoint,
wrong pin/source, other than 20 distinct audio hashes, unlicensed input, malformed/non-unit
reference vectors, changed audio or native inference failure. It embeds the actual 24 prompts
with the verified native text provider and compares all four readings to the actual Python
analyzer values with **absolute tolerance 1e-3**. Audio/reference reads reject final-component
symlinks, non-regular files and excess bytes. Like other private acceptance fixtures, the manifest
is trusted local evidence, not a cryptographic attestation of its generator.

Scope: readings/text-tower parity on the upstream analyzer's audio vectors. Native audio-encoder
parity is a separate M4 prerequisite, not proven by replaying those vectors. Synthetic DB tests
separately exercise stored-vector freshness, batch publication, invalidation lock order,
cancellation and mood-state integration. Task 14 real recall remains **OPEN / NOT RUN**.
