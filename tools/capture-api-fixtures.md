# M-SYNC API fixture capture (Task 18, steps 1–3)

Run `env -u DATABASE_URL -u PGDATABASE tools/capture-api-fixtures.sh` from this checkout.
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
