# vibe-goldens

`run.mjs` regenerates `crates/kima-vibe/tests/fixtures/ts-golden-*.json` from upstream kima-hub's pure TypeScript
modules (pinned `c7527577d925b20f5adf410c7fd4e0c8ac250f69`). See that folder's README for the file list.

The modules are read from the pinned commit's git objects (`upstream.mjs`), never from a working tree, so any
checkout that contains the pin works, including the freshly pulled external reference (`KIMA_HUB_REFERENCE`); the
benches read the same way.

    node tools/vibe-goldens/run.mjs          # UPSTREAM defaults to KIMA_HUB_REFERENCE (env or private tools config)
    git diff --exit-code crates/kima-vibe/tests/fixtures

Needs Node >= 23.6 (type stripping); run with `v26.7.0`. No npm install, no network, no Prisma: the five pure modules
only import each other and a worker stub. The output must be byte-identical on every run.
