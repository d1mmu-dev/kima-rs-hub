# M-SYNC API fixture capture (Task 18, steps 1–3)

Run `env -u DATABASE_URL -u PGDATABASE KIMA_CAPTURE_SET=<sets> tools/capture-api-fixtures.sh`
from this checkout. `KIMA_CAPTURE_SET` (required since M6 Task 19) is a comma list of
`msync`, `mstats`, `m6`: every capture runs, but only the named sets are copied into
`crates/kima-api-types/tests/fixtures/`, and the run refuses to start when one of those
files has uncommitted changes. The original M-SYNC run below predates the variable.
The script rejects caller DB variables, starts a **new** labelled pgvector:pg17 container
with Docker-assigned loopback port and ephemeral data, verifies `kima|kima` over the
host connection, builds this checkout's `kima-server`, generates the sine-tone library
with `tools/make-fixture-library.sh`, and starts an HTTP server on loopback with
`KIMA_NET_DENY_REAL=1`, `KIMA_IDENTIFY=off`, `SQLX_OFFLINE=true`. It registers and signs
in `fixtureadmin`, scans the library, creates a playlist with two scanned tracks,
updates `reverseOrder=true`, and captures the actual HTTP JSON using `jq -eS`.
Only after all requests succeed are the nine staged responses copied into fixtures.
Trap cleanup removes the server, data and DB container even after failure. No
application or development DB is used. Dynamic UUIDs and timestamps remain the
server's actual values, so repeated runs need not yield byte-identical files.

