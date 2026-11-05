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

#### Runtime snapshot failure and enrichment readouts (fix round `deleg_bddde9ca`)

This bounded slice addresses the two Important findings in independent review
`deleg_9d00c60f`; implementation is **pending independent re-review**, not M4 closure.

- A global snapshot failure discovered after startup latches the first reason in the
  shared runtime (not in per-track retry state). Audio alone becomes unavailable on the
  actual `/api/enrichment/status` `mlCapabilities` path. Unstarted claims are released,
  and subsequent cycles claim nothing. Repair infrastructure and **restart** to clear
  the latch and revalidate; no periodic per-file probes or live reload. This follows
  the restart-only recovery default above. Independent text remains usable. The first
  failed track keeps its existing retry schedule; no automatic attempt/reset on restart.
- `clapEmbeddings` reads the real embedding stage: present-track total, done, no-row or
  pending, working, retrying plus terminal failures (`failed`), retrying subset, and
  `waitingMissing`, using the same stage-count rules as existing fingerprint readouts.
  Progress is the existing rounded done/total percentage, not inferred model availability.
  The status `audio` counter uses those same embedding counts; `audioAnalysis` remains
  zero because MusiCNN production integration is not part of this fix.
- Embedding failures use the existing failure API's `entityType=audio`; unfiltered and
  filtered lists include retrying and terminal rows, excluding `FILE_MISSING`, with
  unchanged pagination and stage metadata. Failure `audio` and `total` counts agree.
  `coreComplete` stays metadata-only. `isFullyComplete` also requires embedding work
  settled (no pending/working/retrying), just like enabled fingerprint background work;
  terminal/missing rows settle work but never increment successful embedding counts.
  Disabled/unavailable CLAP with outstanding work therefore cannot falsely imply full
  completion; capability status explains why work is unavailable.
- Regression evidence and exact source manifests are retained outside Git at
  `/home/dimmu/.hermes/cache/scratch/m4-fix-bddde9ca/`. Real audio/text/private-model
  inference evidence is explicitly separate from ordinary model-free DB/API tests.
  The API status test uses the production router with the shared runtime and checks
  the before/after infrastructure failure path, not a fabricated status response.

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

## DB-free MusiCNN compute adapter slice (2026-11-04, uncommitted review candidate)

`kima_ml::musicnn::{MusicnnCompute, MusicnnComputation, MusicnnPatchOutputs}`
(`crates/kima-ml/src/musicnn/compute.rs`) wires the existing, independently
checked `MusicnnFrontend`/`mel_patches` (§6 frontend, Essentia-pinned) to the
existing typed `MusicnnCoreSession`/`MusicnnHeadSession` ONNX adapters
(`onnx.rs`) over all eleven pinned models (core + ten `MusiCnnHead`). This is
**computation only**: no database, persistence, scheduler, shared-decode
wiring, result schema/column, aggregation (mean/temperature-scaling/clamping/
rounding/mood-tag), or production attempt-runner registration is added. The
caller still owns decode, eligibility, claims and publication.

- Input: the existing mono 16 kHz final-90s (or whole-shorter-track) tail,
  matching the owner's clarification above that musicnn/DSP use the tail
  while CLAP uses whole-track windows (the tail is *not* re-derived here; the
  caller already produces it per the existing decoder/analysis boundary).
- All eleven pinned models (core + all ten heads named in §6.4) are mandatory,
  matching the binding spec's "one missing head makes the result unavailable"
  rule: `from_verified_artifacts` fails closed (`OnnxError::MissingHead`) if
  any of the ten is absent, and does not degrade per-head.
- CPU batch group size is **1** patch per model call, exactly reproducing the
  pinned upstream `MusicnnBody`/`MusicnnOnnx` (`services/audio-analyzer/musicnn_onnx.py`
  at `12faf1cd34ff53054221818b0ccde58b8e9b0020`, `CPU_GROUP = 1`); there is no
  server/GPU batching path here (CPU-only is current scope; GPU is M8).
  `MusicnnComputation::NoFullPatch` is returned for input shorter than one
  187-frame patch (upstream: `embeddings.shape[0] == 0`, heads return `None`),
  never a fabricated zero/neutral embedding or head row.
- Every patch-level core/head output is checked for exact row count and
  finiteness before being accepted; a single bad batch aborts the whole
  computation (fails closed) rather than silently dropping that patch or head.
- **Real-model parity gate (binding, §6.4 "CLAP and musicnn cosine ≥ 0.999"):**
  exercised against an independently generated reference. Pinned upstream
  `musicnn_onnx.py`/`analyzer.py` were fetched from
  `github.com/Chevron7Locked/kima-hub` at the pinned commit (SHA-256
  `43710bb5824cee760e9efe96e1dc0a1b5394d40f06330d64fb38de6e11121d0f` /
  `61e8d09a050d55c42b29a876ae80ffeee591c522923d42a4f1aca0346ff69559`) and run
  unmodified in a separate Python venv (real Essentia `2.1-beta6-dev` mel
  frontend, real `onnxruntime` 1.22.0, the same verified private model bytes
  the Rust test loads — fstat/open-flag verified, SHA-256 checked against
  `~/.cache/kima-models/m4-approved-private/manifest.json`, which itself
  anchors to `~/.cache/kima-models/m4-provenance/artifact-inventory.json`
  outside the writable asset dir) against three cases: a 373-frame period-17
  synthetic signal (3 patches), the pinned upstream 30 s `gate_signal()` (19
  patches), and a 90 s nonstationary chirp+tone signal (59 patches, exercising
  the full tail length). `crates/kima-ml/tests/musicnn_compute.rs`
  (`private_pinned_upstream_real_model_parity`, `#[ignore]`) reproduces this in
  Rust and asserts cosine ≥ 0.999 per row for the core embedding and all ten
  heads across all three cases: **measured minimum cosine 0.9999999999989,
  maximum absolute difference 4.77e-6** (float32 core/head rounding plus ORT
  1.22.0 vs. upstream-pinned-requirements 1.30.0; both are the same ORT native
  major/API version used elsewhere in this project, not a reproduced ORT
  1.30.0 run — flagged as a residual, not a parity failure given the margin).
  This is Essentia-frontend-to-Rust-frontend plus ONNX-core/head parity against
  the real pinned bytes, not a TensorFlow-to-ONNX conversion re-validation.
  Reference generation and evidence live outside this repository
  (`~/.hermes/cache/scratch/m4-musicnn-compute/`, private, not redistributed);
  the test fails closed with an explicit panic (`KIMA_MUSICNN_REFERENCE_DIR`/
  `KIMA_MUSICNN_MODEL_DIR` unset) rather than skipping when the private
  prerequisites are absent.
- `musicnn_integration.rs`'s existing Essentia-frontend-only fixture test is
  unchanged; this slice adds `musicnn_compute.rs` (real-model, `#[ignore]`)
  layered strictly on top of it, plus `musicnn/compute.rs`'s synthetic
  in-process unit tests (nonconstant multi-head batching order, and bad-shape/
  non-finite core and head output rejection) that need no models.
- Not addressed by this slice: DSP (LUFS/LRA/key/tempo), head-value
  aggregation into a stored analysis result, GPU batching/server process, and
  wiring into `attempt.rs`'s existing `Combined`/`EmbeddingAfterAnalysis`
  callbacks or any production scheduler/coordinator registration.

## Pure per-track MusiCNN aggregation slice (2026-11-04, uncommitted review candidate)

Continues directly from the DB-free MusiCNN compute adapter slice above,
closing its explicitly named "head-value aggregation into a stored analysis
result" gap, and only that gap: still **pure computation**, no persistence,
no DSP, no scheduler/`attempt.rs`/coordinator wiring, no stored-result
schema/column.

- **Recovered exact pinned upstream per-track reduction** (not invented):
  `services/audio-analyzer/analyzer.py`'s `AudioAnalyzer._extract_ml_features`
  / `positive_prob` / `_generate_mood_tags`'s inputs, at the same pin used for
  D-151's compute parity (`12faf1cd34ff53054221818b0ccde58b8e9b0020`), fetched
  and hash-pinned (SHA-256
  `61e8d09a050d55c42b29a876ae80ffeee591c522923d42a4f1aca0346ff69559`):
  - The 8 binary heads (`danceability` + the 7 `mood_*` heads) each reduce to
    `round(max(0, min(1, mean(column))), 3)` — the mean, over all patches, of
    the *positive class's* probability column, where the column is picked per
    head by `positive_class_index` from the verbatim `HEAD_CLASSES`/
    `HEAD_POSITIVE_CLASS` tables (e.g. `mood_sad`'s positive class `sad` is
    column 1 because `HEAD_CLASSES['mood_sad'] = ['non_sad', 'sad']`, while
    `mood_happy`'s positive class `happy` is column 0).
  - `deam`/`emomusic` are 2-column `[valence, arousal]` regressions on a 1-9
    scale, not classifiers. `valence_raw`/`arousal_raw` are the **mean of the
    two heads' own per-patch column means** (`sum(mean(p[:,col]) for p in
    (deam, emomusic)) / 2`, not a combined mean over both heads' patch rows
    together). Both are then "temperature scaled" around a centre of 5.0 by a
    factor of 2.5 (`center + (raw - center) * temperature`), clamped to
    `[1, 9]`, linearly rescaled to `[0, 1]` via `(scaled - 1) / 8`, and rounded
    to 3 decimals.
  - `danceabilityMl` and `danceability` are upstream's **same** `positive_prob`
    value stored twice (two DB columns, one computation).
  - Genre (`essentiaGenres`, Essentia's own genre classifier, not a MusiCNN
    head) and mood tags (`_generate_mood_tags`, which additionally needs
    `bpm`/`keyScale` that do not come from `MusicnnPatchOutputs`) are **not**
    part of this per-track MusiCNN reduction and are correctly out of scope
    for this contract, not an oversight.
  - This rule is not ambiguous or undecided in the pinned source: the above
    is a direct, line-level reading of `_extract_ml_features`
    (`services/audio-analyzer/analyzer.py` lines ~587-628 at the pin), not an
    inference from tests, docs or commit messages.
- **Implementation:** `kima_ml::musicnn::{MusicnnAggregate, MusicnnAnalysisResult,
  aggregate, aggregate_patches}` (`crates/kima-ml/src/musicnn/aggregate.rs`),
  a pure function over `&MusicnnComputation`/`&MusicnnPatchOutputs` reproducing
  exactly the above reduction. `MusicnnComputation::NoFullPatch` passes
  straight through to `MusicnnAnalysisResult::NoFullPatch` — never a
  fabricated neutral/zero aggregate — matching upstream's `predict()`
  returning `None` for every head on zero patches (no track-level feature
  exists at all, not a feature equal to some default).
  - `round(x, 3)` is implemented as round-half-away-from-zero on an `f64`
    accumulator (`(x * 1000.0).round() / 1000.0`), not CPython's
    round-half-to-even on the exact double value. These differ only at an
    exact `*.*5` boundary of the intermediate f64 — a measure-zero case for
    values produced by averaging float32 model output — and is a documented
    implementation choice, not a different aggregation rule.
  - The mean uses an `f64` accumulator rather than numpy's float32-by-default
    `np.mean` on a float32 array; this is a numerically safer equivalent
    summation, not a different reduction, and is likewise documented rather
    than silently assumed identical.
  - `MusicnnPatchOutputs::try_new` (new public constructor, used both here
    for testing and internally by `MusicnnCompute::compute`'s existing
    validation path, which now delegates to it instead of duplicating the
    same checks) enforces: nonempty, exactly the ten pinned heads present,
    every head's row count matching the patch count, and every value finite
    — so a hand-built `MusicnnPatchOutputs` used to feed the aggregation
    function under test can never silently carry malformed real-model-shaped
    data into the reduction.
- **TDD (synthetic, no models):** `crates/kima-ml/src/musicnn/aggregate.rs`'s
  `#[cfg(test)]` module exercises the exact reduction math directly: the
  binary-head mean/clamp/round, the per-head positive-column table (every one
  of the 8 classifier heads individually, keyed against the verbatim
  `HEAD_CLASSES`/`HEAD_POSITIVE_CLASS` transcription), the clamp independent
  of the final rescale, the valence/arousal mean-of-means plus temperature
  scale plus its own separate `[1,9]` clamp (exercised with inputs that only
  that inner clamp bounds, not the outer `[0,1]` one), `NoFullPatch`
  pass-through, and that `aggregate()` wraps `aggregate_patches()` without
  altering it. 9 tests, all green (`cargo test -p kima-ml --lib musicnn::`).
