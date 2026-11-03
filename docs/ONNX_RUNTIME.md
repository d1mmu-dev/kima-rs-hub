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

## CI / Docker packaging

CI explicitly acquires the pinned wheel with `scripts/acquire-ort.sh`, then
runs the network-free `provision-ort.py` integrity checks. The root-owned
`/opt/kima/ort` directory retains LICENSE and ThirdPartyNotices.txt, removes
write permissions, and is outside model/data volumes. The Rust job sets
`ORT_DYLIB_PATH=/opt/kima/ort/libonnxruntime.so.1.22.0` before tests. This is an
explicit build-time TLS acquisition, not an offline acquisition claim. Hosted
CI has not been run for this local, unpushed change.

Docker uses the same acquisition/provisioning in a separate stage and copies
only the library and notices into the final image at that same absolute path.
Python, curl and the wheel are not copied from that stage. libc and related
loader libraries come from Debian; libstdc++6 and libgcc-s1 are explicitly
installed alongside ffmpeg/libsoxr. Only **linux/amd64** is approved: the
acquisition script rejects other OS/architectures before downloading anything.
Do not label this image as ARM64-ready without separately approved artifacts.
No production model is downloaded or packaged.

Local verification built the complete, unchanged frontend pipeline (`npm ci`,
`npm run build`), Rust server, and final image with the legacy Docker builder
(`DOCKER_BUILDKIT=0 docker build -t kima-m4-ort:review .`). Buildx was unavailable.
The server does not yet depend on kima-ml, so server startup alone would not
exercise ORT. Instead, the matching backend-stage image built the existing
`onnx_session`, `onnx_runtime`, and `onnx_resources` integration test binaries:

```sh
# BACKEND_IMAGE is the image ID printed immediately before the final FROM.
docker run --name kima-m4-ort-test-build --entrypoint sh "$BACKEND_IMAGE" -c '
  cargo test --locked --release -p kima-ml --test onnx_session --test onnx_runtime --test onnx_resources --no-run &&
  mkdir /ort-tests &&
  for name in onnx_session onnx_runtime onnx_resources; do
    for f in target/release/deps/$name-*; do
      if [ -x "$f" ] && [ ! -d "$f" ]; then cp "$f" /ort-tests/$name; fi
    done
  done'
docker cp kima-m4-ort-test-build:/ort-tests /approved/test-output/
sh scripts/check-ort-image.sh kima-m4-ort:review /approved/test-output/ort-tests
```

The final-image check mounts those test-only binaries read-only into the
**unmodified final image**, as user kima, with network disabled, read-only root,
a temporary /tmp, 512 MiB memory, 2 CPUs and 64 PIDs. It verifies native library
SHA-256, root ownership/non-writability of the runtime path and its parents,
notices and loader dependencies. All 13 integration tests passed, including
synthetic Identity inference (42 -> 42), concurrent inference, bounded actual
thread count and missing/relative/nonexistent/corrupt native-library paths in
fresh subprocesses. These failures return capability errors, not process exit
101. Deliberately injected, caught unwind messages in the tests are expected.
The test binaries are not shipped in the production image.

A clean-target, offline/locked Rust test run checks that cached build products
cannot conceal runtime acquisition. Cargo dependency caches must already be
populated; this is not a claim that the Docker build itself is network-free.
Feature-tree checks must continue to exclude `download-binaries`/`copy-dylibs`.
Synthetic inference and a 512 MiB smoke limit do **not** establish real-model
parity or peak-memory requirements. Independent review, lawful real-model
parity and measured resource work remain open. DB migration is out of scope
and remains gated/unstarted.
