# M-SYNC Task 19: comparison frontend podcast slice

Reference: upstream v1.12.3 `c7527577d925b20f5adf410c7fd4e0c8ac250f69`; this is a partial comparison-only migration, not a production Next.js path or completion of Task 19.

## Podcast inventory

Compared paths under `frontend/features/podcast` and `frontend/app/podcasts` against the pinned checkout, by file bytes before the episode-count control was restored:

- Exact matches: `features/podcast/components/PreviewEpisodes.tsx`, `features/podcast/hooks/usePlayEpisode.ts`, `app/podcasts/loading.tsx`. `features/podcast/utils.ts` was exact then but now retains the fork's episode-count sort.
- Adapted same-path files: `CategoryChips`, `CategoryShelf`, `ContinueListening`, `EpisodeList`, `ListeningShelf`, `NewEpisodes`, `PodcastActionBar`, `PodcastHero`, `PodcastSearch`, `PodcastShelf`, `usePodcastActions`, `usePodcastData`, `types.ts`, `app/podcasts/page.tsx`. These are not asserted visually or functionally identical.
- Local-only: `SimilarPodcasts`, `genres.ts`, `routes.ts`, podcast `layout.tsx`, `genre/page.tsx`, `show/page.tsx`, and `category/page.tsx`. The last two adapt upstream `[id]` and `category/[id]` to static query routes. Genre is a legacy retained page; upstream removed it. Local layout retains the podcasts feature gate; detail playback retains the local episode queue controls.
- Upstream-only paths (adapted at different paths, not byte-for-byte imported): `app/podcasts/[id]/page.tsx`, `app/podcasts/category/[id]/page.tsx`.
- Remaining comparison gaps: detail `usePodcastData` still uses the older followed-show similar-podcast shape and does not request preview similar shows; local playback actions differ from upstream. Backend podcast functionality is deferred to M6, not implemented in this slice. Episode queue and episode-count sorting are retained local behavior. Browser interaction and screenshot parity have not been established.

## Remaining Task 19 gates

Inventory the rest of the upstream frontend delta against the fork rather than treating this podcast inventory as the whole refresh. Reconcile remaining partial pages, legacy `/queue` and `/podcasts/genre`, settings and changelog, feature gates, and update `tools/frontend-route-codemod.mjs` including category/settings rules with regression tests. Run the plan's kima-rs Playwright suites, regenerate the M9 visual baselines and inspect screenshot diffs at the specified viewports. Update D-168 only after checking the actual capability gates and scope. No parity or Task 19 completion is claimed here.

## Local verification (2026-11-10)

From `frontend/`, after restoring the local sort control: `npx tsc --noEmit` exit 0; `npm run test:unit` exit 0 (56 files, 514 tests); `npm run build` exit 0 (41/41 Next static-generation units; 42 HTML files under `out`, including error/fallback files). The `out/podcasts` tree contains index, show, category and retained genre HTML. Vite emitted its existing native-config future compatibility warning. These gates test the comparison artifact only; they do not demonstrate live backend integration or visual parity.

Full command logs and the 20-file pre-edit dirty snapshot: `~/.hermes/cache/scratch/frontend-repair-20261110/` (scratch is temporary). Baseline tsc failed eight diagnostics in `CategoryShelf`; baseline unit run had 507 passed and 2 failed (missing NextRouter). The playlist compile-time XOR/reply regression failed before its fix; the local episode-count sort regression failed before its fix.
