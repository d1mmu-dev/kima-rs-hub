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
