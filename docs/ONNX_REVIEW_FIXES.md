# ONNX Important review fixes — execution record

Scope: review of `c95ec4eb9523dad5b65500e8fcc6d248967b409c`.
No DB migration, model provisioning/parity, npm changes, or push.

## Implemented

1. **VerifiedBytes bypass**: removed unchecked public constructor. Compile-fail
   regression first failed because arbitrary bytes still compiled, then passed
   after removal. All synthetic sessions now use `verify_artifact` with an
   explicitly synthetic descriptor (digest + exact size limit). All 13 real
   descriptors reject Identity bytes under their production IDs.
2. **Pool unwind safety**: checkout ownership guard returns the session and
   notifies waiters on success/error/panic. Original callback-panic regression
   failed with a five-second waiter timeout, then passed. A strengthened test
   observes actual condvar wait registration under the pool mutex; temporarily
   restoring the original non-RAII return path reproduced its timeout. Final
   guarded version passed **20/20** repetitions. Shared-session poisoning is
   intentionally fail-closed and tested, not silently recovered for inference.
3. **Offline runtime**: ORT default features removed, dynamic loading only;
   missing/relative/nonexistent/unloadable runtime path gives a load error.
   Both the no-defaults and unprovisioned-runtime tests failed against the old
   configuration before the fix, then passed. Separately acquired upstream
   Microsoft CPU ORT **1.22.0** wheel, matched its digest to versioned PyPI
   metadata, exercised the offline checksum-verifying provisioner and real
   inference from a fresh Cargo target. Corrupt wheel rejected with exit 2 and
   no destination created. Exact provenance, licenses, hashes and pending
   hosted-CI/final-image packaging gates: `ONNX_RUNTIME.md`.
4. **Resource bounds**: default one intra-op thread, allowed threads and pool
   size 1..=4; inter-op 1/sequential execution. Tests first reproduced accepted
   zero threads, accepted size-five pool and machine-sized default thread
   growth, then passed. Aggregate real-session test first reproduced the absent
   32-session limit; final code shares a 32-session/64-thread-slot/4-GiB-estimate
   budget across consumer sessions and pools. Final-clone lifetime, failed-load
   rollback and pool participation tested with real ORT sessions; arithmetic
   overflow and aggregate memory-estimate admission tested without allocating
   multi-GB models. CPU arena retention disabled. **This is not a hard RSS or
   activation-memory bound**; deployment cgroup/peak-memory verification is
   still required.

Additional real-ORT synthetic regression rejects unverified external
initializers (present absolute file and relative location) in both consumer and
pool memory-loading paths. Its native ORT error logs are expected refusal
messages, not test failures. No fake ORT backend or production model assets.

## Final gates actually executed

With a new target directory, the provisioned `.so`, and Cargo network access
forbidden by `--offline`:

- `cargo test --offline --locked -p kima-ml`: **107 passed**, zero failed,
  ignored, measured or filtered out. **106 unit/integration + 1 compile-fail
  doctest**, across 23 result summaries.
- `cargo clippy --offline --locked -p kima-ml --all-targets -- -D warnings`: pass.
- `cargo fmt --all -- --check`: pass; `git diff --check`: pass.
- `cargo tree --offline --locked -p kima-ml -e features`: no
  `download-binaries`, `copy-dylibs` or `fetch-models`.
- Programmatic `--list` reconciliation: baseline **85 existing + 9 ONNX = 94**,
  not 77/95. Final listing **106 regular + 1 doc = 107**, matching execution.

Local evidence prefix `/home/dimmu/.hermes/cache/scratch/m4-onnx-`:
`verified-red.log`, `verified-green.log`, `pool-red.log`, `pool-green.log`,
`pool-deterministic-red.log` (original-path replay), `pool-stress.log`,
`runtime-red.log`, `runtime-green.log`, `resources-red.log`,
`resources-green.log`, `aggregate-red.log`, `aggregate-green.log`,
`baseline-list.log`, `final-list.log`, `final-test.log`, `final-clippy.log`,
`final-features.log`. Scratch evidence is temporary; this record preserves the
verified totals and scope. One intermediate 30-second combined test invocation
was interrupted during an existing slow audio unit test; the final complete run
used a sufficient timeout and passed. An intermediate wrong ORT arena-builder
method did not compile; corrected to the actual CPU provider API before final
checks.

## Still open / not claimed

Independent re-review, clean hosted CI and final Docker-image runtime packaging
and execution are pending. The provisioning/packaging gate is documented, not
reported green. No cross-platform runtime test, lawful real-model parity,
real-model peak RSS, full workspace/frontend/E2E/SQLx checks, or DB work was
performed. The initialization guard assumes the repository's panic=unwind;
a malicious native library is not a Rust sandbox. Reuse shared registry
sessions across workers; the explicit pool intentionally charges duplicates.
