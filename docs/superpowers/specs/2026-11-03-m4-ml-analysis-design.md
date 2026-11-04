# M4 ML analysis: contract clarification and review draft

**Status: decision draft, not implementation approval.** Baseline: local `origin/main` `201454390811bcc9af32b1fc4ebbc21d129e6e86`; this does not assert that the remote was freshly fetched. Parent requirements are [the Rust design](2026-10-16-kima-rust-design.md) §§2, 3, 6, 8, 10–11, [M3 identification design](2026-10-19-m3-identification-design.md) §10 and the [roadmap](../plans/2026-10-16-kima-rs-roadmap.md). The owner has confirmed **noncommercial project use**, the retry clarification and graceful degradation below, not the other proposals in this document. This draft is ready for independent review, not Task 1 acceptance or implementation.

## Approved clarification: missing or mismatched model

If a model is missing or its SHA-256 does not match the expected value, **start the server, report the error, and disable only capabilities dependent on that model**. Other independently verifiable ML capabilities continue; do not disable all ML merely because one model is unavailable. Non-ML server functionality must remain available, and an unavailable model must not produce a false ML success. This corrects the earlier blanket “ML disabled” wording: per-capability degradation is the owner's clarification. The exact capability-to-artifact dependency graph, `DISABLE_CLAP` interaction, readiness endpoint semantics, recovery/reload, and API/UI and error-reporting format remain to be decided. The failure of a required parity fixture still fails the parity gate; degraded server startup is not a parity waiver. No implementation or startup test is claimed here.

## Approved clarification: decode per attempt

The parent §6 says **“Decode once”** without defining retry scope. The owner clarifies that this means **once per combined attempt**, not once over a track's lifetime: when analysis succeeds but embedding fails or the process restarts, retain successful analysis and retry embedding by decoding the file again. Do not persist decoded audio as a retry intermediate, and do not rerun successful analysis merely to retry embedding. A combined attempt normally shares one decode; the duration-uncertainty exception below permits an additional pass when needed. A later embedding-only attempt has its own decode. The existing upstream divergence D-009 describes sharing a decode; it must not be read as a promise of only one decode across retries.

The owner also resolves the conflict between §6's last-90-seconds wording and upstream CLAP selection: **musicnn and DSP analysis retain the last 90 seconds (or the whole shorter track), while CLAP selects upstream windows across the whole track**, not across that tail. Both normally share the decode of a combined attempt, subject to the exception below, with bounded retained audio; mono resampling and the numerical parity gate remain binding. This is a design decision, not an implementation claim.

**Integration blocker:** the existing decoder retains only a 90-second source-rate ring and reports its retained duration, not the actual full decoded duration; its 48 kHz output therefore cannot supply earlier CLAP windows. Pinned upstream obtains duration before selecting offsets and loads each window separately with `librosa.load` (offset and duration at source, then mono resampling). Duration is not necessarily known exactly from container metadata, and for an unknown or inaccurate duration the eventual whole-track percentage offsets can point to samples already discarded by a bounded one-pass collector. Guessing from metadata, treating the retained tail as the track, or accepting a mismatch would silently select wrong audio. The current offset helper alone does not solve this.

**Owner-approved exception:** A combined attempt normally decodes once, collecting the last 90 seconds for musicnn/DSP and candidate whole-track CLAP windows together. When duration is unknown or turns out inaccurate, an additional decoding pass is allowed only as needed. Count actual decoded frames through clean EOF; compare a candidate duration against that count, discard incorrectly placed candidates and retry using the actual count. Unknown duration may use the first pass for counting and analysis, followed by one pass to collect CLAP windows; inaccurate metadata likewise warrants a retry. A missing rewind/reopen capability, a changed source, a second-pass frame-count or format mismatch, decode/demux/read error, or an unresolvable duration/trim discrepancy must be an explicit error, not a partial embedding. No persistent audio intermediate; retained source windows and tail must have fixed limits independent of track length. A bounded ring does not bound decoder packet allocations or the existing byte-slice input copy.

