# M-SYNC Task 19: legacy `frontend/` refreshed to v1.12.3 — path inventory

Reference: upstream v1.12.3 `c7527577d925b20f5adf410c7fd4e0c8ac250f69`, fork base `12faf1cd34ff53054221818b0ccde58b8e9b0020`.
The podcast slice before this refresh is described in `m-sync-frontend-podcast-inventory.md`; this file covers
the whole tree after the refresh. `frontend/` stays a comparison reference only (no production, dev or CI path;
M10 deletes it).

## Base proof and three-way inventory

- Fork commit `93dee5f` (`chore(frontend): fork upstream frontend @ 12faf1cd`): its `frontend/` tree is
  byte-identical (git blob ids) to upstream `12faf1cd` for all 404 of 404 files. Neighbouring commits score lower
  (`e1456c0e`: 402/403, `8d33373e`: 395/404); the two commits right after `12faf1cd` do not touch `frontend/`.
- Three-way classification of every `frontend/` path (base `12faf1cd`, ours = HEAD `8a8aa1f`, theirs `c7527577`):
  246 changed upstream only (238 copied, 6 deleted, 2 dynamic-route additions adapted), 104 changed locally only
  (kept), 87 changed on both sides (conflicts), 41 already equal to v1.12.3 from earlier slices, 156 unchanged.
