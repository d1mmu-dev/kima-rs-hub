# kima-web

Rust/WASM frontend for Kima, built on [vertigo](https://github.com/vertigo-web/vertigo) and served via
server-side rendering from `kima-server`.

**Toolchain (confirmed in M9 Task 0, not assumed).** Target: `wasm32-unknown-unknown`
(`rustup target add wasm32-unknown-unknown`). The plan named vertigo 0.6.3 and a nightly requirement; both
turned out stale: `vertigo-macro` 0.6.x/0.6.4 fails on current nightly (`Span::source_file` was removed from
`proc_macro`) and `vertigo-cli` < 0.12 has no library target, which the in-process SSR design (spec §3) needs.
vertigo/vertigo-macro/vertigo-cli **0.13.4** build for `wasm32-unknown-unknown` on both stable and
nightly (verified with a scratch crate on `cargo +stable` and the default nightly 1.96), so no nightly is
required and the workspace toolchain is not changed. `vertigo build` needs `wasm-opt` (binaryen) on `PATH`:
Arch `pacman -S binaryen`, or without root download a release from
<https://github.com/WebAssembly/binaryen/releases> and put `bin/wasm-opt` on `PATH`. Check the whole setup with
`tools/verify-wasm-toolchain.sh`. Install the CLI matching the library version:
`cargo install vertigo-cli --version 0.13.4 --locked`.

**Build.** `tools/verify-kima-web-build.sh` runs `vertigo build` in this crate and checks the output in `build/`
(gitignored): `index.json` (manifest; `vertigo serve` renders the HTML from it), a hashed `kima_web.<hash>.wasm`
and a hashed `wasm_run.<hash>.js`. vertigo 0.13 writes no static `index.html` and no unhashed `wasm_run.js`.
Set `VERTIGO=/path/to/vertigo` to pick a CLI that is not first on `PATH`; keep it at the same version as the
`vertigo` crate (`=0.13.4` in `Cargo.toml`).