Captured against `feature/m-sync-e` source HEAD `6a89022e8149df6709bbeb82e5704d12ec0ed092`
(before this task's commit), from run-owned `msync18-capture-1794235846-2466375`,
`127.0.0.1:32776`, database/role `kima|kima` (removed on exit; verified with `docker ps -a`):

| Fixture | HTTP route | Capture condition |
| --- | --- | --- |
| me_response | GET /api/auth/me | onboarded admin; no `enrichmentSettings` |
| settings_response | GET /api/settings | signed-in admin |
| public_config_response | GET /api/system-settings/public-config | default empty public URL, `canAcquire=false` |
| library_removals_response | GET /api/library/removals | fresh scan; zero removals |
| playlist_create_response | POST /api/playlists | Road trip |
| playlist_update_response | PUT /api/playlists/{id} | `reverseOrder=true` |
| playlists_list_response | GET /api/playlists | two scanned tracks |
| playlist_detail_response | GET /api/playlists/{id} | stored reverse order, two items |
| enrichment_rerun_response | POST /api/enrichment/rerun | `step=identification`, real successful reset |

All nine listed routes exist; **no unavailable route was substituted with a
fabricated success**. The create/list/detail fixtures have unrelated dynamic id/time
diffs versus the prior capture; their contents are not independent upstream parity
claims. Subsonic snapshots were checked on a different fresh isolated pgvector
container (`msync18-snap-1794235452-2455319`, port 32773, `kima|kima`, removed)
with `INSTA_UPDATE=no`; all 32 tests passed and **zero snapshots changed**. The
expected plan diffs were aspirational, not observed on this fixture and source.

AcoustID examples under `crates/kima-ident/tests/fixtures/acoustid/` are
hand-written synthetic shapes, **not** these live API captures; see their README.
Task 18 step 4's double full-workspace test gate is explicitly outside this slice.

## M-STATS stats fixtures (plan `2026-11-11-m-stats.md`, Task 11)

After the M-SYNC captures the script records the stats history and captures the ten
`/api/stats` responses with `jq -eS`, staged then copied like the others:

1. `POST /api/plays` for `First` (900 s, `secondsListened: 900`, recorded `heard = true`)
   and for `Second` (6 s, `secondsListened: 6`; the listen rule records **no** play for it,
   observed).
2. Two `heard = false` rows by SQL (`gen_random_uuid()` ids; no kima-rs writer produces
   `heard = false`, and only such rows can be a provable skip): `First` one day ago and
   `Third` (6 s) 60 s later, so `First`'s listen is `skipped` and `Third`'s is too short.
3. `POST /stats/false-starts/ignore` for `Second`, then the audit, `collapse`, `remove`
   for `First` and `restore` for `Second`.

| Fixture | HTTP route | Capture condition |
| --- | --- | --- |
| stats_response_s1_response | GET /api/stats?period=all&tz=UTC | admin; 3 rows; no analysis (`shape: []`, `key: null`, D-174); spoken zeros (D-175); no `system` (D-176) |
| stats_hour_response | GET /api/stats/hour?dow=&hour=&tz=UTC | weekday/hour of `listening.lastPlay` |
| stats_tracks_response | GET /api/stats/tracks?of=top | `First`, `Third` |
| stats_track_plays_response | GET /api/stats/track/{First}/plays | 2 listens |
| stats_region_response | GET /api/stats/region?x=12&y=12 | empty S1 answer (D-174) |
| stats_ignore_response | POST /api/stats/false-starts/ignore | `Second` |
| stats_false_starts_response | GET /api/stats/false-starts | one skipped `First` listen, `Second` ignored |
| stats_collapse_response | POST /api/stats/false-starts/collapse | body `{"trackIds":[]}` as the UI sends; nothing to collapse |
| stats_remove_response | POST /api/stats/false-starts/remove | `First`: one row of one track |
| stats_restore_response | POST /api/stats/false-starts/restore | `Second` |

`crates/kima-api-types/tests/stats.rs` round-trips them through `kima_api_types::stats`,
compares each fixture's key paths and JSON types with `stats_upstream_keys.json`
(hand-written from the v1.12.3 TypeScript interfaces; sub-keys under an empty array or
`null` are not observable and not required) and rejects integral floats such as `120.0`.

Captured against `feature/m-stats` source HEAD `d72195db44e5943b45a76853d9c3f97933e71d6d`
(before this task's commit). The capturing delegate could not create Docker containers, so
the script was run from a scratch copy whose only change is the database: a run-owned
database `mstats_capture_1794466228_340749` created in the parent-provided isolated cluster
`mstats-pg-58795` (`127.0.0.1:58795`, identity `kima_verify` checked first) and dropped on
exit. Only the ten stats fixtures were committed; the nine M-SYNC fixtures from the same run
differed from the committed ones only in dynamic ids and timestamps and were left as they
were.

## M6 podcast fixtures (plan `2026-11-12-m6-podcasts.md`, Task 19)

`KIMA_CAPTURE_SET=m6` copies 30 `podcasts_<name>_response.json` files and
`podcasts_capture_manifest.json`. The M6 part (`tools/capture-podcast-fixtures.inc.sh`,
sourced after M-STATS so `/library/recently-listened` also has artist rows) is a **real
loopback flow**, not SQL seeding: `tools/podcast-capture-mock.py` serves synthetic Apple
answers (genres, charts, lookup, search; **not** an Apple recording), three RSS feeds with
Podcasting 2.0 `ai-content` tags, range-aware audio and a PNG cover on one loopback port. The
server reaches exactly that socket through `KIMA_NET_ALLOW_LOOPBACK` (D-216; refused unless
`KIMA_NET_DENY_REAL=1`) and Apple through `KIMA_ITUNES_BASE_URL`.

Scenario: `POST /podcasts/subscribe` follows Race Show (Apple 555; no channel tag = unknown;
"Sprint" tagged human, "Race Recap" tagged AI) and Robot Radio (777, channel tagged AI) from
their feeds; progress 120/1200 s on Sprint (captured) and 300 s on a Robot Radio episode; the
default filter (`hide`) then hides Robot Radio and Race Recap everywhere it lists them.
`?ai=show` (`list_ai_show`) and the persisted setting (`POST /settings
{"podcastAiContent":"show"}` before `listening_ai_show`, reset to `hide` after) show them.
Preview Show (888) is only previewed, so its preview carries `previewEpisodes` and a
`snippet` read from its feed. The mock then adds "Pit Stop" to the Race Show feed and the
manual refresh finds exactly it. Admin label overrides and the unsubscribe run last.

| Fixture | Route | Condition |
| --- | --- | --- |
| categories | GET /podcasts/categories | mock genre tree (Comedy > Improv, Technology) |
| subscribe | POST /podcasts/subscribe | Race Show by feed + Apple id 555, new |
| progress | POST .../episodes/{Sprint}/progress | 120 of 1200 s |
| list / list_ai_show | GET /podcasts, `?ai=show` | hide: Robot Radio and Race Recap hidden (header 2) |
| detail / episodes | GET /podcasts/{race}, `/episodes` | `aiHidden: 1`; Sprint human (feed-episode) |
| next / next_end | GET .../episodes/{Qualifying,Grand Prix}/next | Sprint with progress / `{episode: null}` |
| listening / listening_ai_show | GET /podcasts/listening | setting hide (`aiHidden`) / setting show |
| charts, discover_top, discover_genre, discover_genres, for_you, similar, preview_similar | discovery routes | 777 hidden by its stored AI label |
| preview / preview_followed | GET /podcasts/preview/{888,555} | feed read (episodes + snippet) / followed |
| search / search_discover | GET /search?q=race&type=all, /search/discover?type=podcasts | followed show and visible episodes / Apple 555 |
| recently_listened | GET /library/recently-listened | artist row + Race Show (Sprint) |
| stats_spoken | GET /stats?period=all&tz=UTC | 2 follows, 2 in progress, backlog 4 |
| refresh / refresh_all | GET /podcasts/{race}/refresh, POST /podcasts/refresh-all | 1 new of 5 / 2 queued |
| progress_clear | DELETE .../episodes/{Sprint}/progress | |
| ai_label_show / ai_label_episode | PUT .../ai-label | human (show) / ai (Grand Prix), admin |
| unsubscribe | DELETE /podcasts/{robot}/unsubscribe | |

The manifest records, per request, the upstream route template, path, HTTP status and
`X-Kima-Ai-Hidden`; it also holds the status-only checks of the four non-JSON routes (episode
stream and preview snippet with `Range`, show and episode cover redirects). The script fails
before copying unless 30 named captures exist and all 24 upstream `routes/podcasts.ts`
registrations answered 2xx/3xx. `crates/kima-api-types/tests/podcasts.rs` then checks the DTOs,
key paths against `podcasts_upstream_keys.json` (upstream keys from the backend handlers, each
kima-only key with its DIVERGENCES row: D-207 AI keys, D-059 `trackCount`), populated nested
shapes, cross-response ids, `toISOString` timestamps, the header/body hidden counts and the
route count. `tools/check-podcast-upstream-keys.py <kima-hub checkout>` re-checks the key file
against the cited upstream source lines at `c7527577`, independently of the key file's claims.

Status (2026-11-13): script, mock, key file and tests are committed; **the fixtures are not**.
They must be captured from the committed checkout (`docker run` is needed, see
`~/.hermes/cache/scratch/m6-task19/CAPTURE.md`); until then the fixture tests are `#[ignore]`d.
A dry run of the same include file against a scratch database in the isolated cluster
`m6-fx-pg-58815` (not committed) passed every check and all ten tests, after the wire fixes made in
the same task (preview `description`/`subscribedPodcastId` read from upstream's handler;
`recently-listened` timestamps, caught by `timestamps_are_js_iso` on the first dry run).
