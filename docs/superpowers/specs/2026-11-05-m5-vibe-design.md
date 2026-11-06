# M5 Vibe engine: design

**Status: design draft for owner review.** Baseline: `origin/main` `f392a85`. Upstream reference: kima-hub v1.12.3 = `c7527577d925b20f5adf410c7fd4e0c8ac250f69`, read at `~/.hermes/cache/scratch/kima-hub-latest` (all `backend/src/...` paths below are relative to `backend/src` of that tree unless they start with `services/` or `frontend/`). Parent documents: [Rust design](2026-10-16-kima-rust-design.md), [M3 identification design](2026-10-19-m3-identification-design.md), [M4 ML analysis design](2026-11-03-m4-ml-analysis-design.md), [roadmap](../plans/2026-10-16-kima-rs-roadmap.md) (M5 row; M5b, M-STATS, M-SYNC hand-offs), `docs/DIVERGENCES.md`. Plan: [`../plans/2026-11-05-m5-vibe.md`](../plans/2026-11-05-m5-vibe.md).

**Verification marks.** **[V]** = read in upstream source or kima-rs code at the named file (not taken from a commit message). **[M]** = measured here (§12.2). **[?]** = not verified; each is repeated in §15.

## 1. Intent

Port upstream's "vibe engine": the part of kima-hub that answers *what sounds like this*, *what sounds like this text*, *what is between these two songs*, *what is the mood of this song* and *where does this song sit on the map of my library*. At v1.12.3 this is no longer "pgvector cosine + MusiCNN features" (the v1.11 design the roadmap row was written against). It is a **matching formula** over three terms (CLAP sound vector, a two-axis valence/arousal "feel" read off the same vector by a linear probe, and artist genre), a **UMAP-style 3-D map** laid out from that formula's own neighbours, and **22 learned mood heads** (linear heads on the same CLAP vector) with library-relative statistics [V `services/matching/*`, `services/moods/*`, `routes/vibe.ts`].

M5 is **backend only** (kima-db, a new pure-math crate, kima-ident stages, kima-api routes). Pages for it are M10 (hand-off §13).

## 2. Findings that change the roadmap's assumptions

1. **The roadmap row ("Port upstream `services/matching/__tests__`") is stale.** v1.12.3 has three test families to port: `services/matching/__tests__` (formula, map layout against umap-learn, exact math, integration), `services/moods/__tests__`, and the vibe route tests [V list in §10].
2. **MusicNN and the analysis DSP are not needed by the similarity engine.** `findSimilarTracks`, alchemy, song path, text search, the map and the learned moods read only `track_embeddings` (CLAP), `Artist` genres and `Track` presence [V `rank.ts`, `state.ts`, `moods/state.ts`]. The old hybrid (CLAP + musicnn features) is gone: `hybridSimilarity.ts` is 41 lines and calls the formula only [V]. What *does* touch analysis output is listed in §8 with the degradation for each.
3. **The CLAP model is a hard prerequisite, and kima-rs is on the wrong checkpoint.** The feel probe, the 22 mood heads and every stored vector are bound to the label `laion-clap-music-onnx-v1`: LAION-CLAP's music checkpoint (`music_audioset_epoch_15_esc_90.14`, exported as `music_audio.onnx` SHA-256 `ab1bf5eba067b246198c630f153f774e8f1c3169b3924d4d6a4dc247bd73de97`) [V `matching/feelProbe.json` `model`/`modelSha256`, `moods/moodHeads.json` `model`, migration `20261017120000_music_sound_vectors`: vectors under any other label are deleted]. The kima-rs M4 embeds with `hf_audio.onnx` (the `clap-htsat-unfused` export, SHA-256 `37df17af…`, model id `hf_audio`, pipeline `clap-audio-v1`) [V `crates/kima-ml/src/onnx.rs:190-236`, `crates/kima-ident/src/stages/embedding.rs:40,84`]. Applying the probe or the heads to those vectors yields numbers without meaning. The M4 follow-up that D-148 (void since D-160) already names (new checkpoint, new numpy mel front-end, `music_text.onnx`) is therefore **the prerequisite of real-model acceptance**, not of building M5 (§4.3).
4. **Upstream's text embedding goes through Redis pub/sub to the Python analyzer** (`textEmbeddingBridge.ts`, 15 s timeout, 504 on timeout). kima-rs has an in-process `kima_ident::stages::embedding::embed_text(ctx, text)` (typed errors `TextEmbeddingError::{Unavailable,Tokenize,Inference,Invalid,Worker}`) [V]. The bridge is not ported.
5. **Public vibe feed** (`/api/public/vibe/*`, 5 routes, live-listening SSE, library export with 8 neighbours per track) is a separate feature behind `KIMA_PUBLIC_VIBE`, depends on M-STATS (`stats/listens`) and on the playback-state live channel, and is called by no kima-hub page [V `routes/publicVibe.ts`, `services/publicLibraryExport.ts`, `services/liveListening.ts`]. It is **split out of M5 core** (Phase E of the plan, owner decision Q2).
6. **`getSimilarSongs`/`getSimilarSongs2` do not use the vibe engine upstream**: they pick random songs by the seed's artist and its stored similar artists that have library albums (`routes/subsonic/search.ts:350-405` [V]). The M3 spec parked them under "M5 (controller's call C11)" and D-086 says "stub until M5". M5 implements upstream's behaviour, not a vibe-based one (Task 19; D-190).
7. **No frontend page calls** `POST /api/analysis/vibe/{start,success,failure}` (analyzer callbacks) [V grep of `frontend/lib/api.ts`, `frontend/**`]. Not ported; M4's in-process stage replaces them.

## 3. Goals and non-goals

### 3.1 Goals

1. Reproduce the seven `/api/vibe/*` endpoints with upstream's wire shapes, error strings and limits (§5).
2. Reproduce the matching formula, its library state, the feel probe, ranking caps, the song-path arc walk and the alchemy blend **to the arithmetic**: same constants, same operation order, same tie rules (§6).
3. Reproduce the vibe map (UMAP-learn method, seed 42, colour field, incremental placement) so a library gets a deterministic map and a new track is placed without moving the others (§7).
4. Reproduce the 22 learned moods, their library statistics, the dominant-mood snapshot and `mood-summary` (§8).
5. Replace the delta-0 library re-rank of similar artists (M3 `kima_ident::similar`) with upstream's `RERANK_MAX_DELTA = 0.1` (§9.4).
6. Wire two new enrichment phases (`mood`, `similarity`) into the existing coordinator with progress, rerun steps and invalidation (§9).
7. Run well on a large library (§12) and degrade per capability when CLAP, the probe assets, the text model or the analysis features are missing (§4.4).

### 3.2 Non-goals (each a hand-off)

- Mixes, mood mixes (`/api/mixes/mood/*`), recipes, stations, radio, what's-new, `moodTags` writing, `UserMoodMix` -> **M5b** (they read M5's tables; §13).
- The vibe page, collection masthead mood band, "match this vibe" -> **M10**.
- New CLAP checkpoint, MusiCNN, DSP -> **M4 follow-up** (§4.3, §8).
- Public vibe feed, live listening -> optional Phase E / later milestone.
- GPU, remote ML -> M8/M12. The engine runs hub-side; M12's remote service only supplies vectors.
- Discover albums (`albums.location`) -> M7; M5 writes the filter as one SQL fragment M7 extends (§6.5).

## 4. Inputs, prerequisites, capability gates

### 4.1 What M5 reads (all exist on `main`)

| Need | Where in kima-rs | Note |
|---|---|---|
| CLAP audio vector | `track_embeddings(track_id, file_generation, model_id, model_sha256, pipeline_version, embedding vector(512), created_at)` migration `0024`, fenced publish `kima_db::ml::complete_embedding` [V] | `created_at` is rewritten on every upsert (`ml.rs:128-131` [V]) and a file change deletes the row (trigger `invalidate_track_ml`, `0026` [V]); it plays upstream's `analyzed_at` (the freshness token joined as `embedded_at`). |
| HNSW | `track_embeddings_cosine_idx` `hnsw (embedding vector_cosine_ops)` (defaults m=16, ef_construction=64) [V `0024`] | §11. |
| CLAP text vector | `embed_text(ctx, text)` -> unit 512-d [V] | One loaded text session per process. |
| Capability status | `mlCapabilities` in `GET /enrichment/status` (`cached_status`) [V `routes/enrichment.rs:120`] | Add nothing to the shape; M5 reads `ClapAudioEmbedding`/`ClapTextEmbedding`. |
| Config | `KIMA_CLAP_MODEL_DIR`, `DISABLE_CLAP` [V `kima-core/src/config.rs:346`] | New: `KIMA_VIBE_ASSET_DIR` (§4.2). |
| Artist genres | `artist_genres_effective` view (`0019`), `artists.user_genres text[]` (`0017`) [V] | §6.4. |
| Presence | `tracks.missing_since IS NULL` [V `0004`] | No `corrupt` column, no `albums.location` yet (§6.5). |
| Stage rows | `track_stages.stage` already allows `'similarity'`; the invalidation trigger already resets it [V `0018:4`, `0026:127`] | M5 does **not** use per-track stage rows for mood/similarity (§9.1). |
| Progress shape | `SimilarityProgress{total,completed,pending,progress,isBackground}` all zero [V `kima-db/src/enrichment.rs:686`] | M5 fills it and adds `moods` (same shape). |