Source grounding: pinned upstream [analyzer.py](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer-clap/analyzer.py) gets duration via `librosa.get_duration(path=...)`, computes default offsets, clamps each start to `[0, max(0, duration - 0.001)]`, takes `min(10, max(0, duration-start))`, then calls `librosa.load` separately per window. Its [requirements](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer-clap/requirements.txt) pin librosa 0.11.0; [that version's loader](https://github.com/librosa/librosa/blob/0.11.0/librosa/core/audio.py) converts offset and duration separately with `int(seconds * native_rate)` (truncation toward zero), then downmixes and resamples each window separately. A positive duration that rounds to zero frames is not the same as `None`. Its default soxr resampling fixes output length to a ceiling, whereas the existing rubato helper rounds. Different codec backends and `get_duration` metadata may disagree with observed decoded length; count-based correction is the owner's safety decision, not a claim of bitwise upstream behavior. Compare boundaries and waveforms independently, including compressed formats and encoder delay/padding, before claiming parity.

**Remaining implementation blockers:** an open handle prevents pathname substitution, but does not freeze in-place writes; metadata checks and matching frame counts alone do not prove an immutable stream. A seekable, stable input contract or enforceable snapshot/identity mechanism is required across passes, and unprotected sources must fail closed rather than silently mix versions. A decoder entry point now collects bounded source-rate windows and the tail together for a supplied expected frame count, rejecting mismatches at clean EOF. A source-factory helper retries when a supplied frame hint is absent or inaccurate and checks count, rate and SHA-256 of all decoded mono samples across passes; a correct hint needs one pass. This is not file identity or a snapshot and cannot detect a source changing within a pass or compressed audio the demuxer silently omits. It does not acquire metadata; a separate bounded per-window preparation helper now resamples and repeat-pads but is not production-wired (D-137). Production integration and an enforceable stable-input contract remain unimplemented. Verify per-container frame counts and trimming against actual decoded samples; ensure decoder options match those semantics. Exercise known, unknown and inaccurate duration over 90 seconds, short nonconstant audio, fractional boundaries, failures after valid packets, source mutation and bounded buffering. No whole-track CLAP integration is claimed.

Analysis and embedding have **independent completion**: embedding failure or a restart must not erase a valid successful analysis or falsely mark embedding successful. Disabled/unavailable CLAP cannot be reported as a successful embedding. The owner has not thereby approved a particular dependency graph, scheduling/claim mechanism, failure classification, closure/reopen policy, or all other implementation details.

### Candidate execution shape — subject to review and remaining decisions

- A bounded admitted combined attempt normally decodes once, subject to the duration exception above, producing 16 kHz analysis input and independently resampled 48 kHz CLAP windows; analysis runs before embedding so a failed analysis cannot be published as success. Release memory when the attempt ends. An embedding-only retry may re-decode after a failed embedding or restart, without repeating valid analysis.
- Do not hold database transactions or connections across decode/inference. Any design using stage claims must prevent an expired or superseded claim from publishing output; M3 §10.3 already requires a current claim token and atomic result/report for its stages. Exact ML claim acquisition, file identity validation, atomicity boundaries and buffering/lifetime still need an approved design and deterministic tests.
- Source evidence, **not an approved graph**: upstream [`unifiedEnrichment.ts`](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/backend/src/workers/unifiedEnrichment.ts) makes fingerprint and analysis independently eligible, has embedding wait for analysis output, and recording wait for fingerprint to settle. Its output-prerequisite closure/reopen behavior is not automatically established by M3's reserved `DEPENDENCY_CLOSED` value. Recording's settled prerequisite does not establish one for analysis. The upstream graph is a review candidate; approve or replace it explicitly before implementation.

## Binding functional and numerical gates

- §6 specifies symphonia decode and rubato resampling, last 90 seconds or the whole shorter track, 16 kHz mono for musicnn/DSP and 48 kHz mono for CLAP. The owner's clarification above restricts the tail rule to musicnn/DSP and requires whole-track CLAP windows. Shared startup ORT sessions and bounded decode-to-inference channels are required; model hashes must be verified. CPU is required. §6 also describes optional GPU, while the roadmap places GPU in M8; reconcile scope rather than claim M4 GPU delivery.
- CLAP audio uses FFT 1024, hop 480, 64 Slaney bands 50–14000 Hz. Pinned upstream selects up to five 10-second windows based on the **whole-track** duration: short tracks have fewer windows, and some longer tracks tile rather than use nominal percentage offsets. The offset-only helper is not yet wired into decoding; the existing last-90-seconds ring cannot by itself reproduce whole-track selection for long tracks. Repeat-padding, sample boundary conversion, mean then L2 normalization into 512 dimensions, and the interaction with shared decode still require integration and parity verification. Text uses RoBERTa BPE and Xenova text ONNX with upstream pad semantics. Validate actual tensor contracts and tokenizer behavior before fixing implementation details. Postgres stores CLAP `vector(512)` with HNSW cosine per parent §3.
- musicnn uses 512-frame/256-hop, 96 mel bands, 187-frame patches at stride 93 with final partial patch dropped, 200-dimensional patch embeddings and the ten named heads in §6.
- **CLAP and musicnn cosine ≥ 0.999 against independent Python/upstream real-model references remains binding.** Define and report each comparison case and its denominator; missing assets must fail the gate, not silently skip it. Upstream's weaker CLAP audio ≥0.99 comparison or ONNX-derived regression is not a substitute. No inference or parity gate has been run for this draft.
- DSP requires integrated LUFS/LRA, chroma/Krumhansl key and onset/autocorrelation tempo; key accuracy ≥70% with relative major/minor accepted, tempo ≥85% within ±4% or double/half. If tempo fails, §6 requires aubio FFI and another run. The corpus, reference labels and denominator must be frozen before evaluation, not chosen after seeing scores. D-008 records the planned Essentia divergence.

## Asset provenance and rights boundary

Upstream source references in this section are pinned to [`12faf1cd34ff53054221818b0ccde58b8e9b0020`](https://github.com/Chevron7Locked/kima-hub/tree/12faf1cd34ff53054221818b0ccde58b8e9b0020). Evidence is source/metadata inspection, not a legal opinion or model execution.

- [`musicnn-answers.npz`](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/models/musicnn-answers.npz) was independently fetched in memory and hashed as `05107f43fed00e092be65173dafa0df1cc075c95f6efac1eb311a0529167d329`, matching the upstream manifest. Header inspection found `musicnn` `(19, 200)` and ten `(19, 2)` head arrays: 19 patches of **one** synthetic signal, not 19 independent tracks. No bytes are included here. Upstream [`convert_musicnn.py`](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/convert_musicnn.py) tests heads on reference embeddings; this is not an end-to-end head test. Verify all model bytes, signal reproduction and independently sourced references before accepting parity.
- Upstream's CLAP audio export is based on LAION revision `8fa0f1c6d0433df6e97c127f64b2a1d6c0dcda8a`; the exported ONNX hash in its Dockerfile is an upstream assertion, not independently reproduced here. Xenova text revision `c28f2883575e590e04d3146ff0713c2448d691ba` has [remote LFS metadata](https://huggingface.co/api/models/Xenova/clap-htsat-unfused/revision/c28f2883575e590e04d3146ff0713c2448d691ba?blobs=true) for `onnx/text_model.onnx` SHA-256 `6df7c51f2d78e236a03f2b816e613d538c815639d13644fe7c2124f439da9648`; metadata is not an independent byte hash. Pin and verify all exact artifacts and generation dependencies before use.
- The pinned [UPF notice](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/pb/LICENSE-upf-models) names and includes CC BY-NC-ND 4.0, but its introduction links to BY-NC-SA. Seek rightsholder or qualified legal clarification of that conflict and the intended conversions, outputs, access and sharing. Owner confirmation of noncommercial use does **not** establish permission to distribute converted weights or fixtures. Do not assume every output is an adaptation or that private conversion is necessarily prohibited. LAION and RoBERTa model cards declare Apache-2.0 and MIT respectively; this does not establish rights to every exact derivative, fixture or Xenova file. Hashes and the repository license grant no additional model rights.
- Until exact use and provisioning are cleared, do not copy model or NPZ bytes into this repository, bundle or redistribute them, publish them in CI/images, or provision them into an unapproved service. Determine lawful offline CI access and reproducibility with the owner and rightsholder/legal advice. No legal clearance, real-model parity or DSP acceptance is claimed.

### Manifest trust and provisioning — proposal, not an approved contract

- Binding §6.4 says models are fetched **or** verified by a SHA-256 manifest; it does not specify a manifest format, trust anchor, distribution method or complete asset set. The pinned upstream [musicnn manifest](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/models/MANIFEST.sha256) lists the body, ten heads and a reference NPZ; upstream's [audio-analyzer Dockerfile](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/Dockerfile) copies those assets and runs a build-time numerical gate. The NPZ is a reference, not automatically a runtime model. Upstream's [CLAP Dockerfile](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer-clap/Dockerfile) instead pins SHA-256 values in build instructions for exported audio, downloaded text, tokenizer and preprocessing files; it fetches the text graph from a mutable `main` URL but checks its declared hash. These are upstream assertions and provisioning choices, not independently verified model identities or rights grants.
- The following are **proposed technical defaults**, not owner-approved decisions. They replace the previous list of generic manifest decision gates. They do not clear rights or certify any model.

#### Authority and artifact inventory

Use a reviewed `kima-ml` source-controlled Rust table, compiled into the release binary: `ArtifactId`, fixed single-component ASCII basename and 32-byte SHA-256 for every runtime input. The executable/release is the trust anchor, independent of the operator-writable model directory. No runtime manifest parsing, writable sidecar authority, automatic model download, signatures or custom updater. A reviewed application release updates the table; old binaries retain their old pins. CI tests table uniqueness of IDs and basenames, complete capability coverage, and digest encoding. Never discover the required set by listing the directory; unrelated extra files are ignored and never opened. Fixed names contain no separators and are neither `.` nor `..`; no input controls paths. For each ID open only its exact basename under the configured directory, once. Operators provision files lawfully; deployment supplies the directory and protects the executable, library/runtime and configuration as trusted. The model directory itself can be writable by an adversary: SHA equality and retained bytes protect against substitution, not denial of service. Hashes do not prove publisher integrity, provenance or rights.

Candidate identities, **upstream assertions rather than independently verified model bytes**:

| Capability | Required runtime inputs and expected identity |
|---|---|
| musicnn analysis | Body `msd-musicnn-1.onnx` plus all ten heads named in §6: exactly the corresponding eleven file/hash pairs in the pinned [MANIFEST.sha256](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/models/MANIFEST.sha256). Treat the linked immutable text as the inventory for filling the reviewed release table, not as a runtime download. |
| CLAP audio embedding | `hf_audio.onnx` SHA-256 `37df17afb9881569c4f50e5b691167aa54586418059701a74a54934514969972`, asserted by the pinned CLAP Dockerfile. Implement mel parameters in reviewed code; if implementation instead consumes external preprocessing data, add and verify that exact input before enabling this capability. |
| CLAP text embedding | `text_model.onnx` SHA-256 `6df7c51f2d78e236a03f2b816e613d538c815639d13644fe7c2124f439da9648`; `tokenizer.json` SHA-256 `847bbeab6174d66a88898f729d52fa8d355fafe1bea101cf960dd404581df70e`; pins from the same Dockerfile. Construct the tokenizer solely from verified JSON bytes, with any padding/truncation semantics in reviewed code and independently tested. If further tokenizer inputs are actually read, pin them too, or do not enable text. |
| DSP | No model artifacts; depends on decode/DSP implementation, not musicnn or CLAP files. |

`musicnn-answers.npz` is separately pinned in upstream's manifest for parity testing, **not** a runtime dependency; do not require it for startup. The upstream Dockerfile has additional CLAP preprocessing and tokenizer files because its Python consumer uses them; the proposed Rust consumer must not silently read those or any ONNX external-data sidecars. Validate the JSON-only tokenizer and no-sidecar ONNX assumption against the exact legally obtained artifacts; if false, explicitly add each consumed file to the table with an independently anchored hash and byte-bound loading, or leave the dependent capability unavailable. Do not substitute other files or derived artifacts under these hashes.

#### Verifier and consumer boundary

Implementation skeleton (illustrative API, not shipped code):

```rust
// ArtifactId and the table are private to the trusted crate, not user supplied.
struct VerifiedBytes { id: ArtifactId, bytes: Box<[u8]> }
fn verify(root: &Path, id: ArtifactId) -> Result<VerifiedBytes, ArtifactError>;
fn load_onnx(model: VerifiedBytes) -> Result<ort::session::Session, LoadError>;
fn load_tokenizer(data: VerifiedBytes) -> Result<Tokenizer, LoadError>;
```

Open the fixed file with nonblocking and final-component no-follow flags where supported, then inspect the opened handle and reject anything but a regular file before reading. Do not use a path metadata check followed by a potentially blocking open: a FIFO can be substituted between them. If the platform cannot provide an equivalent safe open and handle inspection, fail closed for the dependent capability. Read a bounded owned buffer, hashing those exact bytes with workspace `sha2`; compare with the embedded digest before constructing `VerifiedBytes`. Limit each read to a reviewed per-artifact maximum plus one byte, so an oversized or growing file is an error rather than unbounded allocation; record measured limits after exact artifact inspection. Restrict the configured root to a trusted operator-controlled directory. Nonblocking open prevents the FIFO open stall, but does not guarantee bounded filesystem operations: network filesystems and other storage can still stall; deployment controls must account for this limitation. Do not claim an open handle freezes contents: concurrent in-place writes can alter a read, but the *buffer actually passed to the consumer* must hash to the expected value. Never reopen by pathname after hashing. Errors include ID and sanitized category (missing, unreadable, not regular, too large, hash mismatch, unsupported external data, parse/session failure), not arbitrary file contents.

Pass `&model.bytes` to `Session::builder()?.commit_from_memory(...)`, never `commit_from_file`; parse tokenizer from the verified byte slice, never `from_file` or hub lookup. Do not permit ONNX external initializers or other external resources unless the loader demonstrably binds every such resource to separately verified bytes; lack of a proven check disables that model rather than falling back to path loading. Keep peak startup memory bounded, load one graph at a time, drop buffers only after the memory-commit API permits it, and share resulting sessions. Protect against model resource exhaustion with limits and deployment controls; this is not a sandbox for a malicious but trusted-pinned model.

Workspace evidence: edition 2024, Rust `1.85`, `sha2 = 0.11`; no `kima-ml`, `ort` or `tokenizers` dependency exists in this worktree/lockfile. As a concrete candidate, [`ort` 2.0.0-rc.10](https://docs.rs/crate/ort/2.0.0-rc.10/source/Cargo.toml) declares Rust 1.81 and ONNX Runtime 1.22; its [builder](https://docs.rs/ort/2.0.0-rc.10/ort/session/builder/struct.SessionBuilder.html) exposes `commit_from_memory`. Pin the exact prerelease when implementing; disable default `download-binaries` and do not enable `fetch-models`, supply a compatible trusted ORT library separately. Crate metadata alone does not establish workspace MSRV/build compatibility, platform packaging, memory limits, external-data rejection or real-model loading: prove these in implementation. `load-dynamic` requires compatible library deployment and makes that library part of the trusted computing base. No ORT build or model execution has been done here.

#### Availability and recovery defaults

Each row above is independently initialized. All eleven musicnn models are required for the complete analysis result; one missing head makes that result unavailable, while DSP may still be computed as a separate internal capability and must not falsely complete the combined analysis stage. CLAP audio and text initialize separately: bad text or tokenizer does not disable audio embedding; bad audio does not disable text. `DISABLE_CLAP` skips both without treating either as a model error. The stage scheduling prerequisite between analysis and embedding remains a separate graph issue: an available encoder alone does not override stage eligibility.

Missing, mismatched, oversized, unreadable or unparseable inputs and session initialization errors yield unavailable only for their dependent capability; collect all results instead of returning early. Start the non-ML server and report capability, artifact ID and category in startup logs and a machine-readable capability status; ordinary server readiness stays ready once non-ML services initialize, while capability status distinguishes `available`, `disabled`, and `unavailable`. Do not mark unattempted work successful; report unavailable work as blocked/retryable without burning repeated automatic retries while the capability remains unavailable. Restart after provisioning to reload and reverify; no live reload in M4. On embedding retry after analysis succeeded, retain analysis and re-decode audio for that new attempt, as above.

**First vertical slice:** with a locally generated, redistributable tiny ONNX fixture and actual ORT consumer, TDD one available and one corrupted-model path through startup status and the real memory session; assert that replacing or rewriting the path after verification cannot change the buffer consumed. Add independent capability/multiple-failure and restart-recovery tests, tokenizer-byte loading, malformed/oversized/nonregular inputs and external-data refusal. On platforms supporting FIFOs, place a FIFO at an artifact basename with no writer and use a bounded test deadline to assert startup proceeds, the dependent capability is unavailable, and independent capabilities remain usable; also test final-component symlink refusal. Exercise server startup without an external DB only where the existing server architecture supports it; otherwise an authorized isolated DB is required. This worktree has no ML consumer, ORT dependency or real loading path, so a standalone verifier here would not establish the key security property and is intentionally not delivered. Model rights and lawful exact-byte access still block real-model parity and release certification.

## Implemented database slice (2026-11-03)

This implementation record supersedes the draft's historical no-code/no-migration
statements for the database scope below only; outstanding design decisions remain open.

The owner ordered PostgreSQL/pgvector persistence after the verified ONNX slice.
Migration `0024_ml_analysis.sql` and `kima_db::ml` implement **CLAP embedding
storage only**, using the already implemented `ClapEmbedding` contract: 512 finite
floats and `|L2 norm - 1| <= 1e-3`, without renormalization. Postgres' vector type
rejects nonfinite values and wrong dimensions; a CHECK enforces the norm. The
HNSW index uses `vector_cosine_ops`. No new model assets or pgvector Rust dependency
are needed: compile-checked SQL casts PostgreSQL `real[]` to/from `vector(512)`.

Pragmatic storage defaults for this slice (not approval of the remaining graph):

- One current embedding per track, cascading on deletion. Each row records the
  encoder artifact ID, its exact 32-byte SHA-256, a caller-supplied nonempty
  preprocessing/aggregation pipeline version, database file generation, and
  publication time. Provenance is a caller assertion; this layer neither hashes
  model bytes nor chooses an active model. Reads require all provenance fields,
  a present matching generation, and a `done` embedding stage. Different model or
  pipeline versions are not silently mixed. A later reanalysis replaces the
  working stage token before inference and atomically replaces the old row on success;
  automatic model-rollout scheduling and history retention are not implemented.
- `embedding_claim` snapshots an **already working** embedding stage and file
  generation before audio access; it neither acquires work nor establishes
  eligibility. Publication owns a short transaction, locks track then stage,
  checks the generation, presence, exact `started_at` token and unexpired lease,
  and commits output and success together. It checks the database wall clock
  **after** acquiring the stage lock, including when lock waits cross expiry.
  Stale/duplicate claims return false without writes; invalid vectors/provenance
  roll back stage success. No connection is held across decode/inference.
- Database triggers conservatively invalidate embeddings and reopen any existing
  analysis/embedding/similarity rows on a change to path, size, mtime, cheap file
  fingerprint, missing/restored status, or explicit generation bump. This covers
  scanner, repoint/restore, organizer and autotag writers without duplicating
  their mutation paths. The scanner's explicit changed-file reopen also bumps
  generation even with unchanged stored metadata. Renames may require reanalysis;
  unrelated title/album metadata changes do not. A scan may advance the generation
  twice (metadata update plus explicit reopen); it is an opaque monotonic fence,
  not a count of file edits. Missing/restored files do not resurrect old output.
- This is **database identity**, not an immutable filesystem snapshot. The existing
  scanner fingerprint hashes size plus head/tail only, not the entire encoded
  audio. Integration must still enforce the stable-input contract described above;
  this slice cannot detect an unobserved in-place file mutation by itself.

Final musicnn/DSP output and head aggregation types do not exist in the current
computation API. Inventing flattened mood/DSP columns or accepting arbitrary JSON
would freeze an unapproved contract. Their result schema/persistence is therefore
explicitly deferred until those types and transformations are established.
Analysis data is not created or claimed successful here. Scheduling, production
inference wiring, server readiness/progress, UI, M5 similarity, and M12 transport
remain outside this slice. Independent completion is preserved; no dependency
graph is inferred. See D-142 for the storage/invalidation divergence.

## Bounded attempt-orchestration slice (2026-11-03, uncommitted review candidate)

The owner authorized continuing with synthetic models. `kima_ml::attempt` now
implements the **per-attempt execution boundary only**, not registration in the
M3 enrichment coordinator or a production scheduler. This narrows the next step
to interfaces that actually exist: `CapabilityEvaluationReport`, the bounded
Symphonia/rubato tail decoder, `decode_clap_auto`'s native-rate whole-track
windows, and caller-owned computation/publication callbacks returning `Result<(), E>`.
No musicnn/DSP result type, column or arbitrary JSON result was introduced.

- `Combined` requires caller admission for the two stages. It requires both
  musicnn and DSP capabilities for a complete analysis callback. Missing or
  disabled CLAP does not prevent analysis: only the tail decode is needed then.
  Analysis callback success precedes embedding callback execution; a failed or
  unavailable analysis blocks this combined attempt's embedding callback.
  This is a **conservative local execution precondition**, not approval of the
  unresolved global stage graph, speculative embedding or terminal closure.
- `EmbeddingAfterAnalysis` requires the caller to have read valid, durably
  completed analysis for the same current file generation. It never calls
  analysis again and does not require analysis models to remain available.
  It opens/decodes a new source for embedding, including after a caller restart.
  If CLAP is unavailable/disabled it opens nothing and reports blocked, not success.
- Accurate frame hints share one decode; absent, zero or inaccurate hints use
  the existing duration-discovery/verified-reopen exception. PCM and attempt
  state are not persisted. Source factories must return stable, fresh inputs;
  decoded fingerprints are not an encoded-file snapshot. Bad decode or changed
  decoded content prevents either callback from running.
- Analysis receives the 16 kHz tail. Embedding receives native-rate selected
  windows and rate, not the old 48 kHz tail. The embedding callback still owns
  per-window preparation, inference, aggregation and publication. No real-model
  tensor contract is assumed by this runner. A successful callback acknowledges
  its own atomic fenced publication, **not database verification by this crate**.
  Storage rejection (`complete_embedding` returning false) must become callback
  failure. There must be no transaction/connection spanning decode or both callbacks.
- Outcomes distinguish acknowledged publication, caller-asserted prior completion,
  callback failure, capability blockage (preserving reasons) and unsatisfied
  analysis prerequisite. No stage rows or capability statuses are rewritten here;
  no automatic retry/lease policy or global capability disable was added.

Tests use original synthetic WAV samples and the existing verified tiny identity
ONNX model through actual ORT. They establish call order, decode counts, retry
isolation, missing/disabled capabilities, fail-closed decode and verified reopen,
**not analysis semantics, restart durability in Postgres, real-model parity or
server startup integration**. Existing database fence tests were also rerun with
the authorized isolated `127.0.0.1:55474/kima_m4_slice` environment, after checking
`DATABASE_URL`, `PGDATABASE` and `current_database()` before mutation. These
existing `#[sqlx::test]` tests use SQLx-managed per-test databases on that isolated
server, not the named base database itself; this is a deviation from a literal
existing-database-only boundary and must be reviewed before further DB execution.
No development database or model assets were targeted. Post-run readback still
shows SQLx test databases; their ownership was not established, so none were deleted.

Remaining integration gates: approved scheduler graph and claims, asynchronous
publication adapters, final musicnn/DSP types and atomic storage, enforceable
stable encoded input, cancellation/admission, server startup/readiness and real
model tensor/parity validation. The synchronous execution boundary is deliberately
not registered with the async job runtime until those adapters and safeguards
exist. Private asset acquisition is a separate parent-controlled task and does
not change this slice's synthetic-only acceptance or establish parity.

## Async fenced CLAP publication adapter (2026-11-03, uncommitted review candidate)

The owner authorized continuing the M4 production-integration slice after the
attempt-orchestration commit. This adds `kima_ml::publish`, feature-gated
behind `pg-publish` (off by default, so the decode/inference core stays
DB-free), which is the **asynchronous publication adapter** the prior slice's
"Remaining integration gates" named and explicitly left undone. It connects
`attempt::run_attempt`'s existing synchronous `embed_and_publish` callback
boundary to the existing `kima_db::ml` claim/publication layer, and nothing
further:

- `publish_clap_embedding(pool, claim, provenance, embedding: &ClapEmbedding)`
  is a thin async wrapper over `kima_db::ml::complete_embedding`. It maps a
  storage rejection (`Ok(false)`: stale claim, missing track/generation, or an
  expired lease) to `PublishError::Rejected`, matching the prior slice's
  explicit requirement that rejected storage claims become callback failure,
  never a quiet `Ok(())`. It takes the already-typed `ClapEmbedding` (finite,
  unit-norm, 512-dim per the existing storage contract), not a raw slice, so
  an unvalidated vector cannot reach this call.
- `publish_clap_embedding_blocking(handle, ...)` bridges this async call into
  `run_attempt`'s plain-sync callback signature. It follows this workspace's
  existing convention of running sync work from async call sites via
  `tokio::task::spawn_blocking` (see `kima-library`/`kima-ident`), not the
  reverse: it must be called from inside a `spawn_blocking` closure, where
  blocking that dedicated thread on the runtime handle is safe and does not
  starve the worker pool. It is not safe to call directly on a worker thread.
- No scheduler/claim-acquisition logic, production worker registration, or
  musicnn/DSP result type was added or implied. The caller still owns
  eligibility, the claim, and calling this only after decode/inference finish
  outside any transaction.

Tests (`cargo test -p kima-ml --features pg-publish --lib publish::`, strict
red/green): a pure `classify(accepted: bool)` unit test for the
success/rejection mapping, and a bridging-pattern test that runs a real async
future to completion from inside `spawn_blocking` on a multi-thread Tokio
runtime (no database) to validate the exact mechanism
`publish_clap_embedding_blocking` depends on. `cargo clippy -p kima-ml
--features pg-publish --all-targets -- -D warnings` is clean; the default
(no-feature) build and `cargo build --workspace` are unaffected; the one
pre-existing `kima-ml` test failure (`onnx::tests::panic_wakes_an_observed_condvar_waiter`,
missing `ORT_DYLIB_PATH` in this environment) reproduces identically on the
unmodified `219fc0f` baseline and is unrelated to this change.

**Deliberately not tested here, and not claimed:** an end-to-end test of
`publish_clap_embedding` against a live `kima_db::ml::complete_embedding` call
needs a real Postgres connection. The prior slice already flagged that its
`#[sqlx::test]` usage against the isolated `127.0.0.1:55474` server creates
per-test databases beyond a literal existing-database-only authorization, and
that this "must be reviewed before further DB execution." This slice does not
expand that permission or run any further DB-mutating test; the live-DB path
through this new adapter is an explicit, documented verification blocker for
the next DB-authorized step, not a claim of integration success. No model
asset was read, converted, or run; no production scheduler/server wiring is
claimed; no git commit was made.

## Live-Postgres publication verification (DB-authorized follow-up)

The owner explicitly authorized SQLx test-database creation/drop exclusively
on isolated `127.0.0.1:55474`; this supersedes the preceding slice's live-DB
authorization blocker, not its remaining production-integration gates.
`crates/kima-ml/tests/pg_publish.rs` adds four `pg-publish`-gated tests using
`kima_db::MIGRATOR`, real `embedding_claim`/`embedding` APIs and synthetic
validated 512-dimensional vectors:

- Async publication persists the exact vector, generation, model ID/hash and
  pipeline version; stage success, lease clearing, finish timestamp and
  source/evidence are read back. A mismatched pipeline cannot read the result.
- The real blocking adapter publishes successfully from `spawn_blocking`.
- A superseded stage token returns `PublishError::Rejected`, writes no vector,
  preserves the replacement claim, and permits that replacement to publish.
- An expired lease returns `PublishError::Rejected` through the blocking bridge,
  with no vector or false stage success.

Because the adapter already exists, RED here is explicitly mutation-based
regression verification, not a claim of implementing a missing feature:
a temporary false-ack/no-write adapter failed all four assertions; separately,
calling real storage but swallowing its rejection failed exactly the two
rejection tests while both successful-publication tests passed. Restoring the
original adapter gave 4/4 GREEN. The initial fixture setup attempt omitted
explicit UUIDs and failed before the assertions; this was corrected and is
not counted as behavioral RED. No production Rust change remains.

Database scope proof: before execution both `DATABASE_URL` and `PGDATABASE`
were explicitly set to `kima_m4_slice` on `127.0.0.1:55474` and checked without
printing credentials. Live TCP readback was
`kima_m4_slice|172.17.0.2|5432`; Docker container `kima-m4-db-slice`
(`a85c2dd80aa3`) mapped only `127.0.0.1:55474 -> 5432`. All 19 initial database
names were inventoried read-only. SQLx 0.8.6 derives deterministic SHA-512
names and drops its target before setup: expected names were computed and
checked for collisions with unexplained existing databases before executing;
only absent/run-owned names were permitted. SQLx metadata uses the isolated
base database; fixture/schema/publication mutations occur in per-test DBs.
Final inventory exactly matched the initial 19; no run-owned DB remained.
No dev `5456` connection/mutation, replacement container, volume or broad
cleanup was used. A process/CWD scan found no competing worktree writer.

Verification (all Cargo commands `--offline --locked`):

- `cargo test -p kima-ml --features pg-publish --no-fail-fast`: one complete
  successful run, **123 unit/integration tests plus 1 doctest**; listing counts
  reconciled with execution. This includes all four new DB tests and the
  formerly failing `onnx::tests::panic_wakes_an_observed_condvar_waiter`.
- `cargo test -p kima-db --test ml`: **12 passed**, including lease-lock races,
  file/generation fences, atomic publication and migration upgrade.
- `cargo clippy -p kima-ml -p kima-db --features kima-ml/pg-publish --all-targets
  -- -D warnings`, `cargo fmt --all --check`, and the default/no-feature
  `cargo check -p kima-ml --no-default-features`: passed.
- Every runner invocation verifies `ORT_DYLIB_PATH` from a no-follow,
  nonblocking regular-file descriptor against the repository-pinned SHA-256
  `7520bc1b4f649ee8fa956442a66ded303e8fd3b465b0159b7e893bdc87df4e38`.
  Runtime: `/home/dimmu/.hermes/cache/scratch/m4-ort-trusted/libonnxruntime.so.1.22.0`.
  No model asset was used.

**Unresolved verification blocker:** the first complete ML attempt stopped at
three `clap_prepare` errors (`synthetic_waveforms_match_pinned_librosa_at_multiple_rates`,
`short_upsample_matches_librosa_soxr_hq`,
`concurrent_resampling_matches_independent_serial_results`), each reporting
`CorruptAudio("non-finite or excessive audio sample")`. A subsequent full run
passed, but repeating the unchanged feature-enabled `clap_prepare` binary ten
times produced nine 9/9 passes and one **SIGSEGV**. Twenty default/no-feature
repetitions passed; this does not establish the cause or dismiss the crash.
All production ML/DB sources are identical to `1dd7cf3`; the native/resampling
instability needs separate diagnosis and independent review before claiming a
reliable ML gate. It was not hidden with ignored tests or a runtime waiver.

Local evidence is retained under
`/home/dimmu/.hermes/cache/scratch/m4-publish-evidence/` (RED/GREEN logs,
initial/per-command inventories, run-owned database allowlist, full-suite
listing and stress logs); the credential-private scope-checking runner is
`/home/dimmu/.hermes/cache/scratch/m4-publish-db-run.py`.
These tests establish the adapter-to-Postgres boundary only, not a combined
real-model/attempt-to-server pipeline, scheduler registration, or model parity.
No commit/push/merge was performed; independent review remains required.

### Native-resampling investigation (blocker remains open)

At unchanged production HEAD `1dd7cf39dbf6bdb4160aee2667ea996205ae8931`,
process/CWD discovery found no competing worktree writer. The reviewed
`pg_publish.rs` and publication documentation were preserved. No database
operation, real-model asset download/use, system-library replacement, commit or push was
performed.

A new `crates/kima-ml/tests/clap_cold_start.rs` starts up to eight fresh child
processes, each with a ten-second deadline and 32 workers synchronized before
**any** resampler call. Every output sample is checked against the existing
independent librosa fixture with the unchanged 3e-6 tolerance. This avoids the
old concurrency test's serial-reference warm-up. Uninstrumented runs reproduce
incorrect **finite** output, e.g. first sample `-0.0048213415` or `1.1186757`
instead of `0.89751744`. Existing finite/magnitude validation cannot detect this
kind of corruption. Both default and `pg-publish` builds fail this regression;
the feature distinction was not causal evidence.

Concrete native defect: installed `libsoxr 0.1.3-5.1` reaches concurrent
`omp_init_lock` calls for the **same lock addresses** during independent
`soxr_oneshot` calls. A diagnostic interposer records this even without its
optional delay. The public C header matches the Rust quality structure and
function declarations; each call owns separate slices, counters and quality
state. The corresponding libsoxr 0.1.3 `src/fft4g_cache.h` initialization checks
`FFT_LEN` without synchronization, initializes five shared OpenMP locks, then
publishes `FFT_LEN = 0`. Multiple first callers can initialize/reset the same
locks while others start using the cache. This is a demonstrated native
first-use race, not an ORT attribution: both preserved feature/default
`clap_prepare` executables link libsoxr/libgomp and do not directly link ORT.

Bounded debugger evidence: 12 original-binary and 40 cold-regression GDB runs
did not reproduce SIGSEGV. A separate supervised run with a diagnostic 1 ms
preemption at `omp_init_lock` captured all-thread stacks and a core at an
**intentional SIGTRAP on duplicate initialization**, through libsoxr to
`clap::soxr_resample`; it is not a stack of the earlier SIGSEGV. A standalone
C-header-based 32-thread reproducer also timed out once in 100 uninstrumented
system-library processes. These establish unsafe native initialization and
reproducible wrong output/hangs, but the exact earlier SIGSEGV instruction is
still unknown.

A scratch-only native experiment protects initialization with an OpenMP
critical section and a separate initialized flag (not a whole-resampling
mutex). With matched `WITH_OPENMP=ON, WITH_PFFFT=OFF` source builds and the
diagnostic preemption, vanilla has duplicate initialization in 8/8 runs and
seven three-second timeouts; the patched experiment has neither in 8/8. The
patched FFT4G build also passes 20/20 fresh-process regression invocations for
each Rust feature mode, without the diagnostic interposer. This is only a
private candidate-native-fix check, not a deployed or independently reviewed fix.
The PFFFT-enabled vanilla build also passes these probes, so build configuration
matters and a passing alternate library alone does not prove a fix. No native
patch was installed or wired into repository packaging. A supported native
source/dependency fix, packaging verification and independent review are still
required; no application serialization, warm-up waiver, ignored test or relaxed
numeric check was introduced.

Verification on the installed system library, all Cargo commands offline/locked:

- `cargo fmt --all --check` and `cargo clippy -p kima-ml --features pg-publish
  --all-targets -- -D warnings`: pass.
- All ML unit tests, non-DB integration targets and doctests executed separately
  for default and feature builds. Default: 117 passed including the doctest,
  one failed (new cold-start regression). Feature: 121 passed including the
  doctest in that sweep. `pg_publish` is deliberately excluded: no DB operations
  were needed or performed, so this is not a claim of full DB-feature coverage.
- Existing `clap_prepare`: 20/20 default and 20/20 feature repetitions pass.
  New cold-start regression: 7/10 default and 8/10 feature repetitions pass;
  remaining repetitions fail on waveform mismatch. Passing old stress gates
  therefore do not clear the blocker.
- ORT path `/home/dimmu/.hermes/cache/scratch/m4-ort-trusted/libonnxruntime.so.1.22.0`
  was opened no-follow/nonblocking, required to be regular and bounded, and
  SHA-256 verified as
  `7520bc1b4f649ee8fa956442a66ded303e8fd3b465b0159b7e893bdc87df4e38`
  before the verification runner. Only synthetic test ONNX was exercised.

Private evidence: `/home/dimmu/.hermes/cache/scratch/m4-native-resampling/`:
`verification-results.json`, `test-counts.json`, failed `*-cold-stress-*.log`,
`cold-000.log`, preserved executables/linkage/hash manifest, `trace-000.log`,
`duplicate-init-gdb.log`, `duplicate-init.core`, standalone `native-cold.c`,
`native-init-experiment.patch`, and matched native build/probe logs. The core
and native sources are diagnostic private artifacts, not distributable runtime
or model assets. Production Rust sources remain unchanged; Important remains
open pending a deployable native fix and review.

### Pinned private dynamic libsoxr integration (review pending)

The owner authorized a project-local CI/Docker dependency replacement, not a
host system-library install. Initial process and CWD discovery found no competing
writer; HEAD remains `1dd7cf39dbf6bdb4160aee2667ea996205ae8931`. Existing
`pg_publish.rs` and its documentation were preserved. No commit/push/merge,
private model access/download/packaging, development DB mutation or broad
cleanup was performed.

The read-only review in
`~/.hermes/cache/delegation/live/deleg_37403aec/task-0.log` supports the
initialization-only fix for the inspected FFT4G/OpenMP lifetime, not a universal
configuration claim. `native/soxr/fft-cache-init.patch` now carries that patch;
`scripts/build-soxr.sh` downloads pinned libsoxr 0.1.3, checks SHA-256, patches
without fuzz, builds a shared Release library with GCC/OpenMP ON, PFFFT/AVFFT OFF
and NDEBUG, requires a libgomp dependency, and installs only into a new explicit
private prefix. No application mutex, warm-up waiver or tolerance change.
The cold regression additionally asserts exactly 480000 output samples before
checking every sample. CI selects the private prefix via `GITHUB_ENV`, exercises
cold/waveform targets in both feature modes, and checks actual `ldd` resolution.
Docker builds on bookworm for its bookworm runtime, deliberately selecting
`/opt/kima/soxr/lib`; runtime libgomp is explicit. The LGPL-2.1-or-later notice,
full license, authors, original source archive, patch, recipe and CMake options
travel with the modified library, with replacement/rebuild instructions in
`native/soxr/README.md`. These native-library rights do not license model assets.

Provenance was independently checked against Debian's HTTPS-published
`libsoxr_0.1.3-4.dsc` (saved under `native/soxr/`), not inferred from the scratch
archive. A new HTTPS download and the saved archive both match its published
SHA-256 `b111c15fdc8c029989330ff559184198c161100a59312f5dc19ddeb9b5a15889`
(94384 bytes). This authenticates the Debian HTTPS publisher/checksum boundary;
no independent OpenPGP-signature or upstream release-signing claim is made.

Scope caveats remain binding: OpenMP-off builds lack the critical section;
assert-enabled builds still read FFT_LEN in a pre-reader-lock assertion. Cache
clear requires complete caller quiescence at process exit, and once-only state
is not reset: no reuse after clear, concurrent unload, or live unload/reload is
supported. Prior wrong-waveform/hang evidence remains valid; the exact earlier
SIGSEGV instruction is still unknown. General native safety, real-model parity,
production ML/server wiring, or M4 completion is not established.

Actual verification and retained failures (private evidence directory
`/home/dimmu/.hermes/cache/scratch/m4-soxr-packaging/`):

- One uninstrumented system-library cold run passed; a single deliberately
  preempted run with duplicate-lock trap failed with **intentional SIGTRAP**
  (`red-delayed-rust.log`), not a new SIGSEGV. The same diagnostic schedule with
  the recipe-built private library passed, without duplicate reports
  (`green-delayed-rust.log`). Historical uninstrumented failures are retained
  under `m4-native-resampling/`; no retry-until-green reliability claim.
- Private host recipe: first attempt failed because CMake 4 removed old policy
  compatibility. Adding explicit policy minimum 3.5 fixed configuration. Native
  GCC warnings remain upstream warnings; they were not hidden or fixed in this
  narrow patch. Both Rust feature modes passed cold + nine waveform tests.
  Full default ML: **118 passed including one doctest**; full `pg-publish` ML:
  **125 passed including one doctest and four actual migrated-Postgres adapter
  tests**, using the final-image library extracted into a private prefix for
  the latter. ORT uses only generated synthetic ONNX graphs.
- `cargo fmt --all --check`, default full-workspace/all-target Clippy with
  `-D warnings`, feature ML/all-target Clippy and `git diff --check`: passed.
- Actual full Docker build initially failed because slim GCC lacked libc6-dev;
  the Dockerfile now explicitly includes it. Corrected full build passed,
  including the unchanged frontend build and release server. Final image
  `kima-m4-soxr:review` is
  `sha256:ea4c063537fbac5bc79b23580bd0759904d5c24fb4f60ba0c2002e91bfb261b0`.
  Packaged libsoxr SHA-256 is
  `6444cf3a36d808a729ab097e7a4c92233fc8c3e3b0ff66098659c0c68f2f7e6d`.
- Matching backend-stage test compilation used a fresh target and offline/locked
  Cargo. First attempt lacked cached `extended 0.1.0`; a read-only host Cargo
  registry mount supplied already cached Rust sources, without network/model
  access. Both feature probe builds then passed. Read-only-mounted probes in
  the **unmodified actual final image** passed both cold tests (eight fresh
  processes each), both nine-test waveform suites, seven ORT session/inference
  tests and two runtime-policy/error tests (unset, relative, absent and corrupt
  runtime paths). Runs were non-root, read-only, network-disabled, capability-
  dropped and CPU/memory/PID-bounded, with tmpfs /tmp and /data, no dev volumes.
  `ldd` resolves libsoxr to `/opt/kima/soxr/lib` and libgomp to the runtime package.
  The server does not yet call the ML crate; mounted tests prove image native
  loading/synthetic inference, not production server inference.
- Corrected historical workspace audit: discovery lists **1930 tests across
  141 package-qualified target/doc sections**. The timed-out log currently has
  **759 observed passing tests and 49 completed sections**, no observed failures.
  Its SHA-256 is `50de25fbd56433c01fe1077900c80706b5a284566fc4843f4c106a47872f4955`,
  not the hash in `workspace-checks.json`. The old 758/42 and 1925/125 summaries
  were stale and also collapsed identically named targets across packages.
  Original logs/manifests remain preserved, but this hash-mismatched partial
  invocation is **not reusable final-state verification evidence**.
  `../m4-ci-continuation/historical-workspace-audit.json` provides the corrected
  exact-name audit; its new bounded batch evidence supersedes the old
  `coverage-summary.json`, now corrected and explicitly marked superseded;
  its original stale bytes are preserved in the continuation evidence.
  **Full local CI remains incomplete until the continuation's exact-name
  reconciliation and frontend/E2E/locale/SQLx gates are checked.** Hosted CI is
  not established by local tests. Independent review of the packaging slice
  found no Critical/Important findings; the two Minor corrections and fresh
  verification results require separate independent review.

### Bounded local CI continuation (uncommitted minor corrections)

Evidence root: `~/.hermes/cache/scratch/m4-ci-continuation/`.
`gate-summary.json`, `reconciliation.json`, `exact-test-records.json`,
`feature-ml-reconciliation.json` and per-invocation JSON/logs retain HEAD,
commands, exit/time/log hashes, source-file hashes and DB inventories. No
single full-workspace execution was retried; every package was bounded.

- Linkage checker now rejects every `=> not found` and requires an absolute
  resolved libgomp path. Seven shell cases pass, including real ELF positive,
  wrong prefix, missing gomp and missing transitive dependency cases. The old
  checker accepts the real missing-gomp case; its failing regression log is
  retained. CI invokes the new shell suite.
- Default workspace: **1913 passed, two ignored, 15 deliberately filtered**;
  all 141 sections finished, but the exact 1930-test inventory is **incomplete**.
  The 15 exact names are listed in `reconciliation.json`: 11
  `kima-db/autotag_identity`, four `kima-ident/artist`. SQLx deterministically
  names its databases and drops a pre-existing same-name DB before each test;
  those remaining pre-existing databases were preserved by filtering their
  colliding tests, not by weakening tests or changing production code.
- Database incident: the first fresh API batch let SQLx automatically remove
  **11 pre-existing SQLx databases** before this hazard was recognized. All
  appear in the earlier run's `created-resources.json`; exact removed names
  are recorded in `tests-kima-api.json` and `gate-summary.json`. No explicit
  DROP was issued, but this is disclosed as a preservation-boundary issue,
  not presented as cleanup. Subsequent batches removed no pre-existing DBs.
- Full native ML: **118 default / 125 pg-publish passed**, including doctests
  and four actual migrated-Postgres adapter tests. Exact feature inventory
  reconciles with no missing/extra names. Both runs used the pinned extracted
  private libsoxr and checksum-verified ORT; no host library was replaced.
- Frontend: offline `npm ci`, typecheck, **482 unit tests in 44 files**, and
  static build passed (checks rerun after npm ci). Local Node is v26.7.0, not
  hosted CI's Node 24. Browser **12/12 passed** against the real export/server
  and a fresh run-owned DB. The supervisor then failed due to an evidence-only
  SQL readback typo (`track_overrides` rather than `track_tag_overrides`);
  failed logs remain. Corrected standalone readback passed: one undone run,
  14 present tracks, restored `Plain Album`, zero overrides. Server stopped.
- SQLx metadata check (`prepare --check --workspace -- --all-targets`, online
  schema on a fresh migrated run-owned DB, offline/locked Cargo) passed.
  Formatting, default workspace/all-target Clippy, feature ML/all-target
  Clippy with `-D warnings`, and whitespace check passed.
- **Still missing:** the 15 preservation-filtered workspace tests; C-locale
  `login_limit`/`auth`; independent review of these corrections/evidence; hosted
  CI. A run-owned C/C database was created on 55476, but SQLx creates its test
  children from en_US.utf8 `template1`, not the connection's base database;
  claiming the locale gate from that base would be false. Neither template1
  nor other clusters/dev resources were modified. A separately authorized C
  cluster and resolution of pre-existing test DB ownership are needed.

No commit, push, merge, private-model bundling or production ML wiring was
performed. Three explicit run-owned databases (`kima_m4_ci_1dd7cf3_{e2e,meta,locale_c}`)
and SQLx-created resources are retained in `resource-final.json` for approved
cleanup; development port 5456 and its volumes were not used.

Resources retained for owner-approved post-M4 cleanup: images
`kima-m4-soxr:review`, `kima-m4-soxr-backend:review`; exited probe containers
`kima-m4-soxr-{probe-build,probe-build-cache,probe-build-feature}`,
`kima-m4-soxr-final-{clap-cold-start,clap-prepare,onnx-runtime,onnx-session}`,
`kima-m4-soxr-feature-{clap-cold-start,clap-prepare}`; failed native build
container `efecd6edd0eb` (`nice_keller`); isolated Postgres container
`kima-m4-soxr-db`, bound only to `127.0.0.1:55476`, named volume
`kima-m4-soxr-pgdata`, base database `kima_m4_soxr` and SQLx-created databases.
The new server's fresh identity was checked before tests; existing 55474/55475
servers were only inspected, and development resources were untouched.
`resource-inventory.json`, `workspace-checks.json`, `ml-feature-full.json`,
`docker-{final,feature}-probes.json` and full logs retain commands, exits,
checksums and partial coverage; scratch targets/source exports are also kept.

### Later isolated-gates run: the previously-missing gates, closed

Evidence root: `~/.hermes/cache/scratch/m4-isolated-gates/` (`summary.json`,
`combined-workspace-reconciliation.json`, `verification-notes.md`). This run
closes the gates the bounded continuation above left **"Still missing"**; it
does not supersede or erase that history (the 15-test filtering, the
11-database preservation incident and the evidence-only supervisor failure
above remain the authoritative record of what happened at HEAD `1dd7cf3` on
first attempt).

- **Exact-name workspace reconciliation is now complete**: 1930 listed,
  1930 observed, 1928 passed, 2 ignored, 0 missing, 0 extra, all 141
  sections finished. The 15 tests the earlier continuation had to filter to
  avoid the SQLx preservation hazard (11 `kima-db/autotag_identity`, 4
  `kima-ident/artist`) ran and passed, against entirely fresh, run-owned
  Docker Postgres clusters (not the long-lived shared 55474/55476 servers),
  so no further pre-existing database was ever at risk.
- **C-locale gate**: 35 live-child-locale cases observed and passed against
  a dedicated `C`-locale cluster (`kima-m4-gates-*-c`, port 32777), separate
  from the earlier inconclusive attempt against `55476`'s base-database
  locale (SQLx's own test children still come from `template1`, so that
  base-locale attempt correctly stayed inconclusive and is preserved as-is
  above).
- **Frontend**: Node 24.22.0 (hosted CI's version, not the earlier run's
  local Node 26.7.0), npm ci / typecheck / build all exit 0, **482 unit
  tests across 44 files** passed.
- **E2E**: **12/12 Playwright cases green** (0 skipped, 0 unexpected, 0
  flaky) against a fresh run-owned database and the real static export +
  server, superseding the earlier evidence-only SQL-readback typo failure
  only as a *result*, not as a *correction to the record*: that failure's
  log and the corrected standalone readback remain exactly as documented
  above ("The supervisor then failed due to an evidence-only SQL readback
  typo...").
- Source-hash reconciliation (`historical-source-differences.json`) confirms
  the only files that differ between the earlier continuation's stamped
  source and this run's are the three already-disclosed non-Cargo-input
  files (`docs/DIVERGENCES.md`, this spec, `scripts/test-soxr-linkage.sh`);
  no Cargo test input changed, so the new pass counts are comparable to the
  old ones.
- All Docker resources (two Postgres clusters, one Node 24 container) are
  run-owned, newly created, retained for owner-approved cleanup, and never
  touched the pre-existing 55474/55475/55476 servers or development
  resources (`preexisting_resources_removed: []`).
- **Still outstanding**: independent review of this evidence and of the two
  fixes below (CI `TMPDIR` for the soxr-linkage self-test step, and the
  hardened `m4-ci-continuation/run.py` fail-closed guards); hosted CI on the
  exact pushed commit. No commit, push or merge has occurred.

### Fixes applied after independent review (uncommitted, same worktree)

- **CI**: `.github/workflows/ci.yml`'s "Native soxr linkage checker
  self-test" step now sets `TMPDIR: ${{ runner.temp }}` explicitly. Local
  `TMPDIR` had been masking the step's unguarded dependency on a scratch
  directory: `env -u TMPDIR sh scripts/test-soxr-linkage.sh` fails fast
  (`set TMPDIR to a scratch directory`, exit 1) exactly as a hosted runner
  would if the step forgot to supply `RUNNER_TEMP`-equivalent. With
  `TMPDIR` set to a fresh scratch directory (the `RUNNER_TEMP`-equivalent),
  all 7 shell linkage cases pass (positive and missing-dependency negative
  cases both reproduced locally).
- **Private CI-continuation runner** (`~/.hermes/cache/scratch/m4-ci-continuation/run.py`,
  not part of the repository): the previous version (preserved as
  `run.py.orig-1dd7cf3`) failed open on two points a reviewer flagged as
  Important: a missing `preexisting-collisions.json` silently became `[]`
  (no protection) instead of aborting, and any command shape other than a
  recognized `cargo test ...` ran unprotected because the database
  inventory happened unconditionally before any shape check. The fix
  (a) classifies every `args` list against a closed allowlist grammar
  (`classify_args`) instead of `args.index('-p')`-style scanning, rejecting
  anything unrecognized before any DB access or subprocess exec; (b) aborts
  with `SystemExit` if the collision inventory file is missing, before any
  DB access; (c) requires a separate `cluster-ownership.json` manifest and
  re-verifies it against a live (read-only) `docker inspect` before
  touching the database, so a fresh, exclusively-owned cluster — not a
  command-shape inference — is the actual safety boundary, with the
  exact-name `--skip` filtering kept only as defense-in-depth for the one
  long-lived shared cluster. `connect_cluster()`'s real docker/psql calls
  moved out of module import into an explicit function so importing the
  module for tests touches nothing live.
  `~/.hermes/cache/scratch/m4-ci-continuation/test_run_guards.py` exercises
  this with fake `inventory`/`Popen`/`docker_inspect` sentinels only (11
  tests, all passing): a missing collision inventory aborts before
  `inventory()`/`Popen` run; an unrecognized shape (e.g. a bare shell
  command) aborts the same way even when both guard files are present and
  valid; a live-container id or port mismatch against the manifest aborts
  the same way; and a fully-satisfied happy path proves the exact-name
  `--skip` filtering still fires using only fake sentinels. This script and
  its tests were never run against the real 55474/55475/55476 servers.

## Decisions required before implementation

1. Approve the stage graph, claim ordering and whether speculative embedding is permitted; define terminal closure/reopen, retry, restart, disabled CLAP, cancellation and missing-file semantics. Do not conflate independent completion with unrestricted eligibility.
2. Define changed/replaced file identity and invalidation, concurrency, claim/lease expiry and stale-write protections, including separate successful analysis and embedding transactions; determine bounds, admission, worker defaults and live resize. Test races with deterministic barriers and do not hold unbounded buffers.
3. Review the proposed capability dependency graph, status and recovery defaults above against the stage graph; settle exact API/UI and GPU milestone scope. Identity work must not falsely report ML results.
4. Decide schema, model/version provenance, reanalysis and DSP-versus-musicnn atomicity; preserve a future M12 push-style remote inference boundary without implementing transport now.
5. Clear rights and immutable, independently verified artifact identities, notices, fixture generation, intended audience and offline CI provisioning; inspect exact model inputs/outputs, preprocessing, head transformations and edge handling. Freeze licensed DSP data, labels, denominator, and silent/short/invalid/nonfinite behavior before scoring.

## Owner decisions (2026-11-04): Stage::Embedding scheduling

Recorded before implementation, per the owner's explicit direction:

1. **Scheduling shape.** CLAP embedding scheduling is a new phase inside the
   *existing* coordinator enrich cycle
   (`crates/kima-ident/src/coordinator/mod.rs`'s `cycle` function/`run_cycle`), not a
   separate queue or worker loop. An `embedding_phase` runs alongside the existing
   `fingerprint_phase`/`network_phases`, independent of the MusicBrainz on/off switch:
   it must keep running even when the MusicBrainz network phases are paused or
   disabled. Implementation: `embedding_phase` is joined together with
   `fingerprint_phase` and `network_phases` inside `run_cycle`'s `phases` future,
   exactly like `fingerprint_phase` already is — neither depends on
   `network_may_run`/`ctx.net`.
2. **Eligibility.** CLAP eligibility is independent of any future Analysis/MusicNN/DSP
   stage: a track becomes eligible for the new `Stage::Embedding` as soon as it
   exists/is scanned — the same due-ness trigger as the existing unconditional stage
   (`Stage::Fingerprint`), not gated on `Recording` or `Fingerprint` success. Verified
   against `kima_db::stages::claim`'s SQL contract: its only stage-specific gating
   clause is `$1 <> 'recording' OR ...`, so any stage other than `'recording'`
   (`'fingerprint'`, now `'embedding'`) is claimed unconditionally — "a row in
   `tracks`" (missing_since IS NULL) is the entire eligibility test, mirroring
   `Stage::Fingerprint` exactly. No SQL change to `claim` was needed for this.
3. **Retry/lease policy.** Exactly the same constants/backoff/lease-duration already
   used for `Stage::Recording` in `crates/kima-db/src/stages.rs`: no new constants were
   introduced. The lease (`claim`'s `next_attempt_at = now() + interval '15 minutes'`)
   is written identically for every stage in one SQL statement; the redelivery cap
   (`REDELIVERY_CAP = 3`), retry cap (`RETRY_CAP = 4`), retry delays (`RETRY_DELAYS`)
   and `record_for`/`lost_worker` helpers in `crates/kima-ident/src/stages/mod.rs` are
   shared, generic-over-stage code already used by `Stage::Recording`'s
   `kima_ident::stages::recording` module; `kima_ident::stages::embedding` reuses them
   unchanged rather than copying or reinventing values.

Implementation record: `Stage::Embedding` added to `crates/kima-db/src/stages.rs`;
`Phase::Embedding` added to `crates/kima-db/src/enrichment.rs` (and
`enrichment_state.current_phase`'s CHECK constraint, migration `0025`) purely so the
new phase can use the existing `mark()`/`Announcer` progress-announcement plumbing —
no change to the M4 progress-readout numbers (`ClapProgress` etc.) is made here; that
remains a separate, not-yet-scoped task. `kima_ident::stages::embedding::run_claim`
calls the already-reviewed `kima_ml::attempt::run_attempt` with
`AttemptKind::EmbeddingAfterAnalysis` (CLAP-only: analysis is never invoked, matching
the "independent of Analysis" decision above) and, on success, publishes through
`kima_ml::publish::publish_clap_embedding_blocking`. A missing or hash-mismatched CLAP
model surfaces as `CapabilityStatus::Unavailable`/`Blocked`, which the stage reports as
a stage failure (`StageFailure::Transient`, classified `INCONCLUSIVE`) so the existing
retry/backoff applies — it never crashes the coordinator and never fabricates a
published embedding.

## Upstream drift note (2026-11-04): CLAP checkpoint migration and MusiCNN mood-head removal deferred

This design and the current M4 implementation are evidence-grounded against upstream
`Chevron7Locked/kima-hub` commit
[`12faf1cd34ff53054221818b0ccde58b8e9b0020`](https://github.com/Chevron7Locked/kima-hub/tree/12faf1cd34ff53054221818b0ccde58b8e9b0020)
(§ "Source grounding" above), which remains kima-rs's pinned reference. As of this
note, upstream `main` has moved to `166b7184` (583 commits ahead of the pin). Of
particular relevance to this document's §6 model inventory and MusiCNN head table:

- Upstream commit `14aaa9f7` ("refactor(analysis): drop raw MusiCNN moods and run CLAP
  on ONNX only") removes the seven MusiCNN mood_* head models (`acoustic`,
  `aggressive`, `electronic`, `happy`, `party`, `relaxed`, `sad`), keeping only
  `danceability`, `deam` and `emomusic`.
- The same commit swaps the CLAP checkpoint from `laion/clap-htsat-unfused` (the
  pinned `hf_audio.onnx`, SHA-256 `37df17af...`, referenced in §6.4 above) to
  `lukewys/laion_clap`'s `music_audioset_epoch_15_esc_90.14.pt` checkpoint, exporting
  new `music_audio.onnx` (SHA-256
  `ab1bf5eba067b246198c630f153f774e8f1c3169b3924d4d6a4dc247bd73de97`) and
  `music_text.onnx` (SHA-256
  `6dbba074b27f8973d84c5c7ae9acd0b9139a6cd9dd82bea6e87954dc3b835956`) in their place.
- The `musicnn-answers.npz` parity fixture (§6.4 above, independently hashed as
  `05107f43...`) also changed upstream to match the reduced head set.

**Owner decision:** stay pinned to `12faf1cd34ff53054221818b0ccde58b8e9b0020` for now.
All M4 work to date (model inventory, MusiCNN head regression tests, CLAP audio/text
adapters, the `hf_audio.onnx`/`text_model.onnx` pins) is already verified and working
against that pin, and re-pinning now would invalidate that verification mid-milestone.
The CLAP music-checkpoint migration and MusiCNN mood-head removal are **deferred to a
separate future task**, not undertaken here. See `docs/DIVERGENCES.md` D-148 for the
tracked divergence entry. A future upgrade task must: re-export and re-verify
`music_audio.onnx`/`music_text.onnx` against the new upstream checkpoint and hashes
above (replacing `hf_audio.onnx`/`text_model.onnx` in the §6.4 table), and drop or
explicitly ignore the seven removed `mood_*` MusiCNN heads (replacing the eleven-file
MANIFEST.sha256 inventory referenced in §6.4 with upstream's reduced set), re-running
the full MusiCNN head regression suite and CLAP parity fixtures against the new
pinned bytes before enabling the upgraded capability.

## Review and verification checklist

- Independently review this clarification against parent §6 and owner decision, and the candidate graph against the pinned source; owner approval is needed for outstanding decisions, not inferred from this draft.
- Exercise combined success, analysis failure, embedding failure, restart after analysis success, embedding-only retry, disabled model, file replacement, expired claims and bounded concurrency. Assert no stale result is made current and no successful analysis is unnecessarily rerun.
- For each approved dependency mapping, start with a missing or hash-mismatched model and assert that the server starts, dependent capabilities report unavailable without false success, and independently verifiable capabilities remain usable. Cover multiple failures and shared dependencies without assuming a particular mapping in advance.
- Verify hashes and reference shapes, finite/nonzero vectors, independent per-case cosine gates and end-to-end head comparisons; deliberately corrupt an asset to prove integrity checks fail. Report real fixture counts, minimum scores and DSP numerator/denominator. Missing rights or artifacts block certification.
- Use strict red/green/refactor for implementation and independent spec/quality review. Database tests require a separately authorized isolated database with both `DATABASE_URL` and `PGDATABASE` verified; never use the development database. No models, fixtures, code, tests, migrations or database changes are delivered by this document.