- Conflicts: 30 merged cleanly with `git merge-file`; 32 had hunks resolved by hand; 8 were dynamic `[id]` pages the
  fork had replaced by query pages (merged three-way into the query page: `album`, `artist`, `audiobooks/book`,
  `audiobooks/series`, `browse/playlists/view`, `mix`, `playlist`, `podcasts/show`); 6 upstream deleted
  (`PlayerModeWrapper`, `usePlayerMode`, home `PodcastsGrid`, library `AlbumsGrid`/`ArtistsGrid` — deleted here
  too; `SimilarPodcasts` kept for the podcast slice); 11 were added on both sides by earlier slices (kept ours,
  settings `pages.ts`/`SettingsScreen.tsx` re-aligned to upstream's library anchors).
- Machine-readable log of the apply: `~/.hermes/cache/scratch/t19-evidence/apply-log.json` (scratch, temporary).

## Result against v1.12.3 (536 upstream files)

| Status | Count |
|---|---|
| Exact (byte-identical to v1.12.3) | 420 |
| Adapted at the same path | 98 |
| Adapted at a local path (static export) | 18 |
| Missing (neither the path nor a mapped local path) | 0 |
| Local-only paths (not in v1.12.3) | 89 |

A green build is not visual parity: exact bytes say nothing about how the page renders against kima-rs data, and
an adapted file can differ visually. See "Browser and visual checks" below.

### Adapted at a local path

- `app/album/[id]/page.tsx` -> `app/album/page.tsx`
- `app/api/events/route.ts` -> `(none: axum serves /api/events, D-007)`
- `app/api/events/ticket/route.ts` -> `(none: axum serves /api/events/ticket, D-007)`
- `app/artist/[id]/loading.tsx` -> `app/artist/loading.tsx`
- `app/artist/[id]/page.tsx` -> `app/artist/page.tsx`
- `app/audiobooks/[id]/page.tsx` -> `app/audiobooks/book/page.tsx`
- `app/audiobooks/series/[name]/page.tsx` -> `app/audiobooks/series/page.tsx`
- `app/browse/playlists/[id]/page.tsx` -> `app/browse/playlists/view/page.tsx`
- `app/library/page.tsx` -> `(none: axum 308 /library -> /collection/, D-011)`
- `app/manifest.webmanifest/route.ts` -> `public/manifest.webmanifest`
- `app/mix/[id]/page.tsx` -> `app/mix/page.tsx`
- `app/playlist/[id]/page.tsx` -> `app/playlist/page.tsx`
- `app/podcasts/[id]/page.tsx` -> `app/podcasts/show/page.tsx`
- `app/podcasts/category/[id]/page.tsx` -> `app/podcasts/category/page.tsx`
- `app/settings/[page]/page.tsx` -> `app/settings/{library,sources,server,people}/page.tsx`
- `app/share/[token]/SharePageClient.tsx` -> `app/share/SharePageClient.tsx`
- `app/share/[token]/layout.tsx` -> `app/share/layout.tsx`
- `app/share/[token]/page.tsx` -> `app/share/page.tsx`

### Adapted at the same path (reason)

- `features/library/useShelfSummary.ts`, `features/stats/useTrackPlays.ts`, `hooks/useUpdateStatus.ts`: requests gated behind the `stats`/`changelog` section flags (D-170) because kima-rs serves no such route (commit `75526f4`)
- `app/audiobooks/page.tsx` (6 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `app/browse/playlists/page.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `app/device/page.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `app/import/playlist/page.tsx` (6 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `app/page.tsx` (4 diff lines): section flags (D-030/D-170)
- `app/playlists/page.tsx` (6 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `app/podcasts/page.tsx` (11 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `app/releases/page.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `app/stats/page.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/activity/ActivityBody.tsx` (25 diff lines): section flags (D-030/D-170)
- `components/activity/ActivityFeed.tsx` (4 diff lines): section flags (D-030/D-170)
- `components/activity/DownloadRow.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/activity/ImportsTab.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/activity/NotificationsTab.tsx` (6 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/layout/ActivityPanel.tsx` (1 diff lines): section flags (D-030/D-170)
- `components/layout/AuthenticatedLayout.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/layout/BottomNavigation.tsx` (10 diff lines): section flags (D-030/D-170)
- `components/layout/MobileSidebar.tsx` (11 diff lines): section flags (D-030/D-170)
- `components/layout/Sidebar.tsx` (43 diff lines): section flags (D-170); active playlist/mix from `?id=` under Suspense (D-011)
- `components/layout/TVLayout.tsx` (18 diff lines): section flags (D-030/D-170)
- `components/layout/TopBar.tsx` (3 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/layout/navItems.ts` (14 diff lines): section flags (D-030/D-170)
- `components/player/UniversalPlayer.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/providers/ConditionalAudioProvider.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/whats-new/MoodMixChoice.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/whats-new/WhatsNewGate.tsx` (9 diff lines): section flags (D-030/D-170)
- `components/whats-new/WhatsNewWindow.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `components/whats-new/useVersionChip.ts` (5 diff lines): section flags (D-030/D-170)
- `features/album/components/AlbumHero.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/album/components/SimilarAlbums.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/album/hooks/useAlbumData.ts` (6 diff lines): id from `useSearchParams` (query route, D-011)
- `features/artist/components/AvailableAlbums.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/artist/components/Discography.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/artist/components/SimilarArtists.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/artist/hooks/useArtistData.ts` (6 diff lines): id from `useSearchParams` (query route, D-011)
- `features/audiobook/hooks/useAudiobookData.ts` (6 diff lines): id from `useSearchParams` (query route, D-011)
- `features/home/components/ArtistsGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/home/components/AudiobooksGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/home/components/ContinueListening.tsx` (10 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/home/components/FeaturedPlaylistsGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/home/components/MixesGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/home/hooks/useHomeData.ts` (16 diff lines): section flags (D-030/D-170)
- `features/library/components/LibraryGrid.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/podcast/components/CategoryChips.tsx` (3 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/components/CategoryShelf.tsx` (18 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/components/ListeningShelf.tsx` (4 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/components/NewEpisodes.tsx` (4 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/components/PodcastHero.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/podcast/components/PodcastSearch.tsx` (6 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/components/PodcastShelf.tsx` (4 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/hooks/usePodcastActions.ts` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/podcast/hooks/usePodcastData.ts` (8 diff lines): id from `useSearchParams` (query route, D-011)
- `features/podcast/types.ts` (9 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/podcast/utils.ts` (5 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `features/search/components/LibraryAlbumsGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/search/components/LibraryAudiobooksGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/search/components/LibraryPodcastsGrid.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/search/components/LibraryTracksList.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/search/components/SimilarArtistsGrid.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/search/components/TopResult.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/settings/SettingsScreen.tsx` (13 diff lines): local library tools section; podcasts sources section omitted (M6)
- `features/settings/components/sections/AccountSection.tsx` (4 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/settings/components/sections/ArtworkSection.tsx` (53 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/settings/components/sections/CacheSection.tsx` (18 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/settings/components/sections/SubsonicSection.tsx` (103 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/settings/components/sections/UserManagementSection.tsx` (2 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/settings/hooks/useSystemSettings.ts` (38 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/settings/pages.ts` (2 diff lines): local library tools section; podcasts sources section omitted (M6)
- `features/settings/types.ts` (9 diff lines): kima-rs settings: saved-key placeholders/Remove, auto-enrich lock, API-key tokens, path encoding (D-122, D-111, D-169)
- `features/vibe/TrackPanel.tsx` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/vibe/VibePanelSheet.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `features/vibe/tabs/NowPlayingTab.tsx` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `hooks/__tests__/useEventSourceReconnect.test.tsx` (12 diff lines): kima-rs `library:progress`/`autotag:progress` events
- `hooks/useEventSource.ts` (8 diff lines): kima-rs `library:progress`/`autotag:progress` events
- `hooks/useMediaInfo.ts` (8 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `hooks/useQueries.ts` (32 diff lines): kima-rs API surface: playlist add contract, autotag/organizer/attention calls, path encoding, `enabled` flags for gated sections
- `lib/__tests__/cover-art-mosaic.test.tsx` (94 diff lines): earlier slice's own version of the test (shared media slice)
- `lib/__tests__/mood-mix-choice.test.tsx` (3 diff lines): earlier slice's own version of the test (shared media slice)
- `lib/__tests__/mood-stations.test.ts` (11 diff lines): reads the upstream station table from `KIMA_UPSTREAM_DIR` and skips without it (kima-rs has no `backend/`)
- `lib/__tests__/playlist-rename-escape.test.tsx` (4 diff lines): imports the query-route playlist page
- `lib/__tests__/podcast-category-page.test.tsx` (4 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `lib/__tests__/podcastShowSort.test.ts` (4 diff lines): podcast slice (a20521a): local routes, episode-count sort, older similar-show shape
- `lib/api.ts` (292 diff lines): kima-rs API surface: playlist add contract, autotag/organizer/attention calls, path encoding, `enabled` flags for gated sections
- `lib/auth-context.tsx` (3 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `lib/features-context.tsx` (59 diff lines): section flags (D-030/D-170)
- `next.config.ts` (81 diff lines): static export config, fork lockfile, webstorage flag (D-007, D-171)
- `package-lock.json` (478 diff lines): static export config, fork lockfile, webstorage flag (D-007, D-171)
- `package.json` (17 diff lines): static export config, fork lockfile, webstorage flag (D-007, D-171)
- `tests/e2e/dogfood/podcast-browser.spec.ts` (16 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `tests/e2e/dogfood/release-112.spec.ts` (8 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `tests/e2e/dogfood/session.ts` (4 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `tests/e2e/dogfood/walkthrough.spec.ts` (12 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `tests/e2e/fixtures/test-helpers.ts` (2 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `tests/e2e/playlists.spec.ts` (10 diff lines): mechanical D-011 only: query-route links via the codemod, `encodeURIComponent` path segments, `usePathname` wrapper
- `vitest.config.ts` (4 diff lines): static export config, fork lockfile, webstorage flag (D-007, D-171)

### Local-only paths

`app/album/page.tsx`, `app/artist/loading.tsx`, `app/artist/page.tsx`, `app/audiobooks/book/page.tsx`, `app/audiobooks/layout.tsx`, `app/audiobooks/series/page.tsx`, `app/browse/playlists/view/page.tsx`, `app/changelog/layout.tsx`, `app/discover/layout.tsx`, `app/import/layout.tsx`, `app/mix/layout.tsx`, `app/mix/page.tsx`, `app/playlist/page.tsx`, `app/podcasts/category/page.tsx`, `app/podcasts/layout.tsx`, `app/podcasts/show/page.tsx`, `app/radio/layout.tsx`, `app/settings/attention/page.tsx`, `app/settings/autotag/album/page.tsx`, `app/settings/autotag/page.tsx`, `app/settings/library/page.tsx`, `app/settings/organize/page.tsx`, `app/settings/people/page.tsx`, `app/settings/server/page.tsx`, `app/settings/sources/page.tsx`, `app/share/SharePageClient.tsx`, `app/share/layout.tsx`, `app/share/page.tsx`, `app/stats/layout.tsx`, `components/FeatureGate.tsx`, `components/__tests__/FeatureGate.test.tsx`, `components/__tests__/upstream-headings.test.tsx`, `features/autotag/__tests__/CandidateList.test.tsx`, `features/autotag/__tests__/ChangesTable.test.tsx`, `features/autotag/__tests__/MappingDiff.test.tsx`, `features/autotag/__tests__/RunPanel.test.tsx`, `features/autotag/__tests__/TargetPicker.test.tsx`, `features/autotag/__tests__/admin.test.tsx`, `features/autotag/__tests__/api.test.ts`, `features/autotag/__tests__/list.test.tsx`, `features/autotag/__tests__/progress.test.ts`, `features/autotag/__tests__/review.test.tsx`, `features/autotag/components/CandidateList.tsx`, `features/autotag/components/ChangesTable.tsx`, `features/autotag/components/MappingDiff.tsx`, `features/autotag/components/RunPanel.tsx`, `features/autotag/components/TargetPicker.tsx`, `features/autotag/progress.ts`, `features/autotag/types.ts`, `features/organizer/__tests__/AlbumGroupedPreview.test.tsx`, `features/organizer/__tests__/entry-points.test.tsx`, `features/organizer/__tests__/organizer-api.test.ts`, `features/organizer/__tests__/organizer-page.test.tsx`, `features/organizer/__tests__/organizer.test.tsx`, `features/organizer/__tests__/runActions.test.ts`, `features/organizer/components/AlbumGroupedPreview.tsx`, `features/organizer/components/IssueChip.tsx`, `features/organizer/components/NotAvailable.tsx`, `features/organizer/components/ProgressBar.tsx`, `features/organizer/filters.ts`, `features/organizer/progress.ts`, `features/organizer/runActions.ts`, `features/organizer/types.ts`, `features/podcast/components/SimilarPodcasts.tsx`, `features/podcast/routes.ts`, `features/settings/__tests__/settings-pages.test.ts`, `features/settings/__tests__/systemSettingsBody.test.ts`, `features/settings/__tests__/useSystemSettings.test.tsx`, `features/settings/components/sections/LibraryToolsSection.tsx`, `features/settings/components/sections/__tests__/ArtworkSection.test.tsx`, `features/settings/components/sections/__tests__/CacheSection.test.tsx`, `features/settings/components/sections/__tests__/SubsonicSection.test.tsx`, `features/settings/systemSettingsBody.ts`, `lib/__tests__/api-path-encoding.test.ts`, `lib/__tests__/playlist-contract.test.ts`, `lib/__tests__/podcast-routes.test.ts`, `lib/__tests__/stats-keyboard.test.tsx`, `lib/playlist-contract.ts`, `lib/section-flags.ts`, `lib/use-pathname.ts`, `playwright.kima-rs.config.ts`, `public/manifest.webmanifest`, `tests/e2e/kima-rs/autotag.spec.ts`, `tests/e2e/kima-rs/enrichment.spec.ts`, `tests/e2e/kima-rs/library.spec.ts`, `tests/e2e/kima-rs/smoke.spec.ts`, `tests/e2e/kima-rs/subsonic.spec.ts`, `lib/__tests__/account-deletion-hidden.test.tsx`, `lib/__tests__/section-flags-requests.test.tsx`

### Exact matches by directory

- `(root)`: 9
- `app/audiobooks`: 1
- `app/changelog`: 1
- `app/collection`: 2
- `app/discover`: 1
- `app/error.tsx`: 1
- `app/favicon.ico`: 1
- `app/global-error.tsx`: 1
- `app/globals.css`: 1
- `app/layout.tsx`: 1
- `app/loading.tsx`: 1
- `app/login`: 1
- `app/not-found.tsx`: 1
- `app/onboarding`: 1
- `app/playlists`: 1
- `app/podcasts`: 1
- `app/radio`: 1
- `app/search`: 2
- `app/settings`: 3
- `app/sync`: 1
- `app/vibe`: 1
- `assets/SpotIcon.png`: 1
- `assets/icon.png`: 1
- `components/MetadataEditControls.tsx`: 1
- `components/MetadataEditor.tsx`: 1
- `components/MoodMixer.tsx`: 1
- `components/ServiceWorkerRegistration.tsx`: 1
- `components/__tests__`: 2
- `components/activity`: 5
- `components/cards`: 2
- `components/enrichment`: 1
- `components/layout`: 7
- `components/lyrics`: 2
- `components/player`: 11
- `components/providers`: 3
- `components/ui`: 23
- `components/whats-new`: 6
- `features/album`: 4
- `features/artist`: 7
- `features/audiobook`: 5
- `features/discover`: 9
- `features/home`: 6
- `features/library`: 9
- `features/playlist`: 1
- `features/podcast`: 5
- `features/search`: 9
- `features/settings`: 26
- `features/spoken`: 2
- `features/stats`: 22
- `features/vibe`: 23
- `hooks/__tests__`: 14
- `hooks/useActivityPanel.ts`: 1
- `hooks/useClickOutside.ts`: 1
- `hooks/useDialog.ts`: 1
- `hooks/useDoubleTap.ts`: 1
- `hooks/useImageColor.ts`: 1
- `hooks/useInView.ts`: 1
- `hooks/useKeyboardShortcuts.ts`: 1
- `hooks/useLeaveDeletedPlaylist.ts`: 1
- `hooks/useLibrarySync.ts`: 1
- `hooks/useLiveReporting.ts`: 1
- `hooks/useLyricsSync.ts`: 1
- `hooks/useLyricsToggle.tsx`: 1
- `hooks/useMediaQuery.ts`: 1
- `hooks/useMediaSession.ts`: 1
- `hooks/useMetadataDisplay.ts`: 1
- `hooks/useNotifications.ts`: 1
- `hooks/usePlayReporting.ts`: 1
- `hooks/usePlaybackProgress.ts`: 1
- `hooks/usePlayerControls.ts`: 1
- `hooks/usePreview.ts`: 1
- `hooks/useSleepTimer.ts`: 1
- `hooks/useSoundRebuild.ts`: 1
- `hooks/useTVNavigation.ts`: 1
- `hooks/useTrackFormat.ts`: 1
- `hooks/useVibeToggle.ts`: 1
- `lib/__tests__`: 70
- `lib/acquire.ts`: 1
- `lib/activity-panel-settings-context.tsx`: 1
- `lib/audio-context.tsx`: 1
- `lib/audio-controller-context.tsx`: 1
- `lib/audio-controller.ts`: 1
- `lib/audio-controls-context.tsx`: 1
- `lib/audio-engine-policy.ts`: 1
- `lib/audio-hooks.tsx`: 1
- `lib/audio-playback-context.tsx`: 1
- `lib/audio-state-context.tsx`: 1
- `lib/backend-url.ts`: 1
- `lib/book-session.ts`: 1
- `lib/brandGradient.ts`: 1
- `lib/clipboard.ts`: 1
- `lib/cn.ts`: 1
- `lib/cover-art.ts`: 1
- `lib/download-context.tsx`: 1
- `lib/format.ts`: 1
- `lib/lyrics-utils.ts`: 1
- `lib/mbid.ts`: 1
- `lib/motion.ts`: 1
- `lib/notify.ts`: 1
- `lib/pagination.ts`: 1
- `lib/playlist-track.ts`: 1
- `lib/podcast-episode.ts`: 1
- `lib/query-client.tsx`: 1
- `lib/query-events.ts`: 1
- `lib/queue-edit.ts`: 1
- `lib/relative-time.ts`: 1
- `lib/requested-views.ts`: 1
- `lib/scroll.ts`: 1
- `lib/search-result-store.ts`: 1
- `lib/shuffle.ts`: 1
- `lib/station-refill.ts`: 1
- `lib/storage-keys.ts`: 1
- `lib/tabListKeys.ts`: 1
- `lib/track-format.ts`: 1
- `lib/track-title.ts`: 1
- `lib/tv-utils.ts`: 1
- `lib/version.ts`: 1
- `public/assets`: 11
- `public/fonts`: 1
- `public/offline.html`: 1
- `public/sw.js`: 1
- `tests/e2e`: 28

<details><summary>All 420 exact paths</summary>

- `.dockerignore`
- `.gitignore`
- `Dockerfile`
- `app/audiobooks/loading.tsx`
- `app/changelog/page.tsx`
- `app/collection/loading.tsx`
- `app/collection/page.tsx`
- `app/discover/page.tsx`
- `app/error.tsx`
- `app/favicon.ico`
- `app/global-error.tsx`
- `app/globals.css`
- `app/layout.tsx`
- `app/loading.tsx`
- `app/login/page.tsx`
- `app/not-found.tsx`
- `app/onboarding/page.tsx`
- `app/playlists/loading.tsx`
- `app/podcasts/loading.tsx`
- `app/radio/page.tsx`
- `app/search/loading.tsx`
- `app/search/page.tsx`
- `app/settings/layout.tsx`
- `app/settings/loading.tsx`
- `app/settings/page.tsx`
- `app/sync/page.tsx`
- `app/vibe/page.tsx`
- `assets/SpotIcon.png`
- `assets/icon.png`
- `components/MetadataEditControls.tsx`
- `components/MetadataEditor.tsx`
- `components/MoodMixer.tsx`
- `components/ServiceWorkerRegistration.tsx`
- `components/__tests__/escapeMarksKeyUsed.test.tsx`
- `components/__tests__/whatsNewGateUser.test.tsx`
- `components/activity/ActiveDownloadsTab.tsx`
- `components/activity/ActivityIcon.tsx`
- `components/activity/DiscoverSettingsTab.tsx`
- `components/activity/HistoryTab.tsx`
- `components/activity/ListHeader.tsx`
- `components/cards/MediaCard.tsx`
- `components/cards/TileCard.tsx`
- `components/enrichment/BothAtOnceSwitch.tsx`
- `components/layout/AuthShell.tsx`
- `components/layout/PageHeader.tsx`
- `components/layout/UnifiedPanel.tsx`
- `components/layout/__tests__/ActivityPanelToggle.test.tsx`
- `components/layout/__tests__/TVLayoutBack.test.tsx`
- `components/layout/__tests__/UnifiedPanelTabs.test.tsx`
- `components/layout/panelChrome.tsx`
- `components/lyrics/LyricsPanel.tsx`
- `components/lyrics/LyricsView.tsx`
- `components/player/FullPlayer.tsx`
- `components/player/KeyboardShortcutsTooltip.tsx`
- `components/player/MediaControlsHandler.tsx`
- `components/player/MiniPlayer.tsx`
- `components/player/OverlayPlayer.tsx`
- `components/player/PlayPauseButton.tsx`
- `components/player/PlayerCover.tsx`
- `components/player/SeekSlider.tsx`
- `components/player/SleepTimer.tsx`
- `components/player/VibeToggleButton.tsx`
- `components/player/__tests__/SleepTimerEscape.test.tsx`
- `components/providers/AudioErrorBoundary.tsx`
- `components/providers/ErrorBoundary.tsx`
- `components/providers/GlobalErrorBoundary.tsx`
- `components/ui/Badge.tsx`
- `components/ui/Button.tsx`
- `components/ui/CachedImage.tsx`
- `components/ui/Card.tsx`
- `components/ui/ConfirmDialog.tsx`
- `components/ui/EmptyState.tsx`
- `components/ui/GalaxyBackground.tsx`
- `components/ui/GradientSpinner.tsx`
- `components/ui/HeroBackdrop.tsx`
- `components/ui/HorizontalCarousel.tsx`
- `components/ui/InlineStatus.tsx`
- `components/ui/KimaMark.tsx`
- `components/ui/LoadingScreen.tsx`
- `components/ui/MediaActionBar.tsx`
- `components/ui/MediaHero.tsx`
- `components/ui/Modal.tsx`
- `components/ui/NumberedRow.tsx`
- `components/ui/PageMessage.tsx`
- `components/ui/Pagination.tsx`
- `components/ui/PlaylistSelector.tsx`
- `components/ui/SectionHeader.tsx`
- `components/ui/SideTooltip.tsx`
- `components/ui/TrackRow.tsx`
- `components/whats-new/ChoiceButton.tsx`
- `components/whats-new/RefileAction.tsx`
- `components/whats-new/ReleaseBody.tsx`
- `components/whats-new/ReleaseLinks.tsx`
- `components/whats-new/WipeAction.tsx`
- `components/whats-new/types.ts`
- `docker-entrypoint.sh`
- `eslint.config.mjs`
- `features/album/components/AlbumActionBar.tsx`
- `features/album/components/TrackList.tsx`
- `features/album/hooks/useAlbumActions.ts`
- `features/album/types.ts`
- `features/artist/components/ArtistActionBar.tsx`
- `features/artist/components/ArtistBio.tsx`
- `features/artist/components/ArtistHero.tsx`
- `features/artist/components/PopularTracks.tsx`
- `features/artist/hooks/useArtistActions.ts`
- `features/artist/hooks/useDownloadActions.ts`
- `features/artist/types.ts`
- `features/audiobook/components/AudiobookActionBar.tsx`
- `features/audiobook/components/AudiobookHero.tsx`
- `features/audiobook/components/SectionList.tsx`
- `features/audiobook/hooks/useAudiobookActions.ts`
- `features/audiobook/types.ts`
- `features/discover/components/DiscoverActionBar.tsx`
- `features/discover/components/DiscoverHero.tsx`
- `features/discover/components/HowItWorks.tsx`
- `features/discover/components/TrackList.tsx`
- `features/discover/components/UnavailableAlbums.tsx`
- `features/discover/constants.ts`
- `features/discover/hooks/useDiscoverActions.ts`
- `features/discover/hooks/useDiscoverData.ts`
- `features/discover/types.ts`
- `features/home/components/DeezerIcon.tsx`
- `features/home/components/HomeHero.tsx`
- `features/home/components/LibraryRadioStations.tsx`
- `features/home/components/StationCard.tsx`
- `features/home/radioData.ts`
- `features/home/types.ts`
- `features/library/components/CollectionMasthead.tsx`
- `features/library/components/DeleteLibraryItemDialog.tsx`
- `features/library/components/LibraryTabs.tsx`
- `features/library/components/LibraryToolbar.tsx`
- `features/library/components/RefinePanel.tsx`
- `features/library/components/TracksList.tsx`
- `features/library/hooks/useLibraryActions.ts`
- `features/library/types.ts`
- `features/library/useLibraryVibe.ts`
- `features/playlist/coverMosaic.ts`
- `features/podcast/components/ContinueListening.tsx`
- `features/podcast/components/EpisodeList.tsx`
- `features/podcast/components/PodcastActionBar.tsx`
- `features/podcast/components/PreviewEpisodes.tsx`
- `features/podcast/hooks/usePlayEpisode.ts`
- `features/search/components/AliasResolutionBanner.tsx`
- `features/search/components/SearchFilters.tsx`
- `features/search/components/SoulseekBrowser.tsx`
- `features/search/components/TVSearchInput.tsx`
- `features/search/components/UnifiedSongsList.tsx`
- `features/search/components/soulseekHelpers.tsx`
- `features/search/hooks/useSearchData.ts`
- `features/search/hooks/useSoulseekSearch.ts`
- `features/search/types.ts`
- `features/settings/SettingsState.tsx`
- `features/settings/components/sections/AudiobookshelfSection.tsx`
- `features/settings/components/sections/CorruptTracksSection.tsx`
- `features/settings/components/sections/DownloadPreferencesSection.tsx`
- `features/settings/components/sections/LidarrSection.tsx`
- `features/settings/components/sections/PlaybackSection.tsx`
- `features/settings/components/sections/PodcastsSection.tsx`
- `features/settings/components/sections/ScrobblingSection.tsx`
- `features/settings/components/sections/SoulseekSection.tsx`
- `features/settings/components/sections/StoragePathsSection.tsx`
- `features/settings/components/ui/ConnectionRows.tsx`
- `features/settings/components/ui/ConnectionTestButton.tsx`
- `features/settings/components/ui/CopyButton.tsx`
- `features/settings/components/ui/SettingsInput.tsx`
- `features/settings/components/ui/SettingsLayout.tsx`
- `features/settings/components/ui/SettingsRow.tsx`
- `features/settings/components/ui/SettingsSection.tsx`
- `features/settings/components/ui/SettingsSelect.tsx`
- `features/settings/components/ui/SettingsSidebar.tsx`
- `features/settings/components/ui/SettingsToggle.tsx`
- `features/settings/components/ui/index.ts`
- `features/settings/hooks/settleFor.ts`
- `features/settings/hooks/useConnectionTest.ts`
- `features/settings/hooks/useSettingsData.ts`
- `features/settings/hooks/useTwoFactor.ts`
- `features/settings/hooks/useWorkerSetting.ts`
- `features/spoken/ColorWash.tsx`
- `features/spoken/SpokenHero.tsx`
- `features/stats/PlayCountLine.tsx`
- `features/stats/components/Chapters.tsx`
- `features/stats/components/Coverage.tsx`
- `features/stats/components/Decades.tsx`
- `features/stats/components/EnergyCurve.tsx`
- `features/stats/components/FalseStarts.tsx`
- `features/stats/components/Finding.tsx`
- `features/stats/components/FitPanel.tsx`
- `features/stats/components/KeyWheel.tsx`
- `features/stats/components/ListeningClock.tsx`
- `features/stats/components/ServerPanel.tsx`
- `features/stats/components/ShapePlot.tsx`
- `features/stats/components/Shortcuts.tsx`
- `features/stats/components/Signature.tsx`
- `features/stats/components/SourceSplit.tsx`
- `features/stats/components/SpokenWord.tsx`
- `features/stats/components/StatTrackRow.tsx`
- `features/stats/format.ts`
- `features/stats/trackLookup.ts`
- `features/stats/types.ts`
- `features/stats/usePlaySlice.ts`
- `features/stats/useTrackMood.ts`
- `features/vibe/MoodLegend.tsx`
- `features/vibe/ScenePanel.tsx`
- `features/vibe/TrackFinder.tsx`
- `features/vibe/TrackMenu.tsx`
- `features/vibe/VibeAlchemy.tsx`
- `features/vibe/VibeSongPath.tsx`
- `features/vibe/VibeToolbar.tsx`
- `features/vibe/librarySearch.ts`
- `features/vibe/map/VibeMapScene.tsx`
- `features/vibe/map/moodAnchors.ts`
- `features/vibe/map/queueChart.ts`
- `features/vibe/map/releaseSharedGeometry.ts`
- `features/vibe/map/ringMark.ts`
- `features/vibe/map/screenPick.ts`
- `features/vibe/map/skyField.ts`
- `features/vibe/map/starShader.ts`
- `features/vibe/map/trackField.ts`
- `features/vibe/mapUtils.ts`
- `features/vibe/tabs/QueueTab.tsx`
- `features/vibe/types.ts`
- `features/vibe/useTrackFinder.ts`
- `features/vibe/useVibeMap.ts`
- `features/vibe/useVibeSearch.tsx`
- `healthcheck.js`
- `hooks/__tests__/playlistEditsInFlight.test.tsx`
- `hooks/__tests__/useClickOutside.test.tsx`
- `hooks/__tests__/useEventSourcePlaylists.test.tsx`
- `hooks/__tests__/useLeaveDeletedPlaylist.test.tsx`
- `hooks/__tests__/useLiveReporting.test.ts`
- `hooks/__tests__/useLiveReportingHook.test.tsx`
- `hooks/__tests__/useLyricsSync.test.tsx`
- `hooks/__tests__/usePlayReporting.test.ts`
- `hooks/__tests__/usePlaybackProgress.test.tsx`
- `hooks/__tests__/usePreview.test.tsx`
- `hooks/__tests__/useSetPlaylistReverseMutation.test.tsx`
- `hooks/__tests__/useTVNavigationBack.test.tsx`
- `hooks/__tests__/useTVNavigationLoadingRow.test.tsx`
- `hooks/__tests__/useTVNavigationSections.test.tsx`
- `hooks/useActivityPanel.ts`
- `hooks/useClickOutside.ts`
- `hooks/useDialog.ts`
- `hooks/useDoubleTap.ts`
- `hooks/useImageColor.ts`
- `hooks/useInView.ts`
- `hooks/useKeyboardShortcuts.ts`
- `hooks/useLeaveDeletedPlaylist.ts`
- `hooks/useLibrarySync.ts`
- `hooks/useLiveReporting.ts`
- `hooks/useLyricsSync.ts`
- `hooks/useLyricsToggle.tsx`
- `hooks/useMediaQuery.ts`
- `hooks/useMediaSession.ts`
- `hooks/useMetadataDisplay.ts`
- `hooks/useNotifications.ts`
- `hooks/usePlayReporting.ts`
- `hooks/usePlaybackProgress.ts`
- `hooks/usePlayerControls.ts`
- `hooks/usePreview.ts`
- `hooks/useSleepTimer.ts`
- `hooks/useSoundRebuild.ts`
- `hooks/useTVNavigation.ts`
- `hooks/useTrackFormat.ts`
- `hooks/useVibeToggle.ts`
- `lib/__tests__/activity-areas.test.tsx`
- `lib/__tests__/activity-feed.test.tsx`
- `lib/__tests__/activity-panel-leave-settings.test.tsx`
- `lib/__tests__/animation-names-exist.test.ts`
- `lib/__tests__/api-detail-not-found.test.ts`
- `lib/__tests__/api-leaving-page.test.ts`
- `lib/__tests__/api-mixes-time-zone.test.ts`
- `lib/__tests__/api-stream-urls.test.ts`
- `lib/__tests__/api-token-refresh.test.ts`
- `lib/__tests__/audio-controller-end.test.ts`
- `lib/__tests__/audio-controller-gapless.test.ts`
- `lib/__tests__/audio-controller-prime.test.ts`
- `lib/__tests__/audio-engine-policy.test.ts`
- `lib/__tests__/book-session.test.ts`
- `lib/__tests__/both-at-once-switch.test.tsx`
- `lib/__tests__/brand-gradient.test.ts`
- `lib/__tests__/changelog-page.test.tsx`
- `lib/__tests__/collection-masthead.test.tsx`
- `lib/__tests__/cover-sizes.test.tsx`
- `lib/__tests__/delete-own-account.test.tsx`
- `lib/__tests__/dialog-escape.test.tsx`
- `lib/__tests__/discover-data-loading.test.tsx`
- `lib/__tests__/discover-hero-week-date.test.tsx`
- `lib/__tests__/download-context.test.tsx`
- `lib/__tests__/enrichment-mood-step.test.tsx`
- `lib/__tests__/features-context.test.tsx`
- `lib/__tests__/genre-station-style.test.ts`
- `lib/__tests__/gpu-status-line.test.ts`
- `lib/__tests__/helpers/audioControllerEnv.ts`
- `lib/__tests__/helpers/dom.ts`
- `lib/__tests__/hero-meta-zero.test.tsx`
- `lib/__tests__/home-radio-row.test.tsx`
- `lib/__tests__/home-sections-load-alone.test.tsx`
- `lib/__tests__/library-delete-placement.test.tsx`
- `lib/__tests__/library-vibe.test.tsx`
- `lib/__tests__/mbid.test.ts`
- `lib/__tests__/metadata-edit-admin.test.tsx`
- `lib/__tests__/metadata-editor-genres.test.tsx`
- `lib/__tests__/metadata-editor-overlay.test.tsx`
- `lib/__tests__/normalize-nfc.test.ts`
- `lib/__tests__/page-end.test.tsx`
- `lib/__tests__/pagination.test.ts`
- `lib/__tests__/panel-tabs-keys.test.tsx`
- `lib/__tests__/playback-state-no-echo.test.tsx`
- `lib/__tests__/playbackLength.test.ts`
- `lib/__tests__/playlist-move.test.ts`
- `lib/__tests__/podcast-category-shelf.test.tsx`
- `lib/__tests__/podcast-summary-text.test.ts`
- `lib/__tests__/popular-tracks-preview.test.tsx`
- `lib/__tests__/query-cache-signout.test.tsx`
- `lib/__tests__/queue-edit.test.ts`
- `lib/__tests__/queue-limit-input.test.tsx`
- `lib/__tests__/queue-tab-long.test.tsx`
- `lib/__tests__/refile-action.test.tsx`
- `lib/__tests__/refine-panel.test.tsx`
- `lib/__tests__/requested-views.test.ts`
- `lib/__tests__/settings-nav-active.test.tsx`
- `lib/__tests__/shown-title.test.tsx`
- `lib/__tests__/shuffle-state.test.ts`
- `lib/__tests__/side-panel-focus-scroll.test.tsx`
- `lib/__tests__/sound-rebuild.test.tsx`
- `lib/__tests__/station-covers.test.ts`
- `lib/__tests__/station-refill-context.test.tsx`
- `lib/__tests__/station-refill.test.ts`
- `lib/__tests__/sync-page-failed.test.tsx`
- `lib/__tests__/unified-panel-settings.test.tsx`
- `lib/__tests__/version-chip.test.tsx`
- `lib/__tests__/vibe-map.test.ts`
- `lib/__tests__/vibe-release-shared-geometry.test.ts`
- `lib/__tests__/whats-new-window.test.tsx`
- `lib/acquire.ts`
- `lib/activity-panel-settings-context.tsx`
- `lib/audio-context.tsx`
- `lib/audio-controller-context.tsx`
- `lib/audio-controller.ts`
- `lib/audio-controls-context.tsx`
- `lib/audio-engine-policy.ts`
- `lib/audio-hooks.tsx`
- `lib/audio-playback-context.tsx`
- `lib/audio-state-context.tsx`
- `lib/backend-url.ts`
- `lib/book-session.ts`
- `lib/brandGradient.ts`
- `lib/clipboard.ts`
- `lib/cn.ts`
- `lib/cover-art.ts`
- `lib/download-context.tsx`
- `lib/format.ts`
- `lib/lyrics-utils.ts`
- `lib/mbid.ts`
- `lib/motion.ts`
- `lib/notify.ts`
- `lib/pagination.ts`
- `lib/playlist-track.ts`
- `lib/podcast-episode.ts`
- `lib/query-client.tsx`
- `lib/query-events.ts`
- `lib/queue-edit.ts`
- `lib/relative-time.ts`
- `lib/requested-views.ts`
- `lib/scroll.ts`
- `lib/search-result-store.ts`
- `lib/shuffle.ts`
- `lib/station-refill.ts`
- `lib/storage-keys.ts`
- `lib/tabListKeys.ts`
- `lib/track-format.ts`
- `lib/track-title.ts`
- `lib/tv-utils.ts`
- `lib/version.ts`
- `playwright.config.ts`
- `postcss.config.mjs`
- `public/assets/icons/icon-128.webp`
- `public/assets/icons/icon-192.webp`
- `public/assets/icons/icon-256.webp`
- `public/assets/icons/icon-48.webp`
- `public/assets/icons/icon-512.webp`
- `public/assets/icons/icon-72.webp`
- `public/assets/icons/icon-96.webp`
- `public/assets/images/SpotIcon.png`
- `public/assets/images/apple-touch-icon.png`
- `public/assets/images/favicon-192.png`
- `public/assets/images/kima.webp`
- `public/fonts/montserrat-latin.woff2`
- `public/offline.html`
- `public/sw.js`
- `tests/e2e/campaign/native.spec.ts`
- `tests/e2e/dogfood/apiKey.ts`
- `tests/e2e/dogfood/enrichment-flow.spec.ts`
- `tests/e2e/dogfood/features.spec.ts`
- `tests/e2e/dogfood/podcast-removal.spec.ts`
- `tests/e2e/dogfood/podcastReplies.ts`
- `tests/e2e/dogfood/preflight.ts`
- `tests/e2e/dogfood/session-reconnects.spec.ts`
- `tests/e2e/dogfood/stats.spec.ts`
- `tests/e2e/enrichment-cycle.spec.ts`
- `tests/e2e/fixtures/picker.ts`
- `tests/e2e/fixtures/shared.ts`
- `tests/e2e/fixtures/stackDb.ts`
- `tests/e2e/fixtures/target.ts`
- `tests/e2e/full-ux-audit.spec.ts`
- `tests/e2e/global.setup.ts`
- `tests/e2e/import-url.spec.ts`
- `tests/e2e/keep-alive.spec.ts`
- `tests/e2e/podcast-next.spec.ts`
- `tests/e2e/predeploy/auth.spec.ts`
- `tests/e2e/predeploy/integrations.spec.ts`
- `tests/e2e/predeploy/library.spec.ts`
- `tests/e2e/predeploy/playback.spec.ts`
- `tests/e2e/queue.spec.ts`
- `tests/e2e/security.spec.ts`
- `tests/e2e/smoke.spec.ts`
- `tests/e2e/vibe-phone.spec.ts`
- `tests/e2e/vibe.spec.ts`
- `tsconfig.json`

</details>

## Decisions recorded

- `/queue` and `/podcasts/genre`: removed as upstream removed them (D-169). Upstream redirects `/queue` to `/` in
  `next.config.ts`; the static export cannot, and axum has no rule, so both answer 404 (Playwright asserts it).
- Settings `[page]`: one exported page per slug (`/settings/library`, `/settings/sources`, `/settings/server`,
  `/settings/people`; `/settings` is "You"), so `/settings/${slug}` links stay path links; the codemod has a
  regression test that leaves them alone.
- Feature gates (D-170): new section flags `stats` and `changelog`; nav items carry their flag; hidden by default
  until kima-rs sends them.
- Podcasts AI filter (owner spec, M6): not implemented here; backend and UI wait for M6.
- npm dependencies stay at the fork's lockfile (D-171).

## Commits

`11870b2` codemod rules and tests; `e66a39f` the refresh itself; `be6db6f` kima-rs Playwright specs; `75526f4`
request gating follow-up; then this inventory and the M9 baselines. The refresh is one commit rather than one per
subsystem: `lib/api.ts`, `hooks/useQueries.ts` and the feature flags are shared by every subsystem, and a trial
split (core libraries and hooks on top of the old components) left type errors in 32 files.

## Gates (frontend/, Node v26.7.0)

- `npx tsc --noEmit`: exit 0 (first run after the apply: syntax errors in one file, then 40 diagnostics; the type
  error list was the conflict map).
- `npm run test:unit`: 125 files, 808 passed, 5 skipped (`mood-stations` without `KIMA_UPSTREAM_DIR`; with it set
  to an upstream checkout the 5 pass). First run after the apply: 34 failed in 11 files (123 files then).
- `npm run build`: exit 0, 40 static-generation units, 41 HTML files under `out/` (previously 41 units / 42 files:
  `-/queue`, `-/podcasts/genre`, `+/changelog`).
- `node --test tools/frontend-route-codemod.test.mjs`: 6 passed (category rule and `usePathname` rewrite failed
  before the change).
- kima-rs Playwright (`playwright.kima-rs.config.ts`, kima `target/debug/kima` from this worktree serving `out/`,
  isolated Postgres, generated fixture library, `KIMA_IDENTIFY=off`): 13 passed (also after the request-gating
  follow-up). Before the spec updates: 8 passed,
  4 failed (settings split into pages, the always-open Apps section). The two new checks (stats/changelog hidden,
  `/queue` and `/podcasts/genre` 404) failed against an export of `8a8aa1f` and pass on the refresh.

## Browser and visual checks

Run on the final tree: kima built from this branch (`target/debug/kima`), kima-web built with vertigo 0.13.4 from
this branch, the legacy reference from the committed `frontend/` as a `next dev` comparison copy in scratch (never
a product path), the seeded visual library on the isolated database. Per page, `diff % / ink % / SSIM` at
1440x900, 1024x768, 390x844 (thresholds 1 % / 8 % / 0.93):

| page | 1440x900 | 1024x768 | 390x844 |
|---|---|---|---|
| login | 0.00 / 0.0 / 1.00 | 0.00 / 0.0 / 1.00 | 0.00 / 0.0 / 1.00 |
| setup | 8.38 / 32.4 / 0.14 | 13.87 / 39.8 / 0.16 | 16.59 / 51.5 / 0.16 |
| artists | 3.48 / 32.8 / 0.34 | 4.28 / 28.1 / 0.37 | 7.19 / 35.6 / 0.35 |
| albums | 22.41 / 63.4 / 0.24 | 29.85 / 64.4 / 0.40 | 24.75 / 41.4 / 0.33 |
| tracks | 5.42 / 44.5 / 0.38 | 6.89 / 40.0 / 0.40 | 11.93 / 49.2 / 0.34 |
| search | 1.94 / 32.6 / 0.42 | 2.09 / 25.4 / 0.56 | 4.39 / 45.6 / 0.44 |
| playlists | 6.43 / 63.3 / 0.32 | 10.98 / 66.5 / 0.36 | 15.70 / 73.9 / 0.31 |
| playlist | 4.56 / 47.2 / 0.41 | 1.92 / 14.6 / 0.67 | 6.72 / 37.9 / 0.51 |
| settings | 2.43 / 37.1 / 0.41 | 2.89 / 31.2 / 0.42 | 4.78 / 56.7 / 0.41 |
| player | 10.56 / 45.9 / 0.34 | 22.60 / 53.5 / 0.37 | 19.99 / 48.5 / 0.35 |

Inspected by eye (legacy, kima-web): login is the same page in both (the 0.00 is real, not a blank capture);
albums 1440 shows the v1.12.3 icon rail and library panel, the smaller page title and the inline sort/page-size
controls against kima-web's v1.11 "navigation index" and "refine" button; settings 1440 shows the five-page
navigation (you/library/sources/server/people) and the Apps section against kima-web's single account section;
player 1024 shows the delete action and playing indicator in track rows; setup shows the new two-step stepper;
playlists 390 shows the import buttons and a three-tile mosaic. The token and layout changes are written into the
M9 plan's Task 15b Step 1. These are new baselines for kima-web, not parity.

API calls the refreshed reference still makes that kima-rs answers 404 (legacy dev-server log of the final run):
`/notifications` (48), `/vibe/mood-summary` (24), `/notifications/downloads/history` (16), `/auth/2fa/status` (6),
`/soulseek/status` (6), `/search/discover` (6), `/vibe/mood/:id` (1). `/stats` and `/whats-new/*` no longer
appear (D-170 follow-up).

## Not done / open

- Visual parity of kima-web with the v1.12.3 reference (M9 Task 15b / M10): every page but login is over the
  bounds above.
- The 404s listed above are not gated; the playlists page's import buttons show although `imports` is off (as
  before this refresh).
- Podcasts AI-label filter and the podcast backend: M6.
- Independent review of these commits and hosted CI on a pushed SHA: not done (nothing pushed).

## Progress note (2026-11-11)

Done in worktree `kima-rust-hub-m-sync-frontend`, branch `feature/m-sync-frontend`, not pushed. Next command for a
resuming writer: `git log --oneline 8a8aa1f..HEAD` and, to re-run the browser checks,
`python3 ~/.hermes/cache/scratch/t19-evidence/run_e2e.py` (needs the isolated DB URL file and a
`t19-target/debug/kima` build; scratch is temporary).