### 4.2 Assets M5 needs (rights first)

| Asset | Upstream file | Size | Licence / provenance (as upstream states it) | M5 handling |
|---|---|---|---|---|
| Feel probe (valence, arousal weights+bias, alpha) | `services/matching/feelProbe.json` | 25 KB | "Derived from DEAM ... CC BY-NC. This file is licensed CC BY-NC, separately from Kima's GPL-3.0 code" [V file header]; fitted on `laion-clap-music-onnx-v1` | **Private asset, not in Git** (owner rule on weights/derivatives). Loaded from `KIMA_VIBE_ASSET_DIR/feel_probe.json`, SHA-256 pinned in a compiled table (M4 pattern), fail-closed. |
| 22 mood heads | `services/moods/moodHeads.json` | 239 KB | `source` lists upstream-lab training files (`kima-eval-data/moods/w1_real/...`); **no licence statement and training-data licences not read [?]** | Same handling; rights of the training sets are open question Q3. |
| MusicBrainz genre list | `services/matching/mbGenres.json` | 37 KB, 2212 names | CC0 (MusicBrainz core data), fetched 2026-10-06 [V file header] | May live in Git (`crates/kima-vibe/assets/`). |
| Parity fixtures | `services/matching/__tests__/fixtures/{parity,map-parity}.json`, `services/moods/__tests__/fixtures/shipped-reference.json` | 92 / 73 / 47 KB | Synthetic, "made-up vectors ... no real data" [V `eval/matching_parity_fixture.py` docstring]; `map-parity` is umap-learn 0.5.12 / scikit-learn output on synthetic points [V test header]; `shipped-reference.json` content not opened beyond its size [?] | Copy to `crates/kima-vibe/tests/fixtures/` with a provenance note (repo is GPL-3.0 both sides). Check `shipped-reference.json` for real-data content before committing (Task 0). |

Capability naming: `vibe_assets` (probe + heads + hashes verified) is one more entry in the existing capability report. Missing/mismatched asset: server starts, the **dependent** features turn off (§4.4), nothing else.

### 4.3 The model-label gate

Upstream filters every query with `te.model_version = MATCHING_MODEL`. kima-rs has no label column; identity is `(model_id, model_sha256)` plus `pipeline_version`. M5 defines one compiled constant `MATCHING_SOUND: {model_id, model_sha256, pipeline_version}` for the **music checkpoint** (expected `model_id = "music_audio"`, sha `ab1bf5eb…`, pipeline `clap-music-audio-v1`; the exact strings are fixed by the M4 follow-up and copied here, [?]). Every eligible-track query joins `track_embeddings` with `model_id = $1 AND model_sha256 = $2` (§6.5).