- **Private real-model parity (binding gate, reduction-math scope):**
  `crates/kima-ml/tests/musicnn_aggregate.rs::private_pinned_upstream_aggregate_parity`
  (`#[ignore]`, hard-panics — not a silent skip — if
  `KIMA_MUSICNN_REFERENCE_DIR` is unset). It reuses, unmodified, the real
  per-patch model outputs the prior D-151/compute slice already captured from
  pinned upstream `musicnn_onnx.py` against the verified private model bytes
  (`~/.hermes/cache/scratch/m4-musicnn-compute/private-reference/{period17,
  gate30,chirp90}/*.f32`; cosine parity against this Rust ONNX path was
  already established there at ≥0.999, measured min 0.9999999999989). This
  test does **not** re-run ONNX or Essentia — that parity is not repeated
  here — it isolates and proves only the per-track reduction step: it builds
  a `MusicnnPatchOutputs` directly from those real arrays via `try_new`, calls
  `aggregate_patches`, and compares against a **separately generated**
  reference (`~/.hermes/cache/scratch/m4-musicnn-compute/reference_aggregate.py`,
  hash-checked against the same pinned `analyzer.py` fetched for this slice,
  `private-reference/aggregate-reference.json`) that transcribes
  `_extract_ml_features`/`positive_prob` **verbatim** over the same real
  arrays, independently of the Rust implementation. All three cases
  (3/19/59 patches) match on every one of the 10 aggregate fields within
  `1e-6` absolute — effectively exact, since both sides consume the same
  pre-computed real-model floats and only the pure reduction math can differ.
  Evidence (private, not redistributed):
  `~/.hermes/cache/scratch/m4-musicnn-compute/{analyzer.py,
  reference_aggregate.py, aggregate-reference.json}`.
