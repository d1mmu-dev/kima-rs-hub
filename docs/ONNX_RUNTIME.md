# ONNX runtime provisioning (M4 review fix)

## Contract

`ort = 2.0.0-rc.10` uses API 22. Cargo enables only `std` and `load-dynamic`,
with defaults **off**. Neither Cargo nor model/session startup downloads or
copies runtime binaries. `ORT_DYLIB_PATH` must name a separately provisioned,
absolute shared-library path. No implicit current-directory/system lookup.
Missing, relative, nonexistent and unloadable runtime paths become capability
load errors. Initialization success/failure is process-wide and cached: provision
before starting and restart after correcting the runtime. This code assumes
panic=unwind (the repository default); ort rc.10's dlopen/API panics are contained
at initialization. A corrupt/hostile native library is not sandboxed by Rust.

The administrator must own the runtime directory and all its parents; the
service/model-volume writer must not be able to replace it. The application
trusts this native-code path, **not** a digest supplied from the writable model
directory. Provisioning verifies the pinned package and extracted library.
Model artifacts remain independently size/digest-checked through
`verify_artifact`; there is no unchecked `VerifiedBytes` constructor. Tests use
explicit synthetic descriptors, not modified production hashes. Memory-only
ORT loading rejects external initializers, including an existing absolute file;
there is no file-path fallback.

## Exercised Linux x86_64 CPU runtime

Separately acquired upstream Microsoft `onnxruntime==1.22.0` PyPI wheel:

- `onnxruntime-1.22.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl`
- SHA-256: `6964a975731afc19dc3418fad8d4e08c48920144ff590149429a5ebe0d15fb3c`
- Extracted `libonnxruntime.so.1.22.0` SHA-256:
  `7520bc1b4f649ee8fa956442a66ded303e8fd3b465b0159b7e893bdc87df4e38`
- Wheel digest independently matched the versioned PyPI JSON metadata over TLS.
  This is upstream package provenance, not a reproducible source-build claim.
- ORT's bundled `LICENSE` is MIT (Microsoft); preserve it **and**
  `ThirdPartyNotices.txt`. Review all applicable bundled third-party notices
  before distribution. This runtime license grants no model/artifact rights.

An operator acquires the exact wheel separately (e.g. an approved artifact
mirror, or an explicit `pip download --no-deps --only-binary=:all:
--python-version 312 --platform manylinux_2_27_x86_64 --implementation cp
--abi cp312 onnxruntime==1.22.0`). Do not pip-install Python dependencies;
Python/NumPy is not needed to execute the native library.

From the repository, with the wheel already present and a new destination:

```sh
python scripts/provision-ort.py /approved/path/onnxruntime-1.22.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl /approved/path/ort-1.22.0
export ORT_DYLIB_PATH=/approved/path/ort-1.22.0/libonnxruntime.so.1.22.0
cargo test --offline --locked -p kima-ml
cargo clippy --offline --locked -p kima-ml --all-targets -- -D warnings
```

The provisioning script never accesses the network, checks both digests before
creating the destination, extracts only the library/license/notices and refuses
an existing destination. No model assets or runtime binaries enter Git.
For another platform or a source build, independently approve provenance,
version/API compatibility, licenses and hashes; do not reuse these hashes or
claim that platform was tested. Rust dependencies still require a separately
populated Cargo cache/vendor directory for `--offline`.

## Resource admission policy

- Default intra-op threads **1**, explicit range **1..=4**. Inter-op threads 1,
  sequential graph execution. No auto-sized ORT thread pool. Linux subprocess
  regression observes the actual process thread count around real inference.
- Pool capacity **1..=4**, validated before allocating sessions. Use the shared
  model registry/session clones across workers, not a model set per worker.
  A deliberately requested pool duplicates weights and is charged accordingly.
- All live consumer sessions and pools share one process-wide admission budget:
  **32 sessions**, **64 intra-op thread slots**, **4 GiB estimated memory**.
  Each session reserves `4 * encoded_model_bytes + 16 MiB`; checked arithmetic,
  aggregate rejection before ORT creation, rollback on load failure, release
  only when the final shared clone/pool drops. No additional charge for clones.
- CPU arena retention is disabled. The estimate is an admission policy, **not a
  hard RSS/activation/allocator bound**: graph optimization, tensor shapes,
  caller inputs and ORT can exceed it. Deployment still requires a measured
  memory limit/cgroup and workload-specific input limits before production.
  Full real-model peak RSS and parity are unverified without approved assets.
- A checkout guard returns and notifies on error **and unwind** without holding
  the pool mutex in user code. Shared-session poisoning returns `LockPoisoned`;
  do not silently clear poison and continue inference. Aborting the process or
  leaking a checkout with `mem::forget` is not unwind recovery.

## Clean CI / Docker status and required packaging gate

A fresh target-directory, `--offline --locked` local build and real ORT tests
were exercised using the separately provisioned library above; this does not
establish a clean hosted-CI or Docker release. Existing workflow/image runtime
packaging is **not completed by this fix**. Until provisioned, ONNX tests will
fail honestly and capability loading reports the missing runtime; do not
restore Cargo auto-download features to hide that failure.

Before release, the CI/image owner must:

1. Provision the checksum-pinned runtime from an approved CI artifact or an
   explicit acquisition stage, retaining license/notices and integrity checks.
2. Run the Rust stage with network disabled, a clean target directory, the
   approved Cargo dependency cache/vendor input, and explicit `ORT_DYLIB_PATH`.
   Confirm the feature tree contains no `download-binaries`/`copy-dylibs`.
3. Copy the provisioned runtime + notices to a root-owned read-only directory in
   the final image. Set the absolute path; include its native loader/libc,
   libstdc++, libgcc, libm, pthread/rt/dl dependencies for the target image.
4. Exercise real CPU inference and missing/corrupt runtime failures **inside
   that final image**, including memory/thread limits and non-root permissions.
   Do not claim hosted/Docker success from this host's `ldd` or Cargo results.
5. Independently re-review the Important fixes and finish lawful real-model
   parity/peak-memory work. DB migration remains out of scope and unstarted.
