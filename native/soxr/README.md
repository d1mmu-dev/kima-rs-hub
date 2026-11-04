# Kima's private dynamic SoX Resampler build

SoX Resampler Library copyright (c) 2007–18 robs@users.sourceforge.net.
LGPL-2.1-or-later, not the application's license. No warranty. The original
`LICENCE`, full `COPYING.LGPL` and `AUTHORS` travel with the library in
`/opt/kima/soxr/share/soxr`, as do the complete original source archive, Kima's
patch, this notice, build recipe and recorded CMake options. No model weights
are included: model licensing/provisioning is a separate boundary; approved
private-use M4 assets must never be copied into Git, CI or Docker images.

## Source integrity and provenance

Pinned source URL:
https://deb.debian.org/debian/pool/main/libs/libsoxr/libsoxr_0.1.3.orig.tar.xz

SHA-256: `b111c15fdc8c029989330ff559184198c161100a59312f5dc19ddeb9b5a15889`
(94384 bytes). Independently retrieved over authenticated HTTPS on 2026-11-04
from Debian, including the Debian-published checksum in
https://deb.debian.org/debian/pool/main/libs/libsoxr/libsoxr_0.1.3-4.dsc
(saved alongside this file). Both a new source download and the saved experiment
archive match that publisher checksum. This is HTTPS publisher provenance, NOT
a claim that we independently verified the DSC's OpenPGP signature or original
upstream release signing. The build checks the reviewed, source-controlled hash,
not metadata downloaded alongside an untrusted archive.

## Modification and exact supported scope

Kima modification, 2026-11-04: `fft-cache-init.patch` guards initialization of
FFT4G's shared cache locks with a named OpenMP critical region and separate
once-only state. Every initializer reads that state under the critical region;
`FFT_LEN` no longer serves as an unsynchronized initialization sentinel. No
application serialization or signal arithmetic change is used.

Reviewed configuration is **GCC, WITH_OPENMP=ON, WITH_PFFFT=OFF,
WITH_AVFFT=OFF, Release -DNDEBUG**, linux/amd64, dynamic library kept loaded for
the process lifetime. The recipe checks the resulting libgomp dependency.
The small patch is NOT a config-general thread-safety proof. Without OpenMP
the critical directive is absent; debug builds retain an assertion reading
`FFT_LEN` before acquiring the reader lock in `UPDATE_FFT_CACHE`, racing cache
growth. Those configurations are unsupported by this recipe. Cache clearing
is only valid after all callers have stopped (upstream atexit); no reuse after
`LSX_CLEAR_FFT_CACHE` is supported because once-only state remains set. Do not
explicitly clear or unload/reload a live library while workers can use it.
This does not identify the precise instruction behind the historical SIGSEGV,
nor prove real-model parity or all native-library thread safety.

## Build, use and replace

On Debian bookworm/amd64 install build prerequisites **inside the build
container**, not into the host: gcc, libc6-dev, make, cmake, patch, curl, ca-certificates,
xz-utils and binutils. From the Kima checkout:

```
sh scripts/build-soxr.sh /absolute/new/private/prefix
export LIBRARY_PATH=/absolute/new/private/prefix/lib
export LD_LIBRARY_PATH=/absolute/new/private/prefix/lib
cargo test --locked -p kima-ml --test clap_cold_start --test clap_prepare
cargo test --locked -p kima-ml --features pg-publish --test clap_cold_start --test clap_prepare
```

The project Dockerfile builds on bookworm, matching the final image baseline;
a library built on a newer host is not substituted into the image. CI explicitly
selects the private prefix for both linking and loading. `ldd` must resolve
`libsoxr.so.0` to that prefix and `libgomp.so.1` to the runtime package.

Dynamic replacement/relinking is intentional: no static libsoxr is embedded.
To rebuild from the image's corresponding-source bundle, recreate directories
`scripts/` (build-soxr.sh) and `native/soxr/` (README.md, DSC and patch) and run the
same command above with a new prefix. The included archive can also be extracted,
patched with `patch -p1`, and built with the explicit CMake flags in the recipe
without network access. Keep license/notices with redistributed replacements.
Operators can mount an ABI-compatible replacement directory read-only and set
`LD_LIBRARY_PATH` to its `lib/`, or rebuild the image with the modified recipe.
The administrator controls that loader path; writable model directories must
not be used for native libraries. Root-owned shipped files are not an attempt
to prohibit user replacement, reverse engineering for debugging modifications,
or relinking. No promise of bit-identical builds is made.