- **Verification, bounded (<300s/tranche):**
  `cargo test -p kima-ml --lib musicnn:: --test musicnn_aggregate -- --include-ignored`
  (9 lib + 1 ignored aggregate test, with `KIMA_MUSICNN_REFERENCE_DIR` set) and
  `cargo test -p kima-ml --test musicnn_compute --test musicnn_aggregate --
  --include-ignored --test-threads=1` (2 compute + 1 aggregate real-model
  tests, `ORT_DYLIB_PATH`/`KIMA_MUSICNN_MODEL_DIR`/`KIMA_MUSICNN_REFERENCE_DIR`
  set) both green in 11.9s. `cargo test -p kima-ml --all-targets` (no private
  env, `ORT_DYLIB_PATH` set): every non-ignored test across every target
  passed, 0 failed (34 test binaries/modules, totals reconciled by `cargo
  test` output; the previously-flagged pre-existing `clap_cold_start`
  libsoxr-race failure did not reproduce in this run — unrelated to this
  change either way, tracked separately in the skill's pitfalls). `cargo fmt
  --check -p kima-ml` and `cargo clippy -p kima-ml --all-targets -- -D
  warnings` both clean after this change; the only files touched by `cargo
  fmt -p kima-ml` were this slice's own new files (confirmed via `git diff`
  against the already-dirty `clap.rs`/`lib.rs`/`pg_publish.rs`, which were
  untouched by the formatter and are unrelated preserved integration state).
- **Changed/new files this round:** `crates/kima-ml/src/musicnn/aggregate.rs`
  (new), `crates/kima-ml/src/musicnn/compute.rs` (adds
  `MusicnnPatchOutputs::try_new` and the `InvalidPatchOutputs` error variant;
  `run_batches` now calls it instead of constructing the struct inline — no
  behavior change, same checks, single source of truth), `crates/kima-ml/src/musicnn.rs`
  (module wiring), `crates/kima-ml/Cargo.toml` (adds `serde_json` as a
  **dev-dependency only**, already a workspace dependency used by every other
  crate — for parsing the JSON aggregation reference in the integration
  test; no change to the default/runtime dependency graph),
  `crates/kima-ml/tests/musicnn_aggregate.rs` (new), this spec section,
  `docs/DIVERGENCES.md` D-152 (accepted). No changes to `attempt.rs`,
  `stages/embedding.rs`, DB, scheduler, or the D-151 compute module's public
  behavior.
- **Not addressed by this slice (open, in order of likely next step):** DSP
  (LUFS/LRA/key/tempo — needed before mood tags can exist at all), mood-tag
  generation, genre, a stored analysis result type/schema/column for this
  aggregate, GPU batching/server process, and wiring into `attempt.rs`'s
  `Combined`/`EmbeddingAfterAnalysis` callbacks or any production
  scheduler/coordinator registration. No DB/persistence/scheduler/attempt
  runner code was touched.

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

## DB-free DSP loudness (LUFS/LRA) compute slice; key/tempo grounding blocker (2026-11-04, review-ready)

- Scope: pure DSP compute only (`kima_ml::dsp::analyze_loudness`), DB-free, no
  persistence/scheduler/`attempt.rs` wiring, reusing the existing shared-decode
  / 16 kHz-mono / last-90-seconds (or whole shorter track) musicnn/DSP tail —
  **not** the whole-track CLAP windows. Prior musicnn `compute.rs`/`aggregate.rs`
  slices (D-151/D-152, previously reviewed clean) are untouched; no overlapping
  edits to those files.
- **Window verification, not assumed:** checked pinned upstream
  [`analyzer.py`](https://github.com/Chevron7Locked/kima-hub/blob/12faf1cd34ff53054221818b0ccde58b8e9b0020/services/audio-analyzer/analyzer.py)
  (`AudioAnalyzer.__init__`) before writing any code. Upstream's
  `self.loudness_ebu = es.LoudnessEBUR128(sampleRate=44100)` and
  `self.rhythm_extractor`/`self.key_extractor` all run on upstream's own
  **44.1 kHz** decode path, *not* the 16 kHz resample it separately creates
  only for musicnn (`self.resampler = es.Resample(inputSampleRate=44100,
  outputSampleRate=16000)`, called right before the mel/musicnn step). So
  upstream's own DSP window/rate differs from its musicnn window/rate; this
  is consistent with — not contradicted by — the owner's already-approved
  kima-rs design (`docs/superpowers/specs/2026-10-16-kima-rust-design.md` line
  137: "16 kHz mono (musicnn, DSP)") fixing **both** musicnn and DSP to the
  same 16 kHz shared-decode tail in kima-rs, which is a deliberate,
  already-accepted implementation choice (not newly invented here), and EBU
  R128 loudness is rate-invariant for a signal expressed in real time (see the
  `known_level_sine_is_sample_rate_independent` test below), so the 44.1 kHz
  vs 16 kHz difference does not change the computed LUFS/LRA value.
- **Loudness (LUFS/LRA): grounded, implemented.** Upstream's
  `es.LoudnessEBUR128` implements the EBU R128 recommendation (EBU Tech
  3341 M/S/I modes, EBU Tech 3342 loudness range). The parent binding design
  doc already resolves the Rust side of this exactly ("DSP features (diverges
  from Essentia)", line 162: **"Loudness: `ebur128` (integrated LUFS,
  loudness range), exact."**) — i.e. this is not an algorithm divergence (same
  EBU R128 standard), only an implementation-library substitution (the
  `ebur128` crate, sdroege, MIT, "a Rust port of the libebur128 C library,
  produces the same results as the C library", vs. Essentia's C++). New
  `kima_ml::dsp::{LoudnessAnalysis, analyze_loudness, DspError}`
  (`crates/kima-ml/src/dsp/mod.rs`): takes mono PCM `&[f32]` + sample rate,
  runs `EbuR128::new(1, rate, Mode::I | Mode::LRA)` +
  `add_frames_f32`/`loudness_global`/`loudness_range`; rejects empty input,
  zero sample rate, non-finite samples, and reports digital-silence
  (below-absolute-gate, `-inf` loudness) as an explicit `UndefinedLoudness`
  error rather than a silent `-inf` or fabricated floor.
- **TDD, synthetic, no models needed:** 9 tests in `dsp::tests`
  (`cargo test -p kima-ml --lib dsp::` → 9/9, 0.04s compute time, ~4s cold
  build). Error-path coverage (empty/zero-rate/non-finite/digital-silence).
  Known-level coverage grounded in the **ITU-R BS.1770-4 conformance
  calibration point** (997 Hz mono sine at -20 dBFS peak → -23.0 LUFS ± 0.1,
  the standard's own program-loudness calibration reference, not a value
  invented by running this code and asserting its own output) and a
  **mathematically self-grounded** 10 dB level-gap → ~10 LU loudness-range
  check (two equal-duration constant-level segments 10 dB apart must differ
  by exactly 10 LU in gated loudness, independent of any external document),
  plus a 16 kHz-vs-44.1 kHz rate-invariance check and a constant-level
  near-zero-LRA check. All green on first run (no red→fix cycle needed; the
  arithmetic was verified by hand against the ITU constant before writing the
  assertion).
- **Real-model-free DSP parity against the pinned upstream script's own
  output: not delivered in this slice, honestly incomplete rather than
  faked.** Essentia's `LoudnessEBUR128` and the `ebur128` crate both claim
  full EBU R128/Tech 3341/3342 compliance (the crate's own README: "passes all
  tests defined in EBU Tech 3341 and EBU Tech 3342"), so a private
  Essentia-vs-`ebur128` parity fixture on the same fixture audio is the
  correct next check but requires installing Essentia 2.1-beta6-dev (already
  used for musicnn parity, D-151) and running `es.LoudnessEBUR128` on an
  identical decoded buffer — not done in this bounded tranche. **No test
  claims this parity exists or is ignored-without-panic**; it is simply not
  written yet. If/when added it must follow the same house rule as the
  musicnn private tests: `#[ignore]`, hard panic on missing reference env var,
  never a silent skip reading as a pass.
- **Key and tempo/BPM: explicit blocker, not implemented.** `docs/DIVERGENCES.md`
  D-008 (Essentia `RhythmExtractor2013(method="multifeature")` BPM +
  `KeyExtractor()` key vs. "Own Rust onset/autocorrelation tempo and chroma +
  Krumhansl key") is still status **`to-decide`**, not `accepted`, even though
  the parent design doc (`2026-10-16-kima-rust-design.md` lines 161-167)
  already names a candidate algorithm *family* for the Rust side ("Key: chroma
  plus Krumhansl–Schmuckler profiles"; "BPM: onset-strength envelope plus
  autocorrelation/comb tempo estimate"; non-goal list at line 33 explicitly
  rules out "Bit-exact parity with Essentia's BPM/key output"). That
  resolves *which standard to target* (an accuracy-threshold-gated own
  implementation, not Essentia replication) but **not the exact parameters
  needed to write an exact-output synthetic TDD fixture or a parity test**:
  no binding document pins a chroma frame/hop size, a specific Krumhansl(-
  Schmuckler vs. -Kessler vs. Temperley) profile table, an onset-strength
  window/hop, a tempo search range (BPM min/max), or a comb-filter
  resolution/number of candidates. The acceptance gate itself (key ≥70%
  relative-major/minor-accepted accuracy, tempo ≥85% within ±4% or
  double/half, measured against a labelled corpus not yet frozen) is a
  statistical accuracy test over an unspecified corpus, which cannot itself
  substitute for pinned per-call parameters in a deterministic synthetic
  unit test with exact expected outputs (e.g. "a 440 Hz pure tone must report
  key = A" requires knowing exactly which 12 chroma bins and which profile
  rotation/correlation method are compared — a reasonable implementer's guess
  here would be inventing the algorithm, which the task's grounding
  requirement explicitly forbids). Per the task's own stop rule, this is
  **not implemented in this slice**; it is deferred pending either (a) owner
  resolution of D-008's `to-decide` status plus exact chroma/profile/onset/
  tempo parameters, or (b) owner direction to instead implement an `aubio`
  FFI binding (GPL-3.0, compatible with this project's GPL-3.0-only license)
  per the design doc's conditional fallback, which has its own exact,
  citable algorithm (`aubio`'s own tempo/`aubio_tempo`
  implementation) and would not have this parameter-grounding gap. See
  `docs/DIVERGENCES.md` D-153.
- Dependency: `ebur128 = "0.1.11"` added to `crates/kima-ml/Cargo.toml`
  (`[dependencies]`, default build — not feature-gated, same as other
  core DSP/decode dependencies). Pure Rust (no bundled/linked C library;
  the optional `capi`/`cc` features that build a C-ABI shim are not enabled),
  MIT-licensed — compatible with this project's GPL-3.0-only license as a
  permissive dependency (MIT notice carried through in distribution, no
  copyleft conflict). No native linking, packaging or Docker changes needed
  (unlike the libsoxr/ORT native-library slices).
- **Verification (bounded, single tranche, well under limits):**
  `cargo test -p kima-ml --lib dsp::` → 9/9 ok, 0.04s. Full
  `cargo test -p kima-ml --all-targets` (no private/model env) → every test
  binary green, 0 failed, ~91s wall including cold `ebur128`/`bitflags`/
  `dasp_*` crate compilation; the previously-noted `clap_cold_start`
  libsoxr-race flake (unrelated, tracked in the skill's pitfalls) did not
  reproduce in this run. `cargo fmt --check -p kima-ml` and `cargo clippy -p
  kima-ml --all-targets -- -D warnings` both clean. `git status` confirms
  `cargo fmt -p kima-ml` touched only this slice's own new `dsp/` module;
  the already-dirty `clap.rs`, `musicnn.rs`, `pg_publish.rs` are byte-for-byte
  unchanged (untouched by the formatter), preserving the prior integration
  state exactly.
- **Changed/new files this round:** `crates/kima-ml/src/dsp/mod.rs` (new),
  `crates/kima-ml/src/lib.rs` (adds `pub mod dsp;`), `crates/kima-ml/Cargo.toml`
  (adds `ebur128 = "0.1.11"` to `[dependencies]`), this spec section,
  `docs/DIVERGENCES.md` D-153 (`to-decide` for key/tempo blocker; loudness
  side marked grounded/implemented within the same entry). No changes to
  `attempt.rs`, `stages/embedding.rs`, DB, scheduler, or the D-151/D-152
  musicnn compute/aggregate modules.
- **Not addressed by this slice (open):** key detection, tempo/BPM estimation
  (blocked per above — owner input needed on D-008 and exact parameters, or
  direction to use `aubio` FFI instead), real-model-free parity test of LUFS/LRA
  against a live Essentia `LoudnessEBUR128` run on the pinned upstream's own
  fixture audio (not written this round — correctly deferred, not faked),
  a stored analysis result type/column including DSP fields, GPU/server
  wiring, and any `attempt.rs`/scheduler/coordinator integration. No
  DB/persistence/scheduler/attempt runner code was touched.

## DB-free DSP key (tonality) and tempo (BPM) compute slice (2026-11-04, review-ready)

- **Unblocks the key/tempo blocker above.** Owner decision 2026-11-04 (quoted
  verbatim by the delegating task): own pure-Rust implementation, no C/FFI —
  chroma + Krumhansl–Schmuckler for key, onset-strength + autocorrelation/comb
  for tempo, explicitly rejecting `aubio` FFI. This resolves D-008's prior
  `to-decide` to **`accepted-with-params`**; the parameter-grounding gap the
  prior slice correctly stopped on is now closed (see full citation trail in
  `docs/DIVERGENCES.md` D-008). Scope: pure DSP compute only, DB-free, no
  persistence/scheduler/`attempt.rs` wiring — same house rule as the loudness
  slice. Prior `dsp::analyze_loudness` (this file's own prior section) is
  untouched beyond adding `pub mod key; pub mod tempo;` and re-exports to
  `dsp/mod.rs`'s existing doc comment area; `musicnn/compute.rs`/`aggregate.rs`
  (D-151/D-152) are untouched.
- **Grounding sources used (no parameter invented ad hoc):**
  - Chroma frame size, hop size, frequency band (25–3500 Hz) and tuning
    reference (A4=440 Hz) are the pinned upstream `analyzer.py`'s own
    `es.KeyExtractor()` call's effective parameters: plain default
    construction, no overriding kwargs, so Essentia's own declared defaults
    apply (`src/algorithms/extractor/keyextractor.h`, `MTG/essentia` commit
    `b9fa6cb674ca43dfb94d28d293aeda441c6745db`, inspected directly): frameSize
    4096, hopSize 4096, minFrequency 25.0, maxFrequency 3500.0, tuningFrequency
    440.0, windowType "hann" — the task's option (a), same effective
    parameters, different (own Rust) engine.
  - The 12-major/12-minor Krumhansl–Schmuckler profile table is the published
    Krumhansl, C. L., & Kessler, E. J. (1982) "Tracing the dynamic changes in
    perceived tonal organization in a spatial representation of musical keys",
    *Psychological Review*, 89(4), 334–368 probe-tone values — the task's
    option (b), cited directly, independently corroborated byte-identical
    against an unrelated open-source Rust MIR crate's own citation of the same
    source (`docs.rs/timestretch` `src/analysis/key.rs`, `KK_MAJOR`/`KK_MINOR`
    constants, found via web search, not copied from it).
  - Tempo search range (40–208 BPM) is the pinned upstream `analyzer.py`'s own
    `es.RhythmExtractor2013(method="multifeature")` call's effective
    `minTempo`/`maxTempo` defaults, inspected directly in
    `src/algorithms/rhythm/rhythmextractor2013.h` (same commit): `minTempo`
    default 40 (declared range [40,180]), `maxTempo` default 208 (declared
    range [60,250]) — option (a) again. `RhythmExtractor2013` is itself a
    composite multi-feature beat tracker (internal `BeatTrackerMultiFeature`
    network) with no single exposed onset-frame/hop parameter at its own level
    to carry over, so:
  - Onset-envelope FFT size (2048) and hop size (512) are
    `librosa.onset.onset_strength`/`librosa.stft`'s own published defaults
    (McFee, B., et al. (2015), "librosa: Audio and music signal analysis in
    python", *Proceedings of the 14th Python in Science Conference*) — option
    (b); at the project's 16 kHz DSP rate this is a 128 ms frame / 32 ms hop,
    inside the 10–50 ms onset-hop range surveyed by Bello, J. P., et al.
    (2005), "A tutorial on onset detection in music signals", *IEEE
    Transactions on Speech and Audio Processing*, 13(5).
  - Onset-strength function is rectified spectral flux (standard "spectral
    difference" function, Bello et al. 2005 §III-B; Dixon, S. (2006), "Onset
    detection revisited", *Proc. DAFx-06*).
  - The autocorrelation/"comb" tempo estimate and its harmonic/subharmonic
    disambiguation use Ellis, D. P. W. (2007), "Beat tracking by dynamic
    programming", *Journal of New Music Research*, 36(1), 51–60, eq. 5/6: raw
    onset-envelope autocorrelation at lag `tau` is multiplied by a log-Gaussian
    "tempo preference" window `W(tau) = exp(-0.5*(log2(tau/tau_0)/sigma_tau)^2)`
    using that paper's own MIREX-tuned values `tau_0=0.5s` (120 BPM) and
    `sigma_tau=1.4` octaves — found necessary (not optional polish) during
    TDD: plain unweighted autocorrelation on a synthetic click track locked
    onto the *third subharmonic* of the true period (picking the search
    range's own boundary lag) before this weighting was added; see "RED→GREEN"
    below. The comb-filter resolution is exactly one onset-envelope hop (512
    samples / 16 kHz = 32 ms), the same grounded hop, not a separately
    invented resolution.
- **New code:** `kima_ml::dsp::key::{analyze_key, KeyAnalysis, KeyMode,
  KeyError}` and `kima_ml::dsp::tempo::{analyze_tempo, TempoAnalysis,
  TempoError}` (`crates/kima-ml/src/dsp/{key,tempo}.rs`), both pure functions
  over `&[f32]` mono PCM + sample rate, consuming the same 16 kHz
  last-90-seconds shared-decode tail as `dsp::analyze_loudness`/musicnn (no
  resampling/tail-selection inside either function). `dsp/mod.rs` re-exports
  both and its module doc now points to them instead of describing a blocker.
  No new crate dependencies: `rustfft` (already a `kima-ml` dependency, used
  identically to `clap.rs`/`musicnn.rs`) supplies the FFT.
- **TDD, RED→GREEN, synthetic, no models needed (27 tests,
  `cargo test -p kima-ml --lib dsp::` → 27/27, 0.46s):**
  - Key (`dsp::key::tests`, 9 tests): error paths (empty/zero-rate/non-finite/
    too-short/digital-silence); a synthetic-fixture method that sums 12 sine
    waves, one per pitch class, each with amplitude `sqrt(profile_weight/10)`
    so the resulting chroma vector is, by construction, exactly proportional
    to the Krumhansl–Schmuckler profile for a chosen tonic/mode; checked for
    C major, A minor, and (sanity sweep) all 12 major tonics. All green on
    first run once a test-helper bug (see below) was fixed.
  - Tempo (`dsp::tempo::tests`, 10 tests): error paths (empty/zero-rate/
    non-finite/too-short/digital-silence); a synthetic click-track fixture
    (multi-harmonic 30 ms decaying burst, once per beat period) at 60, 90,
    120 and 150 BPM, checked within the binding spec's own stated tempo
    tolerance ("tempo ... within ±4% or double/half",
    `2026-11-03-m4-ml-analysis-design.md` line 37 — this document, the one
    being edited, cited as the binding source rather than re-deriving a
    tolerance); a constant-pure-tone case asserting the function never
    panics or returns a non-finite/out-of-range result (loosened from an
    originally-attempted "must read as low confidence" assertion once that
    turned out to require an ungrounded threshold — documented in-code as a
    known limitation, not silently special-cased).
  - **RED→GREEN evidence (genuine bugs found and fixed, not asserted-after
    the fact):** (1) first test run: key detection returned the wrong tonic
    for every case (e.g. tonic 0 detected as pitch class 3) — root cause was
    an inconsistent sign/octave convention between the test's own
    `freq_for_pc` helper and the production `pitch_class` inverse mapping
    (test-only bug, fixed in the test helper, not production code — verified
    by hand-deriving the correct semitone arithmetic before the fix). (2)
    first test run: a 120 BPM synthetic click track measured as ~39.9 BPM
    (the search range's slowest boundary, three times too slow) — root cause
    was genuine: plain mean-normalized autocorrelation of a sparse periodic
    click train has comparably strong (here, even stronger) peaks at integer
    multiples of the true period, and an un-weighted search over the full
    40–208 BPM range picked one; debugged by a temporary instrumented test
    printing every candidate lag's raw correlation (removed before this
    report), which showed near-equal peaks at the true period and at 2x/3x
    it. Fixed in production code by adding the Ellis (2007) tempo-preference
    weighting described above — not a test-only fix, and not a threshold
    tuned to make this one case pass: the paper's own published center/width
    values were used unchanged. (3) first test run: `digital_silence`/
    `constant_tone` tempo tests used too short a fixture duration relative to
    the configured max search lag (47 frames), tripping
    `InsufficientOnsetFrames` instead of exercising the intended `NoOnsetEnergy`/
    confidence path — fixed by lengthening the synthetic fixtures, not by
    relaxing the production bounds check.
- **No real-model/upstream-output parity gate for key/tempo in this slice**
  (honestly not attempted, not silently skipped): no Essentia
  `KeyExtractor`/`RhythmExtractor2013` reference run on shared fixture audio
  exists yet, and the spec's own accuracy gate ("key ≥70% relative
  major/minor accepted, tempo ≥85% within ±4% or double/half") is a
  statistical corpus evaluation against a frozen reference corpus that is a
  separate, not-yet-done task, not substitutable by this slice's
  unit-level synthetic fixtures. The stale clause immediately following that
  gate in this document ("If tempo fails, §6 requires aubio FFI and another
  run") is explicitly superseded by the 2026-11-04 owner decision quoted
  above and must not be read as still mandating `aubio`.
- **Verification (bounded, two tranches, well under the 300s/420s limits):**
  tranche 1 — `cargo build -p kima-ml --lib` (6.4s) then
  `cargo test -p kima-ml --lib dsp::` RED (6 failures, see above) → fixes →
  GREEN 27/27 in 0.46s; `cargo fmt -p kima-ml` (applied cleanly, re-checked
  clean) and `cargo clippy -p kima-ml --all-targets -- -D warnings` (one
  `needless_range_loop` finding in the new `key.rs`, fixed, then clean).
  tranche 2 — full `cargo test -p kima-ml --all-targets --no-fail-fast` with
  `ORT_DYLIB_PATH` set to the trusted provisioned runtime
  (`~/.hermes/cache/scratch/m4-ort-trusted/libonnxruntime.so.1.22.0`): 89
  lib tests (includes the 27 new dsp tests) + all 24 integration test
  binaries green, 0 failed; the previously-documented `clap_cold_start`
  libsoxr FFT4G/OpenMP race flake (unrelated pitfall) reproduced once on a
  first `--all-targets` run without `--no-fail-fast` and went green on
  immediate rerun with `--no-fail-fast`, consistent with its known
  intermittent nature, not caused by this change (`git status` confirms no
  file under its own test/source area was touched this round). `git status`
  also confirms `cargo fmt -p kima-ml` touched only this slice's new
  `dsp/key.rs`/`dsp/tempo.rs` and the `dsp/mod.rs` doc/re-export edit; the
  already-dirty `clap.rs`, `musicnn.rs`, `lib.rs`, `pg_publish.rs`,
  `Cargo.toml` are unchanged by this slice.
- **Changed/new files this round:** `crates/kima-ml/src/dsp/key.rs` (new),
  `crates/kima-ml/src/dsp/tempo.rs` (new), `crates/kima-ml/src/dsp/mod.rs`
  (doc comment + `pub mod key;`/`pub mod tempo;` + re-exports only — the
  loudness code/tests from the prior slice are byte-for-byte untouched), this
  spec section, `docs/DIVERGENCES.md` D-008 (status changed from `to-decide`
  to `accepted-with-params`, append-only — no other row's text or status
  changed, D-153 left as-is since it is itself append-only history of the
  prior slice's blocker). No changes to `attempt.rs`, `stages/embedding.rs`,
  DB, scheduler, `musicnn/compute.rs`, `musicnn/aggregate.rs`, or any other
  file.
- **Not addressed by this slice (open):** real-model/Essentia-reference
  parity evidence for key/tempo (not attempted, see above), the frozen
  labelled-corpus statistical accuracy evaluation the binding spec itself
  requires before key/tempo can be considered fully gated, a stored analysis
  result type/column including key/tempo fields, GPU/server wiring, and any
  `attempt.rs`/scheduler/coordinator integration (explicitly out of scope per
  the delegating task). No DB/persistence/scheduler/attempt-runner code was
  touched. No commit/push/merge/cleanup performed.

## Stored MusiCNN + DSP analysis-result schema/migration slice (2026-11-04, review-ready)

- Scope: standalone, testable `track_analysis` DB schema and migration
  (`crates/kima-db/migrations/0026_track_analysis.sql`,
  `kima_db::analysis` in `crates/kima-db/src/analysis.rs`) for the
  already-accepted MusiCNN aggregate (D-151/D-152) and DSP loudness/key/tempo
  (D-008 accepted-with-params, D-153) compute contracts. No
  `attempt.rs`/scheduler/coordinator wiring, no real-model/Essentia parity
  gate (both explicitly out of scope, separate tasks). `kima-db` gains no
  new dependency on `kima-ml`; result fields are redeclared locally
  (`MusicnnAnalysis`, `LoudnessAnalysis`, `KeyAnalysis`/`KeyMode`,
  `TempoAnalysis`), matching `ml::Provenance`'s existing convention of not
  importing a `kima-ml` type into the DB crate.
- Design, grounded in the existing `track_embeddings`/0024 pattern (same
  `track_id` PK referencing `tracks(id) ON DELETE CASCADE`, same
  `file_generation`/`ml_generation` fencing, same `invalidate_track_ml()`
  trigger deleting the whole row on a tracked identity change) but adapted
  for **four independently-completable results sharing one row**, since
  this task's own requirement ("DSP key/tempo might fail independently from
  musicnn") generalizes to all four groups potentially succeeding/failing
  independently within the same shared-decode attempt (D-009): MusiCNN
  aggregate, DSP loudness (LUFS/LRA), DSP key (tonality), DSP tempo (BPM).
  Each group gets its own nullable column set, its own `*_pipeline_version`
  and its own `*_computed_at`, enforced "all columns in the group are set,
  or none are" by one CHECK constraint per group — a half-populated group is
  a database-level constraint violation, not just an application bug class.
  `file_generation` is one column for the whole row (not per group):
  all four results come from one shared decode of the same file bytes, so
  they share one fence; `invalidate_track_ml()` (0024's function, replaced
  here via `CREATE OR REPLACE`) now also issues `DELETE FROM track_analysis
  WHERE track_id = OLD.id` alongside its existing `track_embeddings`
  deletion, so a row can never mix columns from two generations — a write
  for a new generation always starts from no row, never from stale leftover
  groups from an older one.
- `kima_db::analysis` exposes one `store_*`/read pair per group
  (`store_musicnn`/`store_loudness`/`store_key`/`store_tempo`, and a single
  `analysis()` read returning all four as `Option<(pipeline_version,
  Group)>`), each independently fenced on `tracks.ml_generation` under
  `FOR UPDATE` before writing (same generation-fencing idiom as
  `ml::complete_embedding`, without borrowing its `track_stages`
  claim/lease semantics, since no claim workflow exists yet for the
  'analysis' stage — that is exactly the attempt.rs/scheduler wiring this
  slice does not add). A caller gets back each group's own
  `pipeline_version` string so it can still reject results from a pipeline
  version it no longer trusts (same model-mixing-prevention intent as
  `ml::embedding`'s exact-provenance match), without this module hard-coding
  that policy itself.
- TDD (`crates/kima-db/tests/analysis.rs`, 11 tests, isolated run-owned
  Docker Postgres): table/PK/index existence; absent row reads as `None`
  (never a fabricated default); independent store/read per group; the
  explicit required scenario — tempo not stored leaves musicnn/loudness/key
  intact; stale-generation write rejected with zero partial mutation;
  file-identity change deletes the whole row (not just one group) and still
  resets the pre-existing `track_stages` 'analysis' entry to `pending`
  (0024's unchanged stage-reset list already includes 'analysis'); a later
  generation's write never leaks a stale generation's groups; `ON DELETE
  CASCADE` from `tracks`; half-populated groups rejected by raw SQL
  (bypassing this module's own write path, so the guarantee holds against a
  future caller bug too); out-of-range values (`key_mode` not
  major/minor, `key_tonic` outside 0-11, `tempo_bpm` <=0 or >400,
  `musicnn_danceability` outside [0,1]) rejected by CHECK constraints; and
  an upgrade test from the pre-0026 migration set that inserts
  `track_embeddings`/`track_stages` rows first, then runs 0026 and confirms
  both the new table appears and the pre-existing rows/trigger behavior are
  untouched (`sqlx::test(migrations = false)` plus a version-filtered
  `Migrator`, the same idiom `tests/ml.rs`'s
  `upgrade_from_m3_preserves_existing_track_and_ledger` already uses).
  "Backward semantics (if applicable)": this project's migration convention
  is forward-only (no `.down.sql` files exist anywhere in
  `crates/kima-db/migrations/`), confirmed by inspection before writing this
  test; "backward" here means verifying the *upgrade path* preserves
  pre-existing data, not a reverse migration, which this codebase does not
  have.
- Constraint-removal mutation check (RED, not kept in the suite): dropping
  `track_analysis_musicnn_all_or_nothing` via raw `ALTER TABLE ... DROP
  CONSTRAINT` on a live instance of the migrated schema let an otherwise
  identical half-populated `INSERT` succeed; re-adding the constraint
  restored the rejection. Confirms the CHECK constraint is what the test
  exercises, not a vacuous assertion.
- Verification (bounded tranches, isolated run-owned Docker
  `pgvector/pgvector:pg17`, container `kima-m4-analysis-schema-1793823874`,
  localhost-only port 32815, waited for the published host connection to
  answer `SELECT current_database()` before use per the known
  initdb-temporary-server pitfall): `cargo test -p kima-db --test analysis`
  11/11 green, 4.68s; full `cargo test -p kima-db --all-targets`
  (`SQLX_OFFLINE=true`, regenerated workspace cache) 205/205 green across 17
  binaries, ~93-100s (two separate full runs, both green, both well inside
  the 300s/tranche bound). `cargo fmt -p kima-db` touched only this slice's
  new files (`git diff --stat` confirms the five pre-existing dirty
  `kima-db` files gained zero additional diff from the fmt run). `cargo
  clippy -p kima-db --all-targets -- -D warnings` clean. Root `.sqlx`
  regenerated via `cargo sqlx prepare --workspace -- --all-targets` across
  the whole workspace (not scoped to one crate — an initial scoped
  `-p kima-db` run incorrectly pruned other crates' cached queries from
  `.sqlx`, caught before use and reverted with `git checkout -- .sqlx`
  before redoing it workspace-wide); final diff is the same pre-existing 3
  deleted/2 added baseline plus 4 new query files for this slice's new
  queries (6 new total), 284 files, matching the prior round's own
  "280 files" baseline plus this slice's additions.
- Preflight before using Docker: inventoried existing containers (dev
  `m0-skeleton-postgres-1` on port 5456 confirmed untouched throughout —
  read-only presence check only, no writes), confirmed no name/port
  collision with the new run-owned container, and confirmed no active
  writer (no live `cargo`/`rustc` process, `git status`/`git log -1`
  matched the expected dirty HEAD `226b607` before any edit).
- Not addressed by this slice (open, explicitly out of scope per the
  delegating task): `attempt.rs`/scheduler/coordinator wiring to actually
  call `store_musicnn`/`store_loudness`/`store_key`/`store_tempo` from a
  real analysis attempt; any claim/lease workflow for the 'analysis' stage
  (today only 'embedding' has one, in `kima_db::ml`); real-model/Essentia
  parity gate for key/tempo (D-008/D-153's own already-stated open item,
  unchanged by this slice); mood-tag generation and genre (both need
  `bpm`/`keyScale`, now representable via this schema, but generating them
  is not part of this slice); API/route exposure of stored analysis
  results. No commit/push/merge/broad cleanup/model distribution performed.

## Analysis-stage publish adapter slice (2026-11-04, review-ready)

- Scope: exactly the delegated bridge — wire the already-accepted pure
  compute functions (`kima_ml::musicnn::{MusicnnCompute, MusicnnAggregate}`
  via `musicnn::{aggregate, aggregate_patches}`; `kima_ml::dsp::{analyze_loudness,
  key::analyze_key, tempo::analyze_tempo}`) into `kima_db::analysis`'s
  `store_musicnn`/`store_loudness`/`store_key`/`store_tempo` (D-154,
  reviewed clean, schema itself untouched) through a new
  `crates/kima-ml/src/analysis_publish.rs` module, gated behind the
  existing `pg-publish` feature (off by default, same as `publish.rs`).
  Explicitly out of scope and not added here, per the delegating task:
  any claim/lease/scheduler workflow for the 'analysis' stage (today only
  'embedding' has one, in `kima_db::ml`) and any `attempt.rs` change — both
  remain the *next* slice. `attempt.rs`, `stages/embedding.rs`,
  `musicnn/compute.rs`, `musicnn/aggregate.rs`, `dsp/{mod,key,tempo}.rs`
  and the 0026 migration/`analysis.rs` are all unchanged by this slice.
- Design, mirroring `publish.rs`'s existing fenced async wrapper +
  `spawn_blocking` bridge pattern (not its claim-specific `EmbeddingClaim`
  shape, since no claim workflow exists for 'analysis' yet): `publish_musicnn`
  / `publish_loudness` / `publish_key` / `publish_tempo` each (1) convert a
  `kima-ml` compute-result type to `kima_db::analysis`'s locally-redeclared
  type via a small 1:1 field-copy function (`to_db_musicnn`/`to_db_loudness`/
  `to_db_key`/`to_db_tempo` — all fields match 1:1 except
  `dsp::key::KeyAnalysis::tonic: u8` widening losslessly to the DB struct's
  `tonic: i16`, and each `KeyMode` enum mapped variant-for-variant), (2)
  call the matching `store_*`, and (3) map its `false` return (track moved
  past the caller's `file_generation`: changed file, missing, or deleted)
  to `AnalysisPublishError::Rejected` — the same "rejected storage claim
  must become callback failure" mapping `publish.rs::classify` already
  uses for CLAP, generalized to four independent calls instead of one.
  `AnalysisGroupInputs` holds each group as `Option<(pipeline_version,
  &result)>` so a group the caller never computed (model unavailable, DSP
  failed, or simply not attempted yet) is representable without a
  fabricated error. `publish_track_analysis`/`_blocking` call all four
  present groups **unconditionally, in sequence, with no `?` crossing a
  group boundary** — this is the actual mechanism satisfying the task's
  "each group must store/fail independently; one group's failure must not
  block storing the others" requirement: a `Storage`/`Rejected` error from
  one group's `store_*` call is captured in that group's own
  `Option<Result<_, _>>` slot in `AnalysisPublishReport` and the function
  proceeds to the next group regardless, exactly mirroring the
  independently-nullable, independently-fenced design of migration 0026
  itself (D-154) and `kima_db::analysis`'s own per-group `fence_generation`
  calls (one `FOR UPDATE` check per `store_*`, not one shared transaction
  across groups — confirmed by reading `analysis.rs` before writing this
  adapter, not assumed).
- Why no scheduler/claim layer here: the schema slice's own "not addressed"
  list (above) already named this exact boundary — "no claim/lease
  workflow for the 'analysis' stage" — as the next, separate slice; this
  bridge's functions take `track_id`/`file_generation` directly (matching
  `store_*`'s own signature), the same shape `store_*` itself already
  exposes, so adding a claim type here would be inventing scheduler scope
  the delegating task explicitly reserves for later, not filling a gap in
  this one.
- TDD (`crates/kima-ml/tests/analysis_publish.rs`, isolated run-owned
  Docker Postgres, migrator `kima_db::MIGRATOR` so 0026 applies): 6
  integration tests — all four groups published and read back via
  `kima_db::analysis::analysis()` matching the conversion functions'
  output; an absent group (`None` in `AnalysisGroupInputs`) is never
  attempted while sibling groups still publish in the same call; the
  delegating task's explicit required scenario — one group's direct-adapter
  call rejected for a stale `file_generation` does not block a sibling
  group's `store_*` call made afterwards in the same test, with a final
  `SELECT count(*) FROM track_analysis` confirming exactly one row with
  only the successful group's columns populated; two further direct-adapter
  rejection-without-mutation checks (key, tempo) confirming a rejected
  write leaves no row at all; and the `spawn_blocking` bridge publishing
  one group from a dedicated blocking task and reading it back on the
  calling task's connection. Plus 5 synthetic `#[cfg(test)]` unit tests in
  the module itself (`classify`'s two branches; `to_db_musicnn` field-copy;
  `to_db_key`'s `u8`→`i16` tonic widening and mode mapping;
  `AnalysisGroupInputs::default()`'s all-absent shape) exercised without a
  database. RED confirmed before writing the adapter: the test file was
  written first and failed to compile (`unresolved import
  kima_ml::analysis_publish`, plus type-mismatch errors from comparing
  `kima-ml` result types directly against `kima_db::analysis` row types)
  until the module and its `to_db_*` conversion helpers existed; GREEN on
  the first full implementation attempt, 6/6 and 5/5.
- Preflight before using Docker: reused the still-Up, still localhost-only
  `kima-m4-analysis-schema-1793823874` container (port 32815) left running
  from the schema slice earlier the same day in this same worktree (no new
  container created for this slice); re-verified identity before any test
  run (`SELECT current_database(), inet_server_port()` → `kima`/5432-in-
  container-mapped-to-32815-on-host, confirming this is not the dev
  `m0-skeleton-postgres-1`/5456 cluster) and confirmed no leftover
  `_sqlx_test_*`/`kima_e2e_*` databases before the first `#[sqlx::test]`
  run. Confirmed no active writer before any edit: no live `cargo`/`rustc`
  process for this worktree, no `fuser` lock on `Cargo.lock`, and the
  worktree's dirty files' mtimes predated this session's start with no
  concurrent process touching them.
- Verification (bounded tranches, all green): `cargo test -p kima-ml
  --features pg-publish --test analysis_publish -- --test-threads=1` 6/6,
  6.15s. Full `cargo test -p kima-ml --all-targets --features pg-publish
  --no-fail-fast` (`ORT_DYLIB_PATH` set to the trusted runtime) — every
  binary green, 0 failed (97 lib tests including the module's own 5;
  `analysis_publish.rs`'s 6; `pg_publish.rs`'s pre-existing 4; every other
  `kima-ml` integration binary unaffected), ~97s, well inside the 300s/
  tranche and 420s total bounds; no `clap_cold_start` flake reproduced in
  this run. `cargo fmt -p kima-ml --check` and `cargo fmt -p kima-ml`
  (idempotent, zero diff) clean; `git diff --stat` on the three
  already-dirty files fmt also reformats (`clap.rs`, `musicnn.rs`,
  `tests/pg_publish.rs`) confirms this run added zero additional diff to
  them beyond their pre-existing, already-dirty content (inspected
  line-by-line: the existing diff is the boxed-mel-filter fix, the
  `mod aggregate; mod compute;` declarations, and the pre-existing
  `pg_publish.rs` stage-source-not-model-id fix — none newly introduced by
  this slice). `cargo clippy -p kima-ml --all-targets --features
  pg-publish -- -D warnings` clean. No `kima-db` files touched (`git diff
  --stat crates/kima-db/` shows only the pre-existing dirty files from
  earlier slices, unchanged by this one); no new `.sqlx` entries needed
  (this module calls existing `kima_db::analysis::store_*`/`analysis()`
  functions and issues no new `sqlx::query!`/`query_as!` macro calls of its
  own; its tests use non-macro `sqlx::query`/`query_scalar`, matching
  `tests/analysis.rs`/`tests/pg_publish.rs`'s own convention, so no
  compile-time query cache entry is required).
- Changed/new files this round: `crates/kima-ml/src/analysis_publish.rs`
  (new), `crates/kima-ml/tests/analysis_publish.rs` (new),
  `crates/kima-ml/src/lib.rs` (+`#[cfg(feature = "pg-publish")] pub mod
  analysis_publish;`), `crates/kima-ml/Cargo.toml` (+optional `uuid`
  dependency, added to the `pg-publish` feature list — `kima_db::analysis`'s
  `store_*`/`analysis()` take `uuid::Uuid` directly, unlike `publish.rs`
  which only ever receives a `Uuid` already wrapped inside
  `kima_db::ml::EmbeddingClaim` and so never needed the dependency itself),
  this spec section, `docs/DIVERGENCES.md` D-155 (accepted, append-only,
  D-154 and earlier rows unchanged).
- Not addressed by this slice (open, explicitly out of scope per the
  delegating task): any claim/lease/scheduler workflow for the 'analysis'
  stage (the explicitly-named next slice); `attempt.rs` wiring to actually
  call `publish_track_analysis`/`_blocking` from a real shared-decode
  attempt; real-model/Essentia parity for key/tempo (D-008/D-153's own
  already-open item, unchanged); mood-tag/genre generation; API/route
  exposure of stored analysis results; independent review of this round's
  diff; commit (push only with approval); post-M4 container/resource
  cleanup (the reused `kima-m4-analysis-schema-1793823874` container
  remains Up, as it already was before this slice).



## Analysis-stage claim/lease scheduler slice (2026-11-04)

Implements the claim/lease DB layer that a caller of
`kima_ml::analysis_publish::publish_track_analysis` must already own,
per that module's own doc comment, mirroring the existing CLAP
embedding claim/lease pattern exactly (D-146 precedent) rather than
inventing a parallel mechanism. See D-156 in `docs/DIVERGENCES.md` for
the full rationale, grounding and test inventory.

- **Stage identity, grounded not guessed:** 'analysis' is a new value
  of the *same* generic `track_stages.stage` column/machinery, not a
  new table. The CHECK constraint has listed `'analysis'` since the
  original schema (`0018_track_stages.sql`), and `invalidate_track_ml()`
  (0024/0026) already resets it alongside `'embedding'`/`'similarity'`.
- `kima_db::stages::Stage::Analysis` added; claimed unconditionally like
  `Fingerprint`/`Embedding` (no SQL change needed — `stages::claim` is
  already stage-parametrized). `stages::report_failure`'s
  embedding-only lock-row-before-evaluating-the-lease branch (the
  project's documented lease-fencing pitfall) widened to also cover
  `Stage::Analysis`, since `analysis_publish`'s four independent
  `store_*` calls have the same kind of out-of-transaction publication
  gap as `ml::complete_embedding`'s single call.
- New sibling module `kima_db::analysis_claim`
  (`crates/kima-db/src/analysis_claim.rs`, not added into `kima_db::ml`
  since that module's own doc comment scopes it to CLAP storage):
  `AnalysisClaim{track_id, started_at, file_generation}` plus
  `analysis_claim`/`analysis_claim_for_worker`/`analysis_input_for_worker`,
  mechanically mirroring `ml::EmbeddingClaim` and its three claim
  functions (same worker-token binding via `(track_id, started_at)`,
  same `next_attempt_at > clock_timestamp()` lease check).
- Deliberately stops at the claim/lease snapshot boundary: no
  `complete_analysis` write-side counterpart to `ml::complete_embedding`.
  `analysis_publish`'s `store_*` calls already fence independently
  against `tracks.ml_generation` (not this ledger row); how/when the
  `'analysis'` `track_stages` row itself transitions to `done` given
  four independently-succeeding/failing/absent groups is a
  scheduling-policy decision left to a future `attempt.rs` slice.
  Inventing that policy here was explicitly out of scope and would
  have been guessing, not recovering, a decision.
- TDD on a fresh run-owned isolated Docker Postgres (preflight identity
  + empty-schema check, migrations applied via `sqlx migrate run`,
  never dev5456): `tests/stages.rs` +2 tests (unconditional claim,
  lease/redelivery parity with embedding); new
  `tests/analysis_failure_fence.rs` (4 tests, 1:1 mirror of
  `embedding_failure_fence.rs`: expiry without redelivery rejected,
  expiry-while-waiting-for-the-stage-lock rejected via
  `pg_stat_activity` lock-wait detection plus clock-timestamp polling,
  stolen/redelivered token rejected while the current token can still
  report, and a pin that fingerprint/recording did not get the new
  branch by accident); new `tests/analysis_claim.rs` (5 tests: no
  claim before scheduling, worker-token binds to the exact held claim
  and rejects a wrong token, path+generation pairing across
  invalidation mirroring `ml.rs`'s embedding test, lapsed lease is not
  a live claim through any of the three functions, redelivery
  invalidates the old token).
- Verification (bounded tranche, <<300s): `cargo test -p kima-db --test
  analysis_claim --test analysis_failure_fence --test stages` 27/27 ok
  (~16s). Full `cargo test -p kima-db --all-targets` (SQLX_OFFLINE=true)
  216/216 ok, reconciled against `-- --list` (216 = prior 205 + 5 + 4 +
  2 new). `cargo sqlx prepare --workspace -- --all-targets` added
  exactly one new query cache file (the new `query_as!` in
  `analysis_claim.rs`); the pre-existing dirty-worktree `.sqlx` diff
  (3 deleted/6 untracked) was unchanged otherwise. `cargo fmt -p
  kima-db --check` and `cargo clippy -p kima-db --all-targets -- -D
  warnings` clean. `cargo check --workspace --all-targets`
  (SQLX_OFFLINE=true, does not link) clean — confirms no downstream
  compile breakage in dependent crates.
- **Unrelated environment incident, reported honestly:** `cargo build
  --workspace --all-targets` (full link) crashed on several unrelated
  test binaries with linker `Bus error`s. Root cause: the host disk was
  at 100% (1.7G free of 845G), not this diff — confirmed by the clean
  non-linking `cargo check` of the exact same targets. No disk cleanup
  was performed (outside this task's authorization).
- Changed/new files this round: `crates/kima-db/src/analysis_claim.rs`
  (new), `crates/kima-db/tests/analysis_claim.rs` (new),
  `crates/kima-db/tests/analysis_failure_fence.rs` (new),
  `crates/kima-db/src/stages.rs` (`Stage::Analysis` variant +
  `report_failure` branch), `crates/kima-db/src/lib.rs` (+`pub mod
  analysis_claim;`), `crates/kima-db/tests/stages.rs` (+2 tests), root
  `.sqlx` (+1 new query file), this spec section, `docs/DIVERGENCES.md`
  D-156 (accepted, append-only, D-155 and earlier rows unchanged). No
  changes to `kima_db::ml`, `kima_db::analysis`,
  `kima_ml::analysis_publish`, migrations 0025/0026, or any
  attempt.rs/scheduler/coordinator code.
- Not addressed by this slice (open, explicitly out of scope per the
  delegating task): a `complete_analysis`-style stage-completion
  function and its scheduling policy (next slice's decision);
  `attempt.rs` wiring connecting `analysis_claim`'s functions to
  `analysis_publish::publish_track_analysis`; independent review of
  this round's diff; commit (push only with approval); the disk-space
  environment blocker noted above; post-M4 container cleanup (the new
  `kima-m4-analysis-claim-1793826113` container remains Up, localhost-only
  port 32818).

## Analysis-stage completion policy + attempt.rs per-group capability
## degradation (2026-11-04)

Implements the two items the previous slice left explicitly open: the
`'analysis'`-stage completion policy, and `attempt.rs` wiring that lets
a future production caller degrade MusiCNN independently of DSP. See
D-157 in `docs/DIVERGENCES.md` for the full rationale, grounding and
test inventory; this section is a short pointer.

- **Scope discipline:** only `crates/kima-ml/src/attempt.rs` and its
  test file changed. `analysis_claim.rs`, `analysis_publish.rs`,
  `analysis.rs` (`kima_db::analysis`) and `stages.rs` — all reviewed
  clean across D-150–D-156 — are untouched; this round's own full
  suite run confirms `analysis_publish.rs`'s 6 tests are still green
  unmodified.
- **Per-group capability degradation:** `run_attempt`'s `Combined`
  branch previously blocked the *entire* analysis stage (no decode)
  whenever either `Capability::MusicnnAnalysis` or `Capability::Dsp`
  was unavailable — a structural violation of independent degradation,
  since DSP's three groups carry no model file and MusiCNN's absence
  should only drop its own group. It now blocks only when *both* are
  unavailable; otherwise it decodes and calls `analyze_and_publish`
  with a new `AnalysisAvailability{musicnn,dsp}` telling the callback
  which model-backed group it may skip, mirroring the existing
  CLAP-unavailable/`DISABLE_CLAP` pattern already used for the
  embedding stage.
- **Stage completion policy, grounded not guessed:** new
  `analysis_groups_resolved<E>(groups: [Option<&Result<(), E>>; 4]) ->
  bool`, pure and DB-free (generic over `E` so the default,
  non-`pg-publish` build of `attempt.rs` stays DB-free). The policy —
  `'analysis'` reaches `done` once every group has *some* conclusion
  (published, or never attempted for any reason short of a storage
  rejection), not requiring all four to succeed — follows directly from
  `track_analysis`'s own per-group independence (D-154) and the
  binding spec's explicit "DSP ... must not falsely complete the
  combined analysis stage" language (misread naively as requiring
  *more* than per-group independence, but in context about capability
  status reporting, not blocking DB completion on MusiCNN). The one
  shape treated as unresolved is a group whose storage write was
  itself rejected: that invalidates the whole attempt's `file_
  generation`, not just one group, so the caller must retry the whole
  attempt rather than complete it with partial data from a stale
  generation. The exact wiring of this function into a real
  `track_stages` `report_done`/`report_failure` call is left to the
  still-unwritten production `'analysis'`-stage coordinator (a
  `kima-ident` module analogous to `stages/embedding.rs`) — out of
  scope for this slice, which only had to define and implement the
  *policy function*, not build the coordinator that calls it.
- TDD: `attempt.rs`'s own unit test module gained 6 tests for
  `analysis_groups_resolved` (all-absent resolved; all-published
  resolved; MusiCNN+loudness-only-present resolved, directly exercising
  the task's own grounding example; a non-rejection absent group
  resolved; any rejected group not resolved; a single rejected group
  among absent ones not resolved). `tests/attempt.rs` (mechanically
  updated throughout for the new two-argument `analyze_and_publish`
  signature) gained `missing_musicnn_alone_still_decodes_and_degrades_
  only_that_group` and `missing_dsp_alone_still_decodes_and_degrades_
  only_that_group`, and had the old `unavailable_analysis_is_not_
  false_success_and_does_not_decode` test (which asserted the now-
  incorrect "blocked if either is missing" behavior) replaced with
  `both_analysis_capabilities_missing_blocks_and_does_not_decode`.
- Verification (bounded tranches, fresh run-owned isolated Docker
  Postgres `kima-m4-attempt-completion-1793829256`, pgvector/pgvector
  :pg17, localhost-only port 20131, preflight identity + empty-schema
  check before `sqlx migrate run` applied all 26 migrations cleanly,
  never dev5456): `cargo test -p kima-ml --test attempt` 12/12
  (0.27s, `ORT_DYLIB_PATH` set); `cargo test -p kima-ml --lib
  attempt::` 6/6; `cargo test -p kima-ml --features pg-publish --test
  analysis_publish -- --test-threads=1` 6/6 (8.15s); full `cargo test
  -p kima-ml --all-targets --features pg-publish --no-fail-fast` 0
  failed (~102s, one tranche, inside the 300s/420s bounds). `cargo fmt
  -p kima-ml --check` clean (git diff confirms fmt touched only this
  round's files, not the pre-existing dirty `clap.rs`/`musicnn.rs`/
  `tests/pg_publish.rs`); `cargo clippy -p kima-ml --all-targets
  [--features pg-publish] -- -D warnings` clean; `cargo check
  --workspace --all-targets` (SQLX_OFFLINE=true) clean. The documented
  flaky `clap_cold_start` test reproduced once on the full run and
  passed immediately on an isolated rerun — confirmed unrelated
  (`clap.rs` untouched this round).
- Changed files this round: `crates/kima-ml/src/attempt.rs`
  (`AnalysisAvailability`, `analysis_groups_resolved`, `Combined`
  branch gating, `analyze_and_publish` signature, 6 new unit tests),
  `crates/kima-ml/tests/attempt.rs` (signature migration throughout, 1
  test replaced, 2 new tests), `docs/DIVERGENCES.md` D-157 (accepted,
  append-only), this spec section. No changes anywhere else.
- Not addressed by this slice (open, explicitly out of scope): a real
  production `'analysis'`-stage coordinator (`kima-ident` module
  analogous to `stages/embedding.rs`) that actually calls
  `analysis_claim`, runs `dsp::{analyze_loudness,key,tempo}` and
  `musicnn::{compute,aggregate}`, calls `analysis_publish::
  publish_track_analysis`, and uses `analysis_groups_resolved` to
  decide `stages::report_done` vs `report_failure`; real-model/
  Essentia parity for key/tempo (D-008/D-153's own open item, unrelated
  to this slice); independent review of this round's diff; commit
  (push only with approval); post-M4 cleanup of the new
  `kima-m4-attempt-completion-1793829256` container (left Up,
  localhost-only, port 20131, alongside the other tracked leftover
  test containers from earlier slices).

## Analysis-stage production coordinator + MusiCNN runtime caching fix slice (2026-11-05, uncommitted review candidate)

Closes the exact gap the prior slice named: a real `kima-ident` `'analysis'`-stage
coordinator (`crates/kima-ident/src/stages/analysis.rs`, already present in the
dirty worktree before this round in a form with two bugs, fixed here) that wires
`analysis_claim` (D-156), the pure `dsp::*`/`musicnn::{compute,aggregate}`
functions (D-008/D-151/D-152/D-153), `analysis_publish` (D-155) and the
completion policy `analysis_groups_resolved` (D-157) into one `run_batch`/
`run_claim` pair, mirroring `stages::embedding`'s existing shape exactly
(shared `compute_then_finish`, `StageFailure`/`record_for`/`closed` reuse,
`Stage::Analysis` claim → decode-via-snapshot-store → publish →
`report_done`/`report_failure`).

- **Cosmetic cleanup (part 1):** removed the unused `MusicnnAggregate`,
  `uuid::Uuid` and `VerifiedBytes` imports (each shadowed by an already-used
  fully-qualified path elsewhere in the same file), the unused `use super::*`
  in the `#[cfg(test)] mod tests` block, and the unnecessary `mut` on the
  single-call `open` closure. `SQLX_OFFLINE=true cargo check --workspace
  --all-targets` stays clean (0 errors) after the fix; the only remaining
  warning is the pre-existing, out-of-scope `AnalysisRuntimeState::workers`
  dead field (a semaphore-caching improvement not requested by this task).
- **Correctness/efficiency fix (part 2, the reason for this slice):** the
  handed-off `load_musicnn_compute` reloaded and re-verified all 11 pinned
  MusiCNN ONNX artifacts from disk on every claimed track, unlike
  `stages::embedding::runtime_now`'s cache-once-per-process pattern. Worse,
  its two call sites disagreed on `model_dir`: `analysis_availability`
  correctly passed `ctx.config.analysis.model_dir`, but `compute_and_publish`'s
  per-track call passed `path.parent().unwrap_or(root)` — the *track's own
  file path's parent directory*, not the configured model directory — a
  latent bug independent of the caching problem. Both are fixed together:
  - `ctx.rs`'s `AnalysisRuntimeState.runtime` field, previously
    `Mutex<Option<(CapabilityEvaluationReport, Arc<AnalysisRuntime>)>>` (a
    shape with no way to cache a *failure*, and in practice entirely unused —
    both `runtime` and `workers` were flagged dead-code before this slice),
    is now `Mutex<Option<Result<Arc<AnalysisRuntime>, CapabilityEvaluationReport>>>`,
    byte-for-byte the same shape as `EmbeddingRuntimeState::runtime`.
  - A new `runtime_now(ctx)` in `analysis.rs` locks that mutex, returns the
    cached `Ok`/`Err` if present, else calls `initialize_musicnn(&ctx.config.analysis)`
    exactly once and caches whatever it returns — success or failure, retained
    until process restart, mirroring `stages::embedding::runtime_now` line for
    line (including the `unwrap_or_else(PoisonError::into_inner)` poison
    recovery).
  - `initialize_musicnn` consumes only `ctx.config.analysis` (never a per-track
    path), returning `musicnn_disabled_status()`/`musicnn_unconfigured_status()`
    (new, mirroring `embedding.rs`'s `disabled_status`/`unconfigured_status`)
    for the disabled/unconfigured cases and otherwise delegating to
    `load_musicnn_compute(model_dir)` (now taking only the model directory,
    the unused `_ctx` parameter dropped).
  - Both `analysis_availability` (batch-level) and `compute_and_publish`'s
    MusiCNN branch (per-track) now call `runtime_now(ctx)` instead of
    `load_musicnn_compute` directly, so the 11 models load and verify exactly
    once per process and always from the one correct, configured directory.
  - `AnalysisRuntime` wraps `MusicnnCompute` in a `Mutex` rather than holding
    it bare: `MusicnnCompute::compute` takes `&mut self` (the mel frontend
    reuses scratch buffers) while the underlying ONNX core/head session pools
    are hard-coded to size 1 in `MusicnnCompute::from_verified_artifacts`
    regardless of `AnalysisConfig::workers` — so a `Mutex`-guarded `&self`
    `compute()` method is both necessary (to share the cached `Arc` across
    concurrent claims) and harmless (the native sessions were already
    serialized at size 1).
- **Degradation policy — unchanged, not re-litigated here:** D-157's
  `analysis_groups_resolved` and the `attempt.rs` `Combined`-branch
  availability gate are reused exactly as they were reviewed; this slice only
  supplies the production caller that actually exercises them against real
  `track_stages`/`track_analysis` rows end to end, rather than the synthetic
  unit-test harness D-157 shipped with.
- **New end-to-end test coverage:** `crates/kima-ident/tests/analysis_e2e.rs`
  (new file), structurally mirroring `embedding_e2e.rs`'s own safety gates —
  its own identity-verified throwaway Postgres database (`isolated_db()`:
  structural `DATABASE_URL`/`PGDATABASE` parse, host/port/database-name
  refusals, a live server-identity query, an empty-schema probe, all *before*
  creating a uniquely-named scratch database — never `#[sqlx::test]`, which
  would create/drop databases ahead of any such guard), `#[ignore]`d so a
  plain `cargo test` needs neither a model nor a database, and an explicit
  `panic!`/`assert!` on a missing private-model prerequisite rather than a
  silent skip. Three tests, all run through the real orchestrator
  (`analysis::run_batch`/`run_claim`, and once also `coordinator::run_cycle`):
  1. `full_analysis_dsp_and_musicnn_marks_stage_done` — the real fixture
     (`tools/fixtures/chromaprint/tone.wav`) decoded through the real
     snapshot store, DSP (loudness/key/tempo) and the 11 real pinned private
     MusiCNN ONNX models (`KIMA_MUSICNN_MODEL_DIR`, `ORT_DYLIB_PATH`) all
     succeed; the stage closes `done` with `source = 'analysis'`, no error,
     and `kima_db::analysis::analysis()` reads back all four groups each at
     `PIPELINE_VERSION = "audio-analysis-v1"`.
  2. `missing_musicnn_model_dir_still_runs_dsp_and_marks_stage_done` — no
     `KIMA_MUSICNN_MODEL_DIR` configured (the `MusicnnAnalysis` capability is
     `Unavailable`): DSP still runs to completion and the stage still closes
     `done` (D-157's own policy), with the `musicnn` group read back as
     absent (`None`, never a fabricated value) while loudness/key/tempo are
     all present. Exercised twice — once calling `analysis::run_batch`
     directly, once through a second track via the real
     `coordinator::run_cycle` — to confirm the coordinator wiring itself
     (not just the stage function in isolation) degrades correctly.
  3. `stale_file_generation_rejects_publication_and_claim_is_not_done` — a
     `tracks.ml_generation` bump is held open in an uncommitted transaction
     (any identity-tracked column change, e.g. `mtime`, fires
     `invalidate_track_ml()`). The worker, having already captured the *old*
     generation and audio path via its own MVCC snapshot before that
     transaction started, proceeds through real decode and DSP compute; its
     publish-time generation fence inside `analysis_publish` (a
     `SELECT ... FOR UPDATE` on `tracks`) blocks on the held lock — observed
     via `pg_stat_activity` lock-wait polling, exactly like `embedding_e2e.rs`'s
     own `superseded_worker_cannot_publish_using_replacement_token` test —
     then, once the held transaction commits, re-evaluates against the new
     (mismatched) generation and rejects the write. The claim never closes
     `done` (`ClaimResult` is not `Done`); no `track_analysis` row is ever
     written for the rejected attempt; and the identity-invalidation
     trigger's own side effect (resetting the `'analysis'` stage row to
     `pending`, clearing `started_at`) is itself the redelivery mechanism —
     the next cycle will reclaim and retry the track against its current
     generation.
  - A first draft of these three tests used `kima_ident::EmbeddingConfig::default()`
    (no `snapshot_dir`), which made every one of them fail (or, worse, pass
    for the wrong reason in test 3 — any non-`Done` result, including an
    unrelated infrastructure failure, satisfies `!matches!(result, ClaimResult::Done)`)
    because the analysis stage reuses `stages::embedding::snapshot_store`,
    which requires a configured `snapshot_dir` exactly like the embedding
    stage does. Fixed by adding a `private_snapshot_dir`/`embedding_config_for_analysis`
    helper (mirroring `embedding_e2e.rs`'s own `private_snapshot_dir`) used by
    every test here; re-verified that test 3 genuinely reaches and exercises
    the generation fence (not an unrelated snapshot failure) by rerunning it
    against the corrected config and confirming `pg_stat_activity` really
    shows a `tracks ... FOR UPDATE` lock wait before the held transaction
    commits.
- **Regression check on an unrelated pre-existing failure:** a bounded
  `cargo test -p kima-ident --test events` run turned up
  `cycle_emits_on_count_change_only` failing. Before attributing it to this
  slice, the exact files changed by this round (`ctx.rs`'s struct shape,
  `analysis.rs`'s caching functions) were reverted in place to the state
  handed off before this round (confirmed via `cargo check` reproducing the
  same pre-existing warning set) and the same test re-run: it failed
  identically with none of this round's changes present, confirming the
  failure predates and is unrelated to this slice (environment/timing, not
  investigated further — out of this task's scope). The two reverted files
  were then restored byte-for-byte to this round's fix (diffed against a
  backup to confirm) before any further verification.
- **Verification (bounded tranches, this round):**
  `SQLX_OFFLINE=true cargo check --workspace --all-targets` clean, 0 errors
  (only the pre-existing `workers`-field dead-code warning remains).
  `cargo test -p kima-ident --lib analysis::` 1/1.
  `cargo test -p kima-ident --lib` 32 passed, 0 failed, 4 ignored (the
  pre-existing private-model-gated embedding tests).
  `cargo test -p kima-ident --test coordinator --test events` (dev Postgres,
  `DATABASE_URL=postgres://kima:kima@127.0.0.1:5456/kima`): 21/21 coordinator
  tests pass; the one `events.rs` failure is the pre-existing, unrelated
  issue confirmed above.
  `cargo test -p kima-ident --test analysis_e2e -- --ignored --test-threads=1`
  against a fresh run-owned isolated Docker Postgres (`pgvector/pgvector:pg17`,
  localhost-only port, preflight `SELECT current_database(), inet_server_port()`
  and empty-schema check before any SQLx use, never port 5456/`kima`), with
  `KIMA_MUSICNN_MODEL_DIR=~/.cache/kima-models/m4-approved-private` and a
  trusted `ORT_DYLIB_PATH`: all three tests pass together, 12.65s.
- **Changed files this round:** `crates/kima-ident/src/stages/analysis.rs`
  (cosmetic cleanup + `runtime_now`/`initialize_musicnn`/`musicnn_disabled_status`/
  `musicnn_unconfigured_status`, `AnalysisRuntime` now `Mutex`-wrapped with a
  `&self` `compute()` method, `load_musicnn_compute`/`analysis_availability`/
  `compute_and_publish` updated), `crates/kima-ident/src/ctx.rs`
  (`AnalysisRuntimeState.runtime` field type), `crates/kima-ident/tests/analysis_e2e.rs`
  (new), `docs/DIVERGENCES.md` D-158 (accepted, append-only; D-146 through
  D-157 untouched), this spec section. No changes to `kima-db`, `kima-ml`,
  `coordinator/mod.rs`, `kima-api`, `kima-server` or any other already-dirty
  file.
- **Not addressed by this slice (open, explicitly out of scope):** the
  `AnalysisRuntimeState::workers` dead-code field (a semaphore-caching
  improvement analogous to `embedding_runtime.workers.get_or_init`, not
  requested by this task — today `run_claim_stopping` still allocates a fresh
  `Semaphore` per claim instead of a per-process shared one); real-model/
  Essentia parity for key/tempo (D-008/D-153's own open item, unrelated);
  the pre-existing `events.rs::cycle_emits_on_count_change_only` failure
  (confirmed unrelated to this slice, not investigated further);
  independent review of this round's diff; commit (push only with approval);
  post-M4 cleanup of this round's new isolated Docker Postgres container
  (left Up, localhost-only, port recorded in
  `~/.hermes/cache/scratch/analysis-coord-port.txt`) alongside the other
  tracked leftover test containers from earlier slices.