Consequence until the follow-up lands: **zero eligible tracks** on a library embedded with `hf_audio`. M5 then reports `similarity.total = 0`, `GET /vibe/map` answers 202 only while a layout is plausible (below), `GET /vibe/similar/:id` answers upstream's 404, `POST /vibe/search` answers 503 `{"error":"Vibe search needs the music CLAP model"}` (kima-rs-only text, D-182). Unit and integration tests need no real model: they seed synthetic unit vectors under the gate's label (as upstream's `insertEmbeddings` does).

Owner question Q1: build M5 against synthetic vectors now and accept the real-model parity later, or land the M4 follow-up first.

### 4.4 Degradation matrix

| Missing | Effect | How the API says so |
|---|---|---|
| `DISABLE_CLAP` / audio model unavailable | No new vectors; existing eligible vectors keep working | `mlCapabilities` (M4) |
| CLAP text model/tokenizer | `POST /vibe/search` -> 503; everything else works | `embed_text` error mapping §5.6 |
| `vibe_assets` (probe) | No feel -> no formula. Similar/alchemy/path/map/state all off | `similarity.total = 0`; endpoints as §4.3 |
| `vibe_assets` (mood heads only) | Moods off: `mood-summary` all `unanalyzed`, `GET /vibe/mood/:id` -> `{"dominantMood":"unanalyzed"}`; similarity unaffected | `moods.total = 0` |
| MusiCNN / DSP (today) | See §8: map `energy` null, `valence` from CLAP only; mood readings from CLAP only; recipes (M5b) lose BPM/key filters | per field |
| Analysis stage (today deferred) | The upstream gate "analysis finished" is replaced by "embedding stage settled" (D-186) | — |

## 5. API (wire contract)

All routes under `/api/vibe`, `requireAuth` (any signed-in user), `apiLimiter` as the other `/api` routes [V `index.ts:180`]. JSON camelCase, errors `{"error": "<text>"}` (kima-rs convention, same as upstream here). Ids are UUIDs (D-050), so upstream's CUID checks become UUID checks with the same message (D-193). `parseLimit(raw, fallback, max)` = `max(1, min(parseInt(raw)||fallback, max))` [V `routes/library/paging.ts:7`]; reuse the existing kima-api paging helper if it has the same rule, else add it.

### 5.1 `GET /api/vibe/map`  [V `routes/vibe.ts:53`, `services/vibeMap.ts`]

- `202 {"computing": true}` when no eligible track has a map row **and** the library has present tracks **and** vibe embeddings are possible (kima-rs: CLAP not disabled and the sound model available); otherwise an empty library yields `200` with `tracks: []`.
- `200 MapResponse`: `{tracks: MapTrack[], trackCount, computedAt (ISO, matching_state.map_updated_at or epoch), moodSignature}`.
- `MapTrack` = `{id, x, y, z, colour, title, artist, artistId, albumId, coverUrl, dominantMood, moodScore, moods:{<moodField>: rank}, energy|null, valence|null}`. `x,y,z` = `0.5 + (p − centre)/range` with one `range = max axis span || 1` for all three axes; rows ordered by track id. Mood fields are the five client-facing fields of §8.2.
- `Accept-Encoding: gzip` -> gzipped body with `Vary: Accept-Encoding`; upstream hashes the JSON per request and caches the gzip by that hash [V `vibe.ts:31-47`]. kima-rs keys the cache on `(map_updated_at, mood signature, track count, tracks.updated_at max)` instead (no per-request SHA-1 of ~30 MB); the response bytes are identical in content. **Not verified that nothing else changes a title/cover without bumping these**, so the key also includes `max(tracks.updated_at)` and `max(albums.updated_at)` if the column exists [?] (Task 13 decides after reading the schema; fallback: hash as upstream).
- `energy`/`valence` read `tracks.energy`/`tracks.valence` upstream; kima-rs has neither (§8): `null` unless the M5 CLAP readings table has a value (valence) (D-186/D-187).
- Errors: 500 `Failed to read the vibe map`.

### 5.2 `GET /api/vibe/mood-summary`  [V `vibe.ts:72`, `vibeMap.ts:158-195`]

`202 {"computing":true}` as above, else `{shares:[{key,fraction}], albums:{albumId: mood}, artists:{artistId: mood}}` over the tracks the map draws. Order: biggest share first, `neutral` last among equals by legend position (`LEGEND` = the five client mood fields in `MOODS` order, then neutral), `unanalyzed` tracks left out of everything, any non-legend value counts as `neutral`; a group whose winner is `neutral` is omitted. 500 `Failed to read the mood summary`.

### 5.3 `POST /api/vibe/path`  [V `vibe.ts:96`, `services/songPath.ts`]

Body `{startTrackId, endTrackId, length?, mode?}`. Validation order and texts: non-string/missing -> 400 `startTrackId and endTrackId are required strings`; not UUID -> 400 `Invalid track ID format`; equal -> 400 `Start and end tracks must be different`; `length` truthy -> clamp to 5..50 (upstream `Math.max(5, Math.min(50, length))` on whatever type; kima-rs: non-numeric `length` is treated as absent, D-193); `mode == "discovery"` else `smooth`. Service errors (`UserFacingError`, status 400): `One or both tracks have not been through the similarity step yet`; `These tracks are too similar for a meaningful journey. Try "Similar Tracks" instead.` (distance `1 - pairScore/SELF_SCORE < 0.15`). Answer `{startTrack, endTrack, path: LibraryTrack[], metadata:{totalTracks, embeddingDistance, averageStepSize, mode}}` where every track also carries `displayTitle` (edited title or `null`) [V `vibe.ts:117-124`]. `LibraryTrack` = `{id,title,duration,albumId,albumTitle,albumCoverUrl,artistId,artistName}` (upstream's raw shape; note it is **not** the nested `album/artist` shape of the other endpoints). 500 `Failed to generate song path`.

### 5.4 `POST /api/vibe/alchemy`  [V `vibe.ts:139`]

Body `{add: id[], subtract?: id[], limit?}`. Texts (in order): `At least one track ID in 'add' is required`; `'add' may contain at most 10 tracks`; `'subtract' must be an array of at most 10 tracks`; `'add' must contain valid track IDs`; `'subtract' must contain valid track IDs`; any id without matching data -> 400 `{error:"Some tracks have not been through the similarity step yet", missingAdd, missingSubtract}`; blend with `blendSelfScore <= 0` (or NaN) -> 400 `The tracks to add and to subtract cancel each other out`. Weights `+1/|add|`, `-1/|subtract|`. `limit` default 20, max 100. Ranking `rankMatches(ctx, inputs, {limit, counted: addIds})` (inputs excluded, `add` tracks take places in the album/artist caps, `subtract` tracks do not). `scale = sqrt(blendSq * SELF_SCORE)`, `match = score/scale`, per track `{id,title,displayTitle,duration,distance:1-match,similarity:clamp01(match),album:{id,title,coverUrl},artist:{id,name}}` (no `trackNo`). 500 `Failed to compute alchemy blend`.

### 5.5 `GET /api/vibe/similar/:trackId`  [V `vibe.ts:220`, `hybridSimilarity.ts`]

No id validation (an unknown id is just "no matching data"). `limit` default 20, max 100. Empty result (no matching data, engine off) -> 404 `{error:"No similar tracks found", message:"This track may not have been analyzed yet, or no analyzer is running"}` (also when the feature flag is off, after logging a warning). 200 `{sourceTrackId, tracks:[{id,title,displayTitle,duration,distance,similarity,album:{id,title,coverUrl},artist:{id,name}}]}`; `match = score / SELF_SCORE (1.0625)`, `distance = 1 - match`, `similarity = clamp01(match)`; the seed is excluded and counts as the first member of its album and artist in the caps. 500 `Failed to find similar tracks`.

### 5.6 `POST /api/vibe/search`  [V `vibe.ts:272`, `textEmbeddingBridge.ts`]

Body `{query, limit?}`. `query` missing/non-string/`trim().length < 2` -> 400 `Query must be at least 2 characters`; raw `length > 500` -> 400 `Query must be at most 500 characters`; `limit` default 20, max 100. Embed `trim(query)`, rank the nearest tracks by cosine distance, **no similarity floor**, ties by track id. 200 `{query, tracks:[{id,title,displayTitle,duration,trackNo,distance,similarity: max(0, 1-distance),album{id,title,coverUrl},artist{id,name}}]}`. Errors: text embedding timeout -> 504 `{error:"Text embedding service unavailable", message:"The CLAP analyzer service did not respond in time"}`; kima-rs maps `TextEmbeddingError::Unavailable` to **503** `Text embedding service unavailable` with message `The CLAP text model is not available` and keeps 504 for an inference time limit (15 s `tokio::time::timeout`) (D-183); other -> 500 `Failed to search tracks by vibe`.

### 5.7 `GET /api/vibe/mood/:trackId`  [V `vibe.ts:333`]

`{dominantMood}` from the mood snapshot (`neutral`, `unanalyzed`, or a client mood field such as `moodHappy`). Unknown track -> `unanalyzed`, not 404. 500 `Failed to read the track's mood`.

### 5.8 Other endpoints touched

| Endpoint | Change |
|---|---|
| `GET /api/enrichment/progress` | `similarity` filled; new `moods` object, same 5 keys (`total, completed, pending, progress, isBackground:true`); `isFullyComplete` additionally requires `moods.completed == moods.total && similarity.completed == similarity.total` [V `workers/unifiedEnrichment.ts:3149-3212`]. Cache TTL (`PROGRESS_TTL`) unchanged. |
| `GET /api/enrichment/status` | `currentPhase` may be `mood` or `similarity` (DB check constraint widened, migration like `0025`) [V `enrichmentState.ts:22-24`]. kima-rs keeps its own name `embedding` for the vector phase where upstream says `vibe` (existing D-row, not changed here [?] confirm in Task 12). |
| `POST /api/enrichment/rerun` (M-SYNC-B Task 14, `control::Step`) | Steps `analysis`, `vectors`, `moods`, `similarity` become real; the clear matrix is upstream's `STEP_CLEARS` [V `services/enrichmentRestart.ts:34-52`]. |
| Similar artists (library re-rank) | `kima_ident::similar::rerank`: §9.4. |
| `getSimilarSongs(2)` | §2 item 6. |
| `GET /api/system/features` | `vibeEmbeddings` true iff the matching model + probe are available **or** (upstream rule) `DISABLE_CLAP` is not set and either the analyzer runs or any `track_embeddings` row exists [V `featureDetection.ts:checkCLAP`]. kima-rs rule: `!DISABLE_CLAP && (audio capability available || any eligible-model embedding exists)`; exact route wiring is M4's, M5 only supplies the predicate. |

## 6. The matching formula

All of this is `services/matching/formula.ts` [V 272 lines] and `rank.ts` [V 314 lines]; ported to a pure crate (§9.2). Constants: `FEEL_WEIGHT = 0.25`, `GENRE_WEIGHT = 0.25`, `MAX_PER_ALBUM = 2`, `MAX_PER_ARTIST = 4`, `SELF_SCORE = 1 + 0.25² = 1.0625`.

### 6.1 Pair score

`s(a,b) = ĉ_a·ĉ_b + 0.25²·(v_a·v_b + r_a·r_b) + [artist_a ≠ artist_b]·w_g²·(g_A·g_B)`

- `ĉ` = unit-length CLAP vector (stored vectors are within 1e-3 of unit; the engine renormalises, `unit()`).
- `(v, r)` = `feelVector`: raw probe valence/arousal standardised per axis by the **library** mean and population SD (an axis with sd = 0 contributes 0), then scaled to unit length; the zero vector when both are 0.
- `g_X` = centred unit-length TF-IDF genre vector of artist X over the library vocabulary (§6.4); artists without a model entry have no genre term. `w_g = 0.25·sqrt(soundSpread / genreSpread)`, 0 if `genreSpread <= 0`.
- A weighted input set scores `Σ_i w_i s(t,i)`; the genre part uses `artistBonuses` (`w_g²(g_B·Σ w_i g_A(i) − W_B)`, `W_B` the summed weight of inputs by B, removing the self-artist term).

### 6.2 Feel probe

Per sound vector: `valence = clamp(0, 1, −(e <#> probe_w)/‖e‖ + bias)` (and the same for arousal), computed **in SQL** in float8 over the stored vector [V `state.ts:updateFeel`]; `alpha` is in the file but unused by the SQL [V]. Stored in `matching_feel` with `embedded_at = embedding.created_at` and `probe` = first 16 hex chars of `sha256(JSON.stringify([model, valence, arousal]))` [V `state.ts:23-27`]. The probe id is a **re-run key**, so the Rust side must hash the same JSON text; since `JSON.stringify` of the parsed object is not byte-stable across languages (number formatting), kima-rs defines its own `probe_id = first 16 hex of sha256(asset file bytes)` (D-181). Consequence: a kima-rs install never shares a probe id with upstream; irrelevant because no database is shared.

### 6.3 Library state (`matching_state`, one row)

Built by `buildMatching` in **one RepeatableRead transaction** that first `SELECT … FOR UPDATE`s the state row, then updates feel for stale vectors, computes the inputs fingerprint and, if `(STATE_VERSION, fingerprint)` changed, recomputes and upserts [V `state.ts:189-245`]:

- `STATE_VERSION = "<FORMULA_VERSION=1>/<model>/<probe id>"`; kima-rs prefix keeps `FORMULA_VERSION` independent of upstream's counter (D-181).
- Fingerprint = `sha256(JSON[STATE_VERSION, count, sum(hashtextextended(track_id || ':' || epoch(created_at) || ':' || artist_id || ':' || coalesce(genres_json) || ':' || coalesce(user_genres), 0))])` over eligible tracks with current feel. kima-rs: `hashtextextended` is available (PG 17 [V image]); the hashed genre text is the *effective, filtered* genre list string of the artist so any change that moves the model moves the hash.
- Stats: `n`, `avg/stddev_pop(valence|arousal ORDER BY track_id)` (the `ORDER BY` makes the aggregate order-stable), `soundSpread = 1 − ‖mean(ĉ)‖²` via `vector_norm(avg(l2_normalize(embedding) ORDER BY track_id))`, genre model + `genreSpread` from `buildGenreModel` over `(artist, track count, genres)`, `genreWeight = genreWeight(soundSpread, genreSpread)`. No eligible track -> the row is deleted.
- Write conflicts (`40001`, `40P01`) from a concurrent re-run/map write are swallowed: nothing is written and the next pass rebuilds (`isWriteConflict`). Timeouts: build 5 min, re-run 7 min, map write 8 min — "chosen limits, not measured limits" and must stay in that order [V `state.ts:36-38`].

### 6.4 Genres

`matchingGenres(artist)`: take the artist's merged genres (the person's `userGenres` **instead of** the enriched list when non-empty, `getMergedGenres` [V `utils/metadataOverrides.ts:28-36`], matching D-114's v1.12.3 note and M-SYNC Task 9 / owner question Q5), keep those whose `genreKey` (`lower-case, [^a-z0-9] removed`) is in the MusicBrainz genre list, spelled as MusicBrainz spells them lower-cased [V `state.ts:40-60`]. TF-IDF: `idf = ln(max(taggedTracks,1)/max(df,1))` with `df` weighted by track count, per-artist L2-normalised weights, vocabulary sorted, artists sorted by id, mean weighted by track count, per-artist `norm = ‖tfidf − mean‖` (artists with norm 0 are dropped), `spread = Σ tracks/total − ‖Σ tracks·centred/norm‖²/total²`. Natural log goes through the exact-math `log` (§7.1).

kima-rs source of the artist's list: `artist_genres_effective` (MusicBrainz genres, else file tags — D-114) rather than upstream's `Artist.genres` JSON (enrichment-sourced). The two can differ for artists with file tags only; the filter against the MusicBrainz list removes most of the noise. Not measured on a real library [?]; D-192. If M-SYNC Q5 is answered "merge", the function follows it.

### 6.5 Eligibility (one fragment)

A track is **eligible** iff: it has a `track_embeddings` row with the §4.3 model identity, `vector_dims = 512` and `vector_norm > 0`, `tracks.missing_since IS NULL`, and (today) nothing else. Upstream adds `EXISTS Album.location='LIBRARY'` and `t.corrupt = false` and, for moods, `analysisStatus = 'completed'` [V `rank.ts:170`, `moods/scoredSongs.ts`]. kima-rs has none of those three columns; D-186 records it, and M7 (Discover) extends the single SQL fragment `kima_db::vibe::ELIGIBLE` when it adds `albums.location`. Review Focus: every vibe query goes through `ELIGIBLE` (a test greps the module).

### 6.6 Ranking passes

`rank_matches(inputs, limit, counted)` = score every eligible track not in `inputs`, then apply the caps in SQL with the two-window trick (best 2 per album minus used places, then best 4 per artist among those, ordered `score DESC, id`) [V `rank.ts:187-219`]. `top_scores` = same without caps, inputs includable. `score_library(sets, exclude)` = every eligible track with one score per set (song path uses two sets). `artist_profiles` = per artist mean unit sound vector (`avg(l2_normalize(embedding))`) and mean feel vector over its eligible tracks.

The SQL score is `|blend|·(1 − (embedding <=> blend)) + 0.0625·(z·feel_blend)/‖z‖ + bonus` where `z` = standardised feel in SQL, `bonus` joined from an `unnest(artist_ids, bonus_cols)` CTE [V `rank.ts:124-173`]. kima-rs sends vectors as `$n::real[]::vector` (the existing pattern, `ml.rs`) and the bonus arrays as `uuid[]`/`float8[]`. Float differences: pgvector computes `<=>` in float32; upstream's own tests allow it. kima-rs parity tolerance: scores within `1e-5` of the f64 reference, **rank order identical except where scores differ by less than the tolerance** (Task 6).

### 6.7 Song path

`generateSongPath` [V `songPath.ts`]: `cosθ = pairScore(start,end)/SELF_SCORE`, `distance = 1 − cosθ`, refuse `< 0.15`; `numWaypoints = length || max(8, min(50, round(distance·15)))`; score the library against both endpoints once (`score_library`, endpoints excluded); for waypoint `i=1..n`, `(w0,w1) = slerpWeights(cosθ, i/(n+1))` (`sin((1−t)θ)/sinθ`, `sin(tθ)/sinθ`; straight line when `θ < 1e-6`), candidates = the `8` (smooth) or `10` (discovery) unvisited tracks minimising `1 − (w0·s0 + w1·s1)/SELF_SCORE`, ties by id; smooth picks the first, discovery picks uniformly among the first `min(3, n)`; if the previous two picks share an artist and the new pick has it too, switch to the first candidate by another artist with `distance ≤ 1.05·candidates[0].distance`. `averageStepSize = mean(selected.distance)`. Randomness is injectable (`impl FnMut(usize)->usize`) so tests are deterministic.

## 7. The vibe map

`services/matching/{map,mapCompute,mapLayout,neighbours,exactMath}.ts` [V, 1,200 lines]. Method: "UMAP's published method step for step with umap-learn 0.5.12" on the formula's own 14 nearest neighbours (+ the track itself first = 15), 3-D, then a colour field.

### 7.1 Determinism contract

Upstream implements `exp`, `log`, `atan2` (fdlibm ports) and a mulberry32 PRNG so "every runtime gives the same map bit for bit" and tests assert it [V `exactMath.ts`, test "lays out and places the same bits on every engine"]. Rust's `f64::exp/ln` call the platform libm, which may differ in the last ulp. M5 ports `exactMath` verbatim (≈220 lines, no unsafe) and uses it in the layout path; `sqrt`, `+ − × ÷` are IEEE-exact. This makes the Rust map bit-identical to upstream's for the same inputs **if** the neighbour lists are identical, and in any case reproducible across machines. Gaussian = Marsaglia polar with the same PRNG call order. Constants: `MAP_NEIGHBOURS 15`, `CURVE_A 1.57694346046584`, `CURVE_B 0.8950608779639974`, `AREA_NEIGHBOURS 50`, `NEGATIVE_RATE 5`, `SEED 42`, epochs `500` (≤10,000 tracks) else `200`, placement epochs `100` / `30` (> 10,000 new tracks) at α₀ 0.25, layout α₀ 1.

### 7.2 Pipeline

1. **Neighbours** (`nearestColumns`): all pairs scored with the formula (4 rows per column sweep), top-`k` per row with ties by track-id rank; `maxScore` = highest off-diagonal score. Distance = `maxScore − score`. Time grows with n² (§12).
2. **Start**: subspace iteration on the centred matrix `[sound; 0.25·feel; w_g·genre]` with 13 directions, ≤200 iterations, 3 largest Ritz pairs, `svd_flip`, scaled to max coordinate 10 plus 1e-4 Gaussian noise (`seed+1`).
3. **Layout**: fuzzy graph (smooth-kNN with 64 bisection steps, union `A + Aᵀ − A∘Aᵀ`, weak edges `< max/epochs` dropped, row-major edge order), then `optimize` with clipped ±4 gradients, 5 negative samples, linear α decay.
4. **Colour field**: centre/scale, orthogonal-Procrustes turn onto (arousal, valence) via a 2×2 Jacobi eigen, each track's colour = hue of the mean of its 50 nearest map neighbours' projected points, share of the library's `[hueMin, hueMax]` range.
5. **Placement** of new tracks (`place`): neighbours among laid-out tracks only (k=15), smooth-kNN with local connectivity 0, start at the strength-weighted mean, 100 epochs with all laid-out tracks held still; colour via the stored frame. **No placed track ever moves.**

### 7.3 When it runs (`updateMap(inputsSettled)`)

Single-flight (concurrent callers share one run). Counts first and loads vectors only if a track lacks a place or a layout is possible. A **first layout waits for `inputsSettled`** (analysis + vectors + artist genres settled: `progress.artists.owed === 0` upstream [V `unifiedEnrichment.ts:2877`]) because the layout is permanent. A relayout happens only when `mapVersion` (= `STATE_VERSION/LAYOUT_VERSION`) or the stored frame is missing or fewer than 2 laid-out rows remain. One track: position `(0,0,0)`, colour 0.5, no frame. Writes go through `writeWhileStateStands` (lock the state row; if a re-run deleted it, drop the write) in chunks of 5000 via `unnest` upserts [V `map.ts`].

Rust execution model: `tokio::task::spawn_blocking` (or a dedicated `std::thread`) holding a cancellation flag checked per epoch and at shutdown; rayon for the O(n²) sweep. Memory: sound matrix `n×512` as f32 in memory (upstream holds f64 = 2×).

## 8. Moods and the analysis features

### 8.1 What upstream needs from MusiCNN/DSP (verified) and what kima-rs does today

| Upstream use | Source column (v1.12.3) | Produced by | kima-rs M5 |
|---|---|---|---|
| Matching formula, ranking, map, text search | none | CLAP only | **Full parity, no MusiCNN/DSP needed.** |
| 22 learned mood scores and their library stats | none | CLAP vector, linear heads | **Full parity.** |
| Dominant-mood snapshot gate `analysisStatus = 'completed'` [V `moods/scoredSongs.ts`] | analysis stage state | Essentia analyzer | Replaced by "has an eligible vector" (D-186). When the analysis phase exists and is enabled, the *settled* check also waits for it (`inputs_settled`). |
| `/vibe/map` `energy` | `Track.energy` = mean RMS × 3 [V `services/audio-analyzer/analyzer.py:590`] | Essentia (DSP) | `null` until DSP lands (type is already `number\|null`). |
| `/vibe/map` `valence` | `COALESCE(round(0.7·deamValence + 0.3·clapValenceRaw), deamValence, clapValenceRaw)` [V `unifiedEnrichment.ts:630`] | MusiCNN DEAM/emomusic heads + CLAP zero-shot | CLAP zero-shot only (the same code's own fallback branch); `null` without the M5 readings step. |
| Mood **readings** `arousal`, `instrumental_logit`, `acoustic_logit` in `mood_state` [V `moods/state.ts:25-37`] | `arousal` = `COALESCE(round(0.5·deamArousal + 0.5·clapArousalRaw), deamArousal, clapArousalRaw)`; `instrumentalness`, `acousticness` | MusiCNN (deam) + CLAP zero-shot | CLAP zero-shot values; the state already stores a reading nobody has as `NULL` ("absent", recipes refuse) [V `computeState`]. Numbers differ from upstream's blend, so any recipe z-scores (M5b) are recalibrated per library; D-186. |
| Recipes' BPM and key filters, `STEADY_BEAT` [V `mixes/recipes.ts`] | `bpm`, `bpmConfidence`, `keyScale` | DSP | M5b concern: those filters are dropped/off without DSP. Listed in §13. |
| `key_similarity` with sharps (M-SYNC Appendix B row M5) | analysis key | DSP | **Not an M5 vibe feature**: it is the analysis-v5 key comparison for stats; hand to M-STATS S2/M4. |

### 8.2 Learned moods [V `moods/heads.ts`, `moods/state.ts`, `moodBucketService.ts`]

- 22 heads in fixed order: `happy fun_quirky uplifting sad nostalgic dark trippy calm dreamy energetic aggressive epic party groovy romantic sexy summer night driving workout focus sleep`; `night` is retired (still scored; no tag/recipe may use it). Each head is `weights[512] + bias`, standardisation already folded in; score = `−(embedding <#> w)/‖embedding‖ + bias` in SQL (logit). File must parse with exactly the right order/dims/finite values (`parseMoodHeads`).
- `track_moods` per vector (with `embedded_at` and `weights` = heads version) and `mood_state` (one row: per mood and per reading `mean` + population `sd` over the covered tracks; version `"2/<heads version>"`). Rebuilt when its inputs fingerprint changes; a cheap *change signal* gates the full hash (hash at most hourly) [V `state.ts:79-181`].
- Client-facing mood table (exact): `happy->moodHappy (Upbeat)`, `sad->moodSad (Melancholic)`, `calm->moodRelaxed, id chill (Calm)`, `aggressive->moodAggressive (Intense)`, `party->moodParty (Energetic)`. A track *has* a mood when `(score − mean)/sd >= RECIPE_LINE (1.25)`; the dominant mood is the one with the highest **library rank** (midpoint of lower/upper bound / n, ties half) among those; none -> `neutral`; missing any of the five -> `unanalyzed`. A mood with sd = 0 ranks no one (−∞) [V `moodBucketService.ts:56-134`].
- Snapshot: a process-wide cache `{signature, views, pending}` rebuilt only when `mood_state.version:fingerprint` changes, refreshed every 30 s by a worker and on demand; while there are no statistics the last snapshot stands [V `moodSnapshotWorker.ts`]. In kima-rs this is an `ArcSwap`/`RwLock` in `AppState`, refreshed by the same coordinator cycle plus a 30 s interval task.

### 8.3 CLAP zero-shot readings (new in M5)

Upstream computes `instrumentalness`, `acousticness`, `clapValenceRaw`, `clapArousalRaw` at embedding time in the Python analyzer from the *audio* vector and 8 prompt groups embedded once per process [V `services/audio-analyzer-clap/analyzer.py:60-100, 290-346`]. kima-rs's M4 embedding stage does not. M5 computes them **from the stored vector** after the fact, in the `mood` phase, with `embed_text` for the 24 fixed prompts (cached per process):

- `similarity(group) = mean_k cos(unit(audio), unit(text_k))`;
- `instrumentalness = round3(clamp01(softmax_share(sim(instrumental), sim(vocal), T=15)))`, `acousticness` likewise with `acoustic` vs `electronic`;
- `valence = round3(sigmoid(8.264·(sim(happy)−sim(sad)) + 0.249))`, `arousal = round3(sigmoid(10.654·(sim(high_energy)−sim(low_energy)) + 0.018))` (the Platt constants are fitted to the **music** model [V `VA_CALIB`]).

Stored in `track_clap_readings(track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)`. The prompt texts are copied verbatim (they change every stored score if edited). Because the text model is the *music* text export, this step is gated like everything else (§4.3). Whether this lives in M5 or M5b is Q4 (the only M5 consumer is the map's `valence` and the stored reading stats; the main consumer is M5b).

## 9. Wiring into kima-rs

### 9.1 Phases, not stages

Upstream's mood and similarity steps are **library-wide** passes run in the enrichment cycle after the vibe phase: `executeMoodPhase` (waits for `moodInputsSettled`, runs `buildMoods`, drops cached mixes when moods changed, writes tags) then `executeSimilarityPhase` (`buildMatching`, `updateMap`) [V `unifiedEnrichment.ts:1763-1772, 2820-2895`]. They have no `TrackStage` rows; progress is derived from table joins (`moodProgress`, `matchingProgress`). kima-rs mirrors this: two new coordinator phases after Step 3b (embedding) in `kima_ident::coordinator`, each `Phase` value added to the `enrichment_state.current_phase` check, no per-track rows. The existing `'similarity'` value in `track_stages.stage` stays unused (harmless; the file-change trigger resets it).

`inputs_settled(progress)` = embedding `pending == 0 && working == 0 && retrying == 0` (and, when the analysis phase is enabled, analysis likewise). A moods/similarity pass errors are logged and never stop the cycle [V].

### 9.2 Crate layout (decision)

| Crate | New content |
|---|---|
| **`kima-vibe`** (new, pure) | `exact_math`, `formula` (pair score, genre model, feel, caps, slerp), `umap` (layout, place, colour field, PCA start, PRNG), `moods` (heads parse, rank/classify, summary), `song_path` (waypoint walk over supplied score vectors), `zero_shot` (the readings arithmetic). No sqlx, no tokio, no `ort`. `#![forbid(unsafe_code)]`. |
| `kima-db::vibe` | Migrations, `ELIGIBLE` fragment, state build, feel update, rank/capped queries, map read/write, mood scoring/state, progress counts, rerun clears. |
| `kima-ident::stages::{moods, similarity}` + coordinator hooks | Asset loading and verification, single-flight map task, the two phases, text-prompt cache for §8.3. |
| `kima-api::routes::vibe` + `kima-api-types::vibe` | The seven routes and wire DTOs (hand-written, M9 convention). |

Why a new crate rather than `kima-ml`: `kima-ml` carries `ort`, native soxr and audio decode; the engine is arithmetic that must be unit- and fixture-testable in seconds and reused unchanged by M12's hub. Why not inside `kima-ident`: the arithmetic is not identification. The crate is small enough (≈2.5 kLOC) to review independently.

### 9.3 Migrations (sketch; numbers are the next free at merge time, M4 owns `0024-0027`)

```sql
-- 00NN_vibe_engine.sql
CREATE TABLE matching_feel (
    track_id    uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL,          -- = track_embeddings.created_at it was computed from
    probe       text        NOT NULL,
    valence     real        NOT NULL CHECK (valence BETWEEN 0 AND 1),
    arousal     real        NOT NULL CHECK (arousal BETWEEN 0 AND 1)
);
CREATE TABLE matching_state (
    id text PRIMARY KEY DEFAULT 'default' CHECK (id = 'default'),
    version text NOT NULL, fingerprint text NOT NULL, track_count integer NOT NULL,
    feel_mean_valence float8 NOT NULL, feel_mean_arousal float8 NOT NULL,
    feel_sd_valence float8 NOT NULL,   feel_sd_arousal float8 NOT NULL,
    sound_spread float8 NOT NULL, genre_spread float8 NOT NULL, genre_weight float8 NOT NULL,
    genres jsonb NOT NULL,
    built_at timestamptz NOT NULL DEFAULT now(),
    map_version text, map_frame jsonb, map_updated_at timestamptz
);
CREATE TABLE matching_map (
    track_id uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL,
    x float8 NOT NULL, y float8 NOT NULL, z float8 NOT NULL, colour float8 NOT NULL,
    laid_out boolean NOT NULL
);
CREATE TABLE track_moods (               -- 22 real columns, names as MOODS
    track_id uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL, weights text NOT NULL,
    happy real NOT NULL, fun_quirky real NOT NULL, /* ... all 22 ... */ sleep real NOT NULL
);
CREATE TABLE mood_state (                -- id/version/fingerprint/track_count/computed_at,
    id text PRIMARY KEY DEFAULT 'default' CHECK (id = 'default'),   -- <mood>_mean/_sd float8 NOT NULL x22,
    version text NOT NULL, fingerprint text NOT NULL, track_count integer NOT NULL,   -- <reading>_mean/_sd float8 NULL x3
    computed_at timestamptz NOT NULL DEFAULT now()
    /* ... */
);
CREATE TABLE track_clap_readings (       -- §8.3, optional (Q4)
    track_id uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL,
    instrumentalness real NOT NULL, acousticness real NOT NULL,
    valence_raw real NOT NULL, arousal_raw real NOT NULL
);
ALTER TABLE enrichment_state DROP CONSTRAINT enrichment_state_current_phase_check;
ALTER TABLE enrichment_state ADD CONSTRAINT enrichment_state_current_phase_check
    CHECK (current_phase IN ('artists','tracks','fingerprint','embedding','mood','similarity'));
CREATE INDEX matching_map_embedded_idx ON matching_map (embedded_at);   -- only if EXPLAIN shows a use (Task 3)
```

Differences from upstream: uuid keys and FKs; `ON DELETE CASCADE` from `track_embeddings` is what makes "a file change drops the vector, its feel, map and mood rows" true (upstream cascade, same) [V]; `matching_state`/`mood_state` singleton `CHECK`; snake_case columns; the `id='default'` text key kept for the lock row. **Lock order** (copy upstream's rule): state row first, then score/feel rows, then track rows.

`user_mood_mix`, `tracks.mood_tags` and the 22 `mood_state` column list are written in full in the migration; mix and tag columns belong to M5b's migration (they need recipes).

### 9.4 Library re-rank of similar artists

`artistSimilarity.ts` [V lines 125-225]: after merging ListenBrainz+Deezer, look up the seed's and each result's library artist row, load `artistProfiles` for all, and for each result with both profiles `closeness = artistMatch(seed, result)` (cross term / sqrt(self·self), genre term only between different artists, null when an artist has no spread), `distance = 1 − closeness`, `delta = 0.1·(1 − 2·clamp(distance, 0, 1))`, `match = clamp01(r.match + delta)`; artists without data are not adjusted; **stable sort by adjusted match desc**. kima-rs replaces `RERANK_MAX_DELTA = 0` in `kima_ident::similar` (the M3 note "needs M5's matching state") with the real function; a failure logs and returns the unranked list with `inLibrary` false, as upstream (D-189). Call sites unchanged.

### 9.5 Invalidation and rerun

- File change / path change: existing `invalidate_track_ml` deletes the embedding; cascade removes feel, map, moods, readings. The next `buildMatching` sees a changed fingerprint (count and hash) and rebuilds; the map keeps every other track's place.
- Re-run (`control::rerun(step)`), in one transaction, **first** `DELETE FROM matching_state` (so an in-flight map write finds nothing to lock) and `DELETE FROM mood_state`, then step-specific clears, per the matrix in `enrichmentRestart.ts` [V]: `identification|analysis|vectors` clear similarity+moods; `artists` clears similarity only; `moods` clears moods only; `similarity` clears similarity (state, feel, map). "Clearing similarity" = `DELETE FROM matching_state` (+ `matching_map`, `matching_feel`). Tests: the four race tests of `matching.integration.test.ts` (§10).

## 10. Test and parity plan (what the tasks verify)

| Upstream test (v1.12.3) | M5 equivalent | Kind |
|---|---|---|
| `matching/__tests__/formula.test.ts` + `fixtures/parity.json` (50 synthetic tracks, 16-d, expected scores, neighbours, sets, artist match) | `kima-vibe/tests/formula_parity.rs` reading the copied fixture | parity, 1e-12 |
| `exactMath.test.ts` (ulp bounds, bits, "same bits on every engine") | `exact_math` tests vs `std` within 1 ulp (8 for pow) + a frozen bit table generated from upstream (Task 0) | parity |
| `mapLayout.test.ts` + `map-parity.json` (umap-learn 0.5.12 / sklearn 1.9, rel 2e-5) | `kima-vibe/tests/umap_parity.rs` | parity, 2e-5 |
| `matching.integration.test.ts` (state, feel, map, rank, caps, alchemy, path, re-run races, deadlocks) | `kima-db/tests/vibe_*.rs` on a real isolated Postgres (never mocked) | integration |
| `moods/__tests__/{heads,moods.integration,moodTags}`, `shipped-reference.json` | `kima-vibe/tests/moods_parity.rs`, `kima-db/tests/moods.rs` (tags are M5b) | parity + integration |
| `routes/__tests__/{vibeMap,vibeMood,vibeMoodSummary,vibeTitles,similarUnanalysedSeed}.route.test.ts` | `kima-api/tests/vibe.rs` through the real router | route |
| `services/__tests__/{textEmbeddingBridge,textSearch,textSearchExact.integration,knnLimit,hybridSimilarity.*}.test.ts` | `kima-db/tests/vibe_search.rs`, `kima-api/tests/vibe.rs` | integration |
| `artistSimilarity.rerank.test.ts` | `kima-ident/tests/similar_rerank.rs` | unit |
| Real-model: none upstream | `embedding_e2e`-style ignored test with `KIMA_CLAP_MODEL_DIR` + `KIMA_VIBE_ASSET_DIR` panicking when unset | e2e, after Q1 |

Goldens that do not exist upstream (needed for a bit-level check of the Rust port): run upstream's TS (`tsx`, node 20/24 under `~/.hermes/tools`) on the 50-track fixture and a ~300-track synthetic library once, store `layoutLibrary`/`placeTracks` outputs as `kima-vibe/tests/fixtures/ts-golden-*.json` with the generator script and versions recorded (Task 0). **This needs `npm install` of upstream's backend deps or only `tsx` + the pure modules; not verified that the pure modules import without Prisma [?]** (`formula.ts`, `mapLayout.ts`, `mapCompute.ts`, `neighbours.ts`, `exactMath.ts` import only each other and `./worker`; `neighbours.ts` imports `runInWorker` from `./worker` [V], which is harmless to stub).

## 11. Vector index decision: HNSW (kept), exact scan below a threshold

### 11.1 Where an index is used

Only `POST /vibe/search` can use an ANN index. The ranking paths (`similar`, `alchemy`, `path`) compute a three-term score per track with no `ORDER BY embedding <=> …` on its own, so Postgres scans every eligible row **by design** upstream [V `rank.ts:scoreQuery`]; that is part of "exactness" and is kept (§12).

### 11.2 Evidence

1. Upstream moved IVFFlat -> HNSW on purpose: "IVFFlat degrades after bulk inserts without REINDEX; HNSW does not", HNSW `m=16, ef_construction=200` [V migration `20260307000000_switch_ivfflat_to_hnsw`, repeated in `20260915000000_...repair`]. IVFFlat needs a `lists` value chosen from the row count (upstream had `lists=224`) and its centroids are trained on whatever was in the table at build time; a library that grows from 0 would be trained on an empty/small table.
2. Upstream measured filtered search on **50,000 real music-CLAP vectors**: pgvector's iterative scan with `max_scan_tuples = 20,000` filled 40 of 50 results, `40,000` filled 49.5, "the most any higher value reached" [V comment in `utils/knn.ts:12-17`]. The filters (missing files, vectors of other models) are exactly why the plain walk comes back short.
3. [M] In a scratch `pgvector/pgvector:pg17` (pgvector **0.8.7**, so iterative scans and `l2_normalize` exist [M `vector.control`]) with 100,000 random unit 512-d vectors (5 % rows filtered out):
   - exact blended ranking in upstream's SQL shape (cosine + feel term + bonus join + two window caps): **0.41 s** per seed (3 runs: 440/407/406 ms) on this 32-core host;
   - plain exact top-20 by cosine, filtered, parallel seq scan: **165 ms**;
   - HNSW (m=16, ef_construction=64, the current `0024`) build: **16 s and 71 s** in two runs (260 MB index);
   - IVFFlat (lists=100) build: **1.8 s**.
   - **Recall@20 on random vectors is meaningless for CLAP**: HNSW ef=100 gave 0.153, ef=40 0.128; IVFFlat probes=1/10 gave 0.053/0.113. Random 512-d points have no neighbourhood structure, so this shows only that the index cannot be trusted blindly, **not** a ranking of HNSW vs IVFFlat on real music vectors. **No real-vector recall measurement exists in this design [?]**; Task 14 measures it on the first real library.
4. kima-rs's existing HNSW (`0024`, defaults) is already built and maintained by the M4 writes.

### 11.3 Decision

**Exact-scan threshold measurement (Task 6, [M]).** `tools/vibe-bench.sh` (example `kima-db/examples/vibe_bench.rs`): run-owned `pgvector/pgvector:pg17` (pgvector 0.8.7), `shared_buffers=1GB`, default `max_parallel_workers_per_gather=2`, 32-core host, random unit 512-d vectors, 5 % of tracks missing, HNSW index dropped (an exact `ORDER BY (embedding <=> q) + 0, track_id LIMIT 20` never uses it), 30 queries after 2 warm-up queries, each with a fresh query vector:

| tracks | eligible | median | p95 | max |
|---|---|---|---|---|
| 25,000 | 23,754 | 163 ms | 169 ms | 177 ms |
| 50,000 | 47,572 | 284 ms | 288 ms | 297 ms |
| 100,000 | 95,102 | 303 ms | 311 ms | 314 ms |
| 200,000 | 190,180 | 415 ms | 425 ms | 428 ms |

A second run with `max_parallel_workers_per_gather=8` gave the same at 100k/200k (p95 319/428 ms; 25k/50k noisier, p95 195/399 ms). Growth is sub-linear (the parallel scan only engages above a table-size threshold). **Proposal: keep 200,000** — p95 at 200k is 0.43 s, far under the ~1.0 s rule, so the default is not lowered. It is a measurement on random vectors on one host, not a production guarantee; the embedding time of the query (≈150 ms [?]) is extra.

- Keep `hnsw (embedding vector_cosine_ops)`; do **not** introduce IVFFlat. Rebuild it with `ef_construction = 200` if Task 14's recall on real vectors is below 0.95@20 at the chosen `ef_search` (a migration then, not now).
- Text search: `eligible_count <= KIMA_VIBE_EXACT_MAX` (default **200,000**, configurable; owner Q6 2026-11-06 asked for a proposal, measured in Task 6 below) -> **exact** ordered scan (`ORDER BY (embedding <=> q) + 0, track_id`); above it -> index path as upstream: `SET LOCAL hnsw.ef_search = clamp(2·limit, 100, 1000)`, `hnsw.iterative_scan = relaxed_order`, `hnsw.max_scan_tuples = 40000`, re-sort outside the materialised CTE with `+ 0`, equal distances by id, and fall back to the exact read when the index returns fewer than `limit` [V `textEmbeddingBridge.ts:205-235`, `utils/knn.ts`]. Both paths must give the same top-`limit` on a clustered synthetic set (Task 14 test). D-184.
- The 2-stage rank (index shortlist, exact rescore) is **not** done; the exact scan is upstream's behaviour and fits the budget up to ≈300 k tracks (§12.1).

## 12. Performance budget

### 12.1 Interactive paths (library of 100,000 eligible tracks, 8 cores, warm cache; **targets**, only the SQL proxies are measured)

| Operation | Budget | Evidence |
|---|---|---|
| `similar`, `alchemy` (one exact scored pass + caps) | p95 ≤ 1.0 s | [M] Task 6 through the real code (`rank_matches`, 95k eligible of 100k, genre term on, 32 cores, default 2 parallel workers): **steady state 0.60-0.71 s** per call (median 0.61-0.63 s over four runs); in 3 of 4 runs the first 2-3 calls after the state build took 1.1-1.2 s (cause not isolated [?]; likely first-touch hint-bit/buffer effects on freshly written rows), so a p95 over 20 calls reads 1.1-1.2 s in those runs and 0.63 s in the clean one — **inside the 1.0 s budget in steady state, over it for the first calls after a bulk write**. The earlier 0.41 s proxy had no genre/bonus join. `top_scores` median 0.56 s; two-set `score_library` (path) median 0.69-0.79 s; state build 33 s (first fill, 95k feel rows), unchanged-library rebuild check 2.3 s |
| `path` (2-set scored pass + n waypoint scans in Rust) | p95 ≤ 2.0 s | scan once for both sets (as upstream); waypoint scans are `n ≤ 50` passes over a 100k-row in-memory `(id, artist, s0, s1)` vector; [M] Task 7b `generate_song_path` through the real code at 95k eligible: median 0.73 s, p95 0.75 s (5 runs, smooth, default length; the scoring pass alone 0.68 s) |
| `search` | embed ≤ 150 ms (CPU, [?]) + rank ≤ 0.5 s | [M] 165 ms exact top-20 |
| `map` read | ≤ 1.5 s (≈30 MB JSON, gzip ≈ 4 MB) | upstream's size comment "tens of megabytes ... compresses ~8x" [V `vibe.ts:27`]; cache gzip keyed by version |
| `mood-summary`, `mood/:id` | ≤ 100 ms | snapshot in memory |
| Beyond 300 k tracks | revisit | rank is O(n) SQL per request |

### 12.2 Background

| Pass | Budget | Notes |
|---|---|---|
| Feel update | ≤ 60 s for a first fill of 100k | one `INSERT ... SELECT` |
| State build | ≤ 60 s | holds the state row lock; timeouts as upstream |
| Mood scoring (22 dot products/vector) + stats | ≤ 120 s first fill | one statement |
| **First layout** | ≤ 10 min at 100k, 8 cores; hard memory cap 1 GB | n²·512 = 5.1e12 multiply-adds, plus fuzzy graph and 500 epochs. Estimate [?]: Task 8 microbenchmarks the neighbour sweep and fixes the f32/f64 accumulation choice (D-185). Upstream JS does this single-threaded in a worker. |
| Incremental placement | ≤ 5 s per 100 new tracks | each new track scans the laid-out set |

The enrichment cycle must never block on the layout (the cycle awaits it only through the single-flight handle with a status; it proceeds when the task is running). `updateMap` runs every cycle but returns after one count query when the map is complete [V].

## 13. Hand-offs

| To | What M5 provides / needs |
|---|---|
| **M4 follow-up** | New CLAP checkpoint `laion-clap-music-onnx-v1` audio+text, numpy mel front-end, pipeline/model strings that §4.3 copies; whole-track windows (already). The 90 s tail rule differs from v1.12.3 (middle 90 s) — M4's. |
| **M5b** | `mood_state` readings and 22 heads (recipes), `moodTags` writer (needs recipes' `definedCtes`), `UserMoodMix` + `/api/mixes/mood/buckets/*`, `/whats-new/mood-mixes*`, radio row's `vibeEmbeddings` gate, `familiarity.ts`/`nightPlays.ts` which call `matchingContext + topScores/rankMatches` (public `kima_db::vibe` API: `matching_context`, `top_scores`, `rank_matches` must be `pub`), `dropCachedMixes` on mood change (a `MoodsChanged` event). |
| **M-STATS** | S2 uses `energy/valence/bpm/key/instrumentalness`: take valence/instrumentalness from `track_clap_readings`. `play_audit_ignore` unaffected. |
| **M7** | `albums.location` -> extend `ELIGIBLE`; Discover songs never scored; Discover recommendations stay outside M5. |
| **M10 (kima-web)** | Pages and components, with the API calls that feed them [V `frontend/lib/api.ts:2056-2094`]: `app/vibe/page.tsx` (563 lines) + `features/vibe/{map,tabs,TrackPanel,VibeAlchemy,VibeSongPath,useVibeMap,useVibeSearch,MoodLegend,ScenePanel,VibeToolbar}`; `features/library/useLibraryVibe.ts` + `CollectionMasthead.tsx` (mood band, `GET /vibe/mood-summary`); `features/stats/useTrackMood.ts` (`GET /vibe/mood/:id`); sidebar item "vibe" (`components/layout/navItems.ts`); player "match this vibe" (`/vibe/similar`). Map polling on 202. WebGL scene is the large item (outside M5). |
| **M12** | Remote ML supplies vectors only; the `MATCHING_SOUND` identity check rejects mixed fleets (model id + sha + pipeline). |
| **Public vibe (Phase E / later)** | §2 item 5; refs `routes/publicVibe.ts`, `services/{publicLibraryExport,liveListening}.ts`, `workers/moodSnapshotWorker.ts` (export refresh), D-062. |

## 14. Divergence rows to reserve

The highest used on `main` is **D-180** (Next.js UI, `docs/DIVERGENCES.md`); `feature/m-sync` holds D-163..D-179 [V grep of both trees]. M5 reserves **D-181 – D-195**. Status of each is `to-decide` or `open` until its task lands; text is drafted in plan Task 20.

| ID | Area | Gist |
|---|---|---|
| D-181 | Vibe / formula | Rust port; probe id = sha256 of the asset file; `FORMULA_VERSION` own counter; real columns read as f32 not text-parsed f64; SQL scoring kept. |
| D-182 | Vibe / model gate | `(model_id, model_sha256, pipeline_version)` instead of the `model_version` label; `created_at` is the freshness token; 503 text for "needs the music model". |
| D-183 | Vibe search | In-process `embed_text` instead of Redis pub/sub; 503 vs 504 mapping. |
| D-184 | Vibe search | Exact scan up to `KIMA_VIBE_EXACT_MAX`, HNSW + iterative scan above. |
| D-185 | Vibe map | Native thread + rayon, f32 storage; exact-math port; accumulation choice from Task 8's benchmark; gzip cache key. |
| D-186 | Moods / eligibility | No `analysisStatus`, `corrupt`, `albums.location`; readings from CLAP only until MusiCNN/DSP; assets outside Git. |
| D-187 | CLAP readings | Zero-shot readings computed after the fact from the stored vector (upstream: at embedding time). |
| D-188 | Enrichment | Phases `mood`/`similarity`; progress `moods`; rerun steps real. |
| D-189 | Similar artists | Re-rank enabled (supersedes the M3 delta 0). |
| D-190 | Subsonic | `getSimilarSongs(2)` is vibe-based (ranked by the formula from the seed track) instead of upstream's random artist+similar-artist pick; artist-based path kept as fallback when the seed has no eligible embedding. Owner decision Q8 (2026-11-06). Amends D-086. |
| D-191 | Public vibe | Not in M5 core; D-062 stays. |
| D-192 | Genres | Matching genres from `artist_genres_effective` + `user_genres` (replace), MB-list filter. |
| D-193 | Validation | UUID checks replace CUID regex; non-numeric `length` ignored. |

## 15. Risks and unverified items

1. **Model checkpoint mismatch (§2.3)** — highest risk; real-model acceptance blocked by the M4 follow-up (Q1).
2. **Rights of `feelProbe.json` (CC BY-NC) and `moodHeads.json` (unstated training sets)**; M4 is noncommercial, redistribution is not established [?]. Assets stay outside Git; Q3.
3. **First-layout time at 100k** unmeasured (§12.2). Mitigation: Task 8 benchmark before Task 9; fall back to approximate neighbour candidates only with an owner-approved D-row (it would break the "exact" claim).
4. **HNSW recall on real CLAP vectors** unmeasured (§11.2.3).
5. **`created_at` as freshness token**: if any writer touches `track_embeddings` without rewriting it, `matching_feel` goes stale silently. Test: every upsert path changes `created_at` (D-182).
6. **`hashtextextended` fingerprint collisions** are a theoretical staleness risk, as upstream.
7. **Map JSON size** at 100k (≈30 MB uncompressed) in axum: use a streaming gzip body and `Content-Length` from the cache.
8. **Sqlx offline cache**: new queries need `cargo sqlx prepare --workspace`; the state/rank SQL is dynamic (`unnest` of N bonus columns) and uses `query()` not `query!()`; those paths are tested against a real database only.
9. **Not verified**: `shipped-reference.json` contents; whether upstream's pure modules run under `tsx` without Prisma (§10); `featureDetection`'s kima-rs wiring (M4); the exact `currentPhase` naming map (`embedding` vs `vibe`); whether `albums.updated_at` exists for the map cache key; pgvector version in production images (the floating `pg17` tag gave 0.8.7 here, CI/Docker should pin).

## 16. Open questions for the owner

1. **Q1 Sequencing:** build M5 now against synthetic vectors (real-model parity later) or land the M4 follow-up (new checkpoint) first? Recommended: build now; the engine, state, map and moods do not need the model to be tested, only to be accepted.
2. **Q2 Scope of M5:** is "backend vibe + moods + rerank + similar songs" (Phases A-D) the milestone, with the public vibe feed (Phase E) deferred? Recommended: yes.
3. **Q3 Assets:** may `feel_probe.json` (CC BY-NC) and `mood_heads.json` be provisioned privately like model weights (outside Git, SHA-pinned), or do you want the rights of their training data checked first? Also: do you want the mood heads at all before that check (they only affect `moods`, `mood-summary`, M5b)?
4. **Q4 CLAP zero-shot readings** (§8.3): in M5 (recommended; 1.5 d) or in M5b?
5. **Q5 Genre source** (D-192): follow M-SYNC Q5 (person's list replaces enriched) — confirm both use the same function.
6. **Q6 Exact-scan threshold** default 200,000 (§11.3): acceptable, or always exact, or always index (upstream)?
7. **Q7 Layout cost:** if Task 8 shows > 10 min at 100k, accept (a) a longer background layout, (b) f32 accumulation (D-row), or (c) approximate neighbour candidates (breaks exactness)?
8. **Q8 `getSimilarSongs`:** upstream's artist-based behaviour (recommended, parity) or a vibe-based improvement (a divergence the frontend would not notice)?
9. **Q9 Map cache key** (§5.1): accept a version key instead of hashing the JSON (identical bytes in practice, [?] for rare title/cover edits)?

### 16.1 Owner decisions (2026-11-05) and assumptions made while building Phases A-B

Owner: **Q1** build now on synthetic vectors; real-model acceptance waits for M4's checkpoint swap (`kima_db::vibe::MATCHING_SOUND` is the single constant to change). **Q2** M5 = plan phases A-D; Phase E (public vibe feed) is later. **Q3** `feelProbe.json` and `moodHeads.json` stay private outside Git, SHA-256 pinned, loaded from `KIMA_VIBE_ASSET_DIR`; the mood heads are not used or generated before their training-data licences are checked, so moods sit behind the `vibe_mood_heads` capability (unavailable when the file is absent) and are tested with synthetic heads.

Assumptions (everything else follows the plan's recommendation):

1. **`shipped-reference.json` is not copied.** Its four 512-d vectors have the statistics of real embeddings and its scores come from the unlicensed heads; the plan's `moods-reference.json` does not exist and `shipped_reference_scores_to_1e-4` is `#[ignore]` behind `KIMA_VIBE_ASSET_DIR` (Task 11).
2. **Goldens use Node's type stripping, not `tsx`** (Node 26.7; the pure modules are copied with `.ts` import suffixes, `./worker` stubbed, `gaussian` exported). The pure modules import without Prisma: the spec's [?] is resolved. An extra `ts-golden-library.json` carries the golden inputs.
3. **Capabilities:** `vibe_assets` = the feel probe (no probe, no formula); `vibe_mood_heads` = the 22 heads. The spec's single `vibe_assets` entry could not express "moods off, similarity on" (section 4.4).
4. **Migration number 0040** (M-SYNC holds 0029+). The phase check lists all seven phases including `analysis` (0027).
5. **`MATCHING_SOUND`** uses the spec's expected music-checkpoint strings (`music_audio`, `clap-music-audio-v1`, sha `ab1bf5eb...`); they are unconfirmed until the M4 follow-up. Until then a library embedded by today's M4 has zero eligible tracks.
6. **`ELIGIBLE` inlines the identity as SQL literals** (checked to `[A-Za-z0-9._-]`), so it has no bind parameters.
7. **Ids in `kima-vibe` are strings**; lowercase hyphenated UUID text sorts like the uuid bytes Postgres sorts by.
8. **The SQL two-window cap equals the walk only when each album belongs to one artist** (true here: a track's artist is its album's artist). `two_window_differs_for_shared_albums` documents the counter-example.
9. **`pow` is held to 8 ulp only on upstream's domain** (x in [0,60), exponent 0.895...), not on arbitrary exponents.
10. **serde_json `float_roundtrip`** is required: the default parser was 1 ulp off on a synthetic weight.
11. **`KIMA_VIBE_EXACT_MAX=0` is valid** and forces the index path; non-numbers are a config error.
12. **Asset cache lives in `EmbeddingRuntimeState`** (loaded-once model state) so no `IdentCtx` constructor changes.
13. Task 3 step 3 (EXPLAIN on 10k rows) moves to Task 5, where the feel-join and map-count queries first exist.

### 16.2 Owner decisions (2026-11-06)

- **Q4:** CLAP zero-shot readings (instrumentalness, acousticness, valence/arousal; §8.3) are **in M5** (plan Task 17), not M5b.
- **Q6:** the owner has no opinion and asked for a proposal. Proposal: `KIMA_VIBE_EXACT_MAX` default **200,000** as specced, configurable. Task 6 measures exact top-20 latency at 25k/50k/100k/200k synthetic vectors on the build host; if exact p95 at 200k exceeds ~1.0 s the default is lowered to the largest measured size under that budget. Numbers and the resulting default are recorded in §11.3.
- **Q7:** if the first layout at 100k is slow, Task 8 first finds out *why* (profile distance computation vs neighbour search vs optimisation epochs; compare with upstream's algorithm and, if the TS reference runs, its real runtime on the same input), records the findings, then picks the cheapest faithful fix. No approximation is pre-committed.
- **Q8:** `getSimilarSongs`/`getSimilarSongs2` are **vibe-based** (deliberate divergence, D-190). The artist-based path stays as the fallback when the seed track has no eligible embedding. The D-190 row lands in the commit that lands the behaviour (Task 19).
- **Q9:** no answer; the plan's recommended option (version key for the map cache) applies.
