# Apple podcast fixtures (M6 Task 5)

`crates/kima-podcast/tests/fixtures/apple/{catalog,search}.json` are upstream's recorded Apple
replies, not new recordings (owner decision Q5, taken by the controller under the owner's
"continue" go-ahead; see the M6 plan's open-questions section).

- **Source:** kima-hub v1.12.3 (`c7527577d925b20f5adf410c7fd4e0c8ac250f69`),
  `backend/src/__tests__/fixtures/apple/catalog.json` (recorded 2026-10-16T14:43:09Z) and
  `search.json` (2026-10-17T18:43:08Z), written by upstream's
  `backend/scripts/record-apple-podcasts.ts` against `https://itunes.apple.com`.
- **Licence:** upstream ships them in its GPL-3.0 repository (`LICENSE`: GNU GPL v3,
  `backend/package.json` `"license": "GPL-3.0"`); kima-rs is GPL-3.0-only, so it can carry them
  under the same licence. The replies themselves are Apple's catalogue data, which upstream
  already redistributes for tests; kima-rs uses them for tests only and never serves them.
- **Format:** upstream's `{source, recordedAt, replies: {"<path?query>": {status, body}}}`, which
  `kima_net::record::Recording::load` reads as is (each reply a GET entry), plus a
  `provenance` line.
- **Trimmed (catalog.json, 3.3 MB → 0.4 MB):** only the replies kima-rs tests ask for (the genre
  tree, the overall chart and the 1495/1502/1507 charts, the one- and three-id lookups); in them
  only the fields kima-rs reads: genres `name`/`id`/`subgenres`; chart entries `id.attributes.im:id`,
  `im:name`, `im:artist.label`, `summary.label` (cut to 80 characters), the last `im:image`,
  `category.attributes`; lookup results `wrapperType`, `kind`, `collectionId`, `feedUrl`,
  `genreIds`, `genres`, `collectionName`, `artistName`, `artworkUrl600`, `artworkUrl100`,
  `trackCount`. `search.json` is unchanged apart from the provenance line.
- **Regenerate:** run the trimming over upstream's file again (a short script: load, keep the
  listed URLs and fields, write compact JSON); never record against Apple from a test.
