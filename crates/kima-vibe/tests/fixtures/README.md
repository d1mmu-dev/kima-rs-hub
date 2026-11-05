# kima-vibe test fixtures

All files are synthetic or derived only from synthetic inputs. Nothing here is real library data,
an audio-derived vector, a feel probe, or a mood head (owner decision Q3, 2026-11-05: those stay private
outside Git).

## Copied from upstream (GPL-3.0 on both sides)

Source: <https://github.com/Chevron7Locked/kima-hub> tag v1.12.3, commit `c7527577d925b20f5adf410c7fd4e0c8ac250f69`.

| File | Upstream path | SHA-256 | Generator |
|---|---|---|---|
| `parity.json` | `backend/src/services/matching/__tests__/fixtures/parity.json` | `99af9bf52ceb2cc83f0883393f71d8aa04a5bf24dee19559b0181b5eb8f5efd6` | `services/audio-analyzer-clap/eval/matching_parity_fixture.py` ("made-up vectors, ratings and genres, no real data") |
| `map-parity.json` | `backend/src/services/matching/__tests__/fixtures/map-parity.json` | `a6817565a01453fd0857c3b11aaf9651f73e117f20df418068ea5f172e02a5b3` | `services/audio-analyzer-clap/eval/map_parity_fixture.py` (umap-learn / scikit-learn output on made-up points) |
| `../../assets/mbGenres.json` | `backend/src/services/matching/mbGenres.json` | `430a655a0fbd77ebcede127d83a30d945685d84d87fc62c899f40d7eb70ae741` | MusicBrainz genre list, CC0, fetched 2026-10-06 by upstream |

## Deliberately NOT copied: `shipped-reference.json`

Upstream's `backend/src/services/moods/__tests__/fixtures/shipped-reference.json`
(SHA-256 `5c562196be7a67665caeeb3bcd2e1b8fa37941c15f8f7f450bd54dfde2704dc3`, 47 KB) holds four 512-d unit
float32 vectors with the 22 mood logits "computed from W1's shipped models". Its provenance statement says
nothing about the vectors being synthetic; they have the statistics of real CLAP audio embeddings, and the logits are
outputs of the mood heads, whose training-data licences are unchecked (Q3). It is therefore not committed and the
plan's `moods-reference.json` does not exist. The test `shipped_reference_scores_to_1e-4` (Task 11) stays
`#[ignore]`d behind `KIMA_VIBE_ASSET_DIR` until the owner clears it or supplies the file privately.

## Generated here: `ts-golden-*.json`

Produced by `tools/vibe-goldens/run.mjs`, which runs upstream's own pure modules (`exactMath`, `formula`,
`mapLayout`, `mapCompute`, `neighbours`) from the pinned clone under Node and records IEEE-754 bit patterns
(16 hex digits per f64).

- Node `v26.7.0` (`~/.hermes/tools/node-26.7.0-linux-x64`), native TypeScript type stripping. **No `tsx`**: the
  modules are copied to a temp dir with `.ts` import suffixes, `./worker` stubbed, and `gaussian` exported (the only edits).
- `ts-golden-exactmath.json`: 2,000 inputs each for `exp`, `log`, `atan2`, `pow` (including NaN/inf/zero/subnormal
  edges), the first 1,000 outputs of `seededRandom(42)` and of `gaussian(seededRandom(1234))`.
- `ts-golden-library.json`: the inputs both libraries below use (tracks with raw valence/arousal, artist genres already
  filtered to the MusicBrainz list), plus the library statistics as bit patterns.
  `fixture50` is `parity.json`'s 50 tracks; `synthetic300` is 300 tracks / 12 artists / dim 32 drawn with `seededRandom(7)`.
- `ts-golden-neighbours.json`: `nearestColumns(k=14, same set)` (idx, scores, `maxScore`) and `neighbourLists(k=10)` per library.
- `ts-golden-layout.json`: `layoutLibrary` (positions, colour, frame, `maxScore`) per library.
- `ts-golden-place.json`: `layoutLibrary` of the first 250 tracks of `synthetic300`, then `placeTracks` of the other 50.

Regenerate: `node tools/vibe-goldens/run.mjs && git diff --exit-code crates/kima-vibe/tests/fixtures`.
