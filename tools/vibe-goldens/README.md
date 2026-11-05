# vibe-goldens

`run.mjs` regenerates `crates/kima-vibe/tests/fixtures/ts-golden-*.json` from upstream kima-hub's pure TypeScript
modules (pinned `c7527577d925b20f5adf410c7fd4e0c8ac250f69`). See that folder's README for the file list.

    git clone https://github.com/Chevron7Locked/kima-hub "$UPSTREAM"
    git -C "$UPSTREAM" checkout c7527577d925b20f5adf410c7fd4e0c8ac250f69
    node tools/vibe-goldens/run.mjs          # UPSTREAM defaults to ~/.hermes/cache/scratch/kima-hub-latest

Needs Node >= 23.6 (type stripping); run with `v26.7.0`. No npm install, no network, no Prisma: the five pure modules
only import each other and a worker stub. The output must be byte-identical on every run.
