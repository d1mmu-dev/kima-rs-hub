# M2: Subsonic — Design

Date: 2026-10-17
Status: Draft, pending owner approval
Parent spec: `docs/superpowers/specs/2026-10-16-kima-rust-design.md` (§2 `kima-subsonic`, §5 auth, §8 row M2,
§10 error mapping, §11 snapshot tests)
Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md` (rows M1 and M2, "Notes carried forward")
Builds on: M0 and M1 as merged on `main` (crates kima-core, kima-db, kima-jobs, kima-library, kima-api,
kima-server; forked frontend as a static export).
Upstream reference: kima-hub @ `12faf1cd` (`backend/src/routes/subsonic/*`, `middleware/subsonicAuth.ts`,
`utils/subsonicResponse.ts`, `docs/subsonic.md`, `frontend/features/settings/components/sections/SubsonicSection.tsx`).

## 1. Intent

After M1 the owner can browse and play the library in a browser. M2 makes the same library playable from
phone and desktop music apps: DSub, Symfonium, Feishin, Amperfy, Substreamer and anything else that speaks
the Subsonic API or its OpenSubsonic extension.

A user opens **Settings > Native Apps** and creates a token for each app ("Phone", "Car"). They type the server
address, their username and the token into the app. The app can then browse, search, stream (original or a
lower-bitrate transcode), download, show covers, edit playlists, sync its play queue, save bookmarks and star
things. Every finished song shows up in the user's kima listening history. Revoking a token disconnects that
one app at once.

Upstream already has a Subsonic API, and it is the reference here. kima-rs differs in one place that matters:
upstream refuses the classic token-and-salt handshake (`t=`/`s=`, error 41), and many clients use it by default.
kima-rs supports it for app tokens (§5).

Success: after M2, the owner can connect DSub, Symfonium and Feishin to their server with a token. In each app
they can browse the whole library, play a FLAC both as the original and as a 128 kbps transcode, seek inside the
song, scrobble it and see the play in kima's history, star an album and see it as starred in another app, and
save the queue on one device and restore it on another. Revoking the token makes the app's next request fail
with "Wrong username or password".

## 2. Goals

From the parent spec's M2 row:
- A new crate `kima-subsonic` serves the OpenSubsonic API under `/rest/*`, in XML (the default) and JSON
  (`f=json`). It is a thin adapter over the same `kima-db` and `kima-library` code that `kima-api` uses.
- Per-app Subsonic tokens that can be revoked, plus Subsonic token+salt auth (§5).
- Responses are snapshot-tested with `insta`, in both formats (§10).

Scope, which is everything upstream's Subsonic API does that M0 and M1 data can back (§4 lists each endpoint):
- browse (artists, indexes, folders, albums, songs) and album lists;
- search (`search`, `search2`, `search3`) and random songs;
- stream, download, HLS stub, transcode decision and stream, and cover art;
- playlists (list, read, create, update, delete);
- scrobbling into kima's plays;
- play queue (by id and by index) and bookmarks;
- star, unstar, starred lists and ratings;
- user info and admin user management;
- the "compat always answers" stubs: internet radio, chat, video, captions, jukebox, avatar.

Features that belong to later milestones answer with an empty list or a "not supported" error, as upstream's
own stubs do: podcasts (M6), lyrics, artist info and genres (M3), similar songs (M5), and share links (M8).

Carried forward (roadmap M1 row, both must land in M2):
- A periodic scheduler in `kima-jobs` that queues a safety-net library reconcile (§9.1).
- An `AdminUser` extractor that replaces the seven copies of the admin check in `kima-api` (§9.2).

Folded in from the M1 final review, because they touch what Subsonic serves (§9.3):
- watcher and stream symlink handling made consistent with the reconcile;
- a cover-art cache that notices changed art and has a size cap;
- `ETag`/`Last-Modified`/conditional requests on streams;
- ffmpeg-dependent tests that fail rather than pass silently when CI lacks ffmpeg.

## 3. Non-goals and anti-goals

Non-goals (M2):
- Podcasts, lyrics, artist bios and images, genres, similar songs and radio, share links. Their endpoints are
  stubs (§4) until their milestones.
- 2FA. kima-rs has no 2FA until M8. When it lands, the password path in §5.3 refuses 2FA accounts, as upstream
  does.
- Rate limiting. Upstream limits `/rest` to 1500 requests a minute per IP. kima-rs has no rate limiter until M8
  (D-089).
- `X-API-Key` auth on `/api` with the same tokens. That is M8 (roadmap note R4). In M2 an app token works only on
  `/rest` (§5.1).
- Device linking (upstream `deviceLink.ts`). M8.
- Jukebox mode, internet radio, chat, video and captions. Upstream does not have them either.
- A vertigo (`kima-web`) settings page. The token UI lives in the forked Next.js frontend; M10 ports it.

Anti-goals (things M2 must not do):
- Never answer "ok" to a write that stored nothing. Upstream's rule, kept: an unsupported write gets error 0
  with a reason (`compat.ts`).
- Never put a driver or SQL error message in a response. Internal errors answer code 0 with the fixed text
  "Internal error" and go to the log.
- Never store an app token in a form that a database dump alone can turn back into the token (§5.2).
- Never log Subsonic credentials. `/rest` request logging records the path only, never the query string (§5.6).
- No second copy of the stream, transcode or cover-art logic. `kima-subsonic` calls the code `/api` uses (§8).
- Never treat a stub as a feature. A stub returns an empty list or an error, never invented data.

## 4. Endpoints

All paths are under `/rest/`. Each one also answers without the `.view` suffix (`/rest/ping` and
`/rest/ping.view`), as upstream does for clients such as Musa. Each path accepts GET, HEAD and POST. With POST,
parameters come from the query string and from an `application/x-www-form-urlencoded` body, merged, with
repeated keys collected (the OpenSubsonic `formPost` extension). Every endpoint except `tokenInfo` and
`getOpenSubsonicExtensions` needs Subsonic auth (§5).

Status meanings:
- **full**: everything the endpoint does in the OpenSubsonic spec that upstream also does, backed by real data.
- **partial**: works, but some parameters or variants are not backed yet (named in the row).
- **stub**: answers with an empty result or a "not supported" error, as described in the row.

Columns: upstream file is under `backend/src/routes/subsonic/`. "Answer" names the response element, and
notes what kima-rs does where it differs from upstream. Each difference has a DIVERGENCES row (§12).

### 4.1 System

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 1 | `ping` | full | index.ts | empty ok |
| 2 | `getLicense` | full | index.ts | `license valid=true email="kima@kima" licenseExpires="2099-12-31T23:59:59"` |
| 3 | `getMusicFolders` | full | index.ts | one folder `id=1 name="Music"` |
| 4 | `getOpenSubsonicExtensions` | full | index.ts | no auth needed (OpenSubsonic requires that; upstream wants auth). Advertises `apiKeyAuthentication` v1, `formPost` v1, `indexBasedQueue` v1. Leaves out `songLyrics` (M3) and `getPodcastEpisode` (M6) until they are real |
| 5 | `tokenInfo` | full | index.ts | no Subsonic auth; `apiKey` param; `tokenInfo username=…`; 10 when `apiKey` is missing, 44 when it is unknown |
| 6 | `getScanStatus` | full | index.ts | `scanning` = a reconcile is queued or running; `count` = number of present tracks (upstream: the number of queued scan jobs) |
| 7 | `startScan` | full | index.ts | admin only (50 otherwise); `kima_library::jobs::request_reconcile`; answers `scanStatus scanning=true count=…` |

### 4.2 Browsing

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 8 | `getArtists` | full | library.ts | `artists ignoredArticles="The A An Le La Les Los Las Die Der Das"`, `index name=X` buckets keyed on the first letter of `sort_name` (§7.4), A–Z, else `#`; only artists with present tracks; each `artist` has `id name albumCount coverArt="ar-<id>" starred? userRating?` |
| 9 | `getIndexes` | full | library.ts | same data under `indexes`, plus `lastModified` (epoch ms of the newest track change). With `ifModifiedSince` ≥ `lastModified` it answers an empty `indexes` (DSub polls this way; upstream ignores the parameter) |
| 10 | `getArtist` | full | library.ts | artist plus its `album` list (year newest first, then title); 70 "Artist not found" |
| 11 | `getMusicDirectory` | full | library.ts | `id=1` lists artists as `child isDir=true`; an artist id (bare, `ar-`, `artist:`) lists its albums; an album id (bare, `al-`, `album:`) lists its songs; 70 "Directory not found" |
| 12 | `getAlbum` | full | library.ts | album plus `song` list, ordered by disc, track number, title; 70 "Album not found" |
| 13 | `getSong` | full | library.ts | one `song`; 70 "Song not found" |
| 14 | `getAlbumList2` | partial | library.ts | `albumList2` with `album` entries. Types: `random`, `newest` (album `created_at` descending), `highest` (the caller's rating), `frequent` and `recent` (the caller's plays), `alphabeticalByName` and `alphabeticalByArtist` (`sort_name`), `starred` (albums the caller starred), `byYear` (`fromYear`/`toYear`, descending when from > to; 10 if either is missing). `byGenre` answers an empty list until M3 has genres. A missing or unknown `type` acts as `newest`, as upstream. `size` defaults to 10 and is capped at 500; `offset` defaults to 0 |
| 15 | `getAlbumList` | partial | library.ts | same lists under `albumList`, but as `album` entries in the folder (Child) shape: `isDir=true title=… parent=<artistId>` plus the ID3 fields (upstream sends only the ID3 shape, which leaves the title blank in folder clients) |

### 4.3 Search and lists

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 16 | `search3` | full | search.ts | `searchResult3` with `artist`, `album`, `song`; `artistCount`/`albumCount`/`songCount` default 20, capped at 500; `*Offset` default 0. The query uses the M1 search (D-053). An empty query (`""` or `''` too) lists everything in `sort_name`/title order, paged by the same parameters: Symfonium and others sync their whole library this way (upstream answers empty) |
| 17 | `search2` | full | search.ts | the same, under `searchResult2` |
| 18 | `search` | partial | search.ts | legacy `searchResult`; uses `query`, or failing that `any`, then `title`, `album`, `artist`; `count`/`offset` for songs. Upstream reads only `query`. No `newerThan` |
| 19 | `getRandomSongs` | partial | search.ts | `randomSongs`; `size` default 10, 1..500; `fromYear`/`toYear` filter on the track year, falling back to the album year. `genre` answers an empty list until M3 |
| 20 | `getSongsByGenre` | stub | search.ts | `songsByGenre` empty (no genres until M3); 10 when `genre` is missing |
| 21 | `getTopSongs` | full | search.ts | `artist` name matched case-insensitively, exact first, then substring; the artist's songs ordered by play count over all users, then title; `count` default 50, 1..500; an unknown artist answers empty `topSongs` |
| 22 | `getSimilarSongs` | stub | search.ts | `similarSongs` empty until M5; 10 when `id` is missing |
| 23 | `getSimilarSongs2` | stub | search.ts | `similarSongs2` empty until M5 |
| 24 | `getGenres` | stub | library.ts | `genres` empty until M3 (upstream lists enrichment genres) |
| 25 | `getNowPlaying` | full | playback.ts | `nowPlaying` with `entry` = a song plus `username minutesAgo playerId playerName`. Sources: Subsonic "now playing" scrobbles (§6.3) and web-player `playback_state` rows of type `track` updated in the last 2 hours, as upstream; one entry per (user, player), newest first, at most 50 |

### 4.4 Media

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 26 | `stream` | full | playback.ts | the audio, through the shared stream code (§8.2): ranges, `ETag`, transcode. `format=raw` is the original; `maxBitRate` picks the quality (§6.1); `timeOffset` and `estimateContentLength` are ignored. 70 "Song not found" for an unknown or missing track. No play is logged here, as upstream: only `scrobble` counts |
| 27 | `download` | full | playback.ts | always the original, with `Content-Disposition: attachment; filename*=UTF-8''<file name>` |
| 28 | `hls` (`hls.view`) | partial | playback.ts | as upstream: a one-entry `application/vnd.apple.mpegurl` playlist pointing at `stream.view` with the caller's auth parameters and `maxBitRate` from the first `bitRate`. No segmenting |
| 29 | `hls.m3u8` | partial | playback.ts | alias of `hls` |
| 30 | `getTranscodeDecision` | partial | compat.ts | `transcodeDecision` from the track's real format, as upstream: direct play unless the client asked for a different `format`; `mediaType` other than `song` → 70 |
| 31 | `getTranscodeStream` | partial | playback.ts | `mediaType=song` streams as `stream` with `maxBitRate`; `podcast` → 70 until M6; anything else → 0 "Unsupported mediaType" |
| 32 | `getCoverArt` | full | playback.ts | album cover through the shared cover code (§8.2, §9.3). `id` may be an album id, `al-<id>`, a track id or `tr-<id>` (its album's cover); `size` snaps to the M1 sizes (D-043). `ar-<id>` and artist ids answer 70, since kima-rs has no artist images until M3. 70 "Cover art not found" when there is no art |
| 33 | `getAvatar` | stub | compat.ts | as upstream: a 1×1 transparent PNG for a known `username`, 70 for an unknown one, 10 when missing |

### 4.5 Scrobbling, queue and bookmarks

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 34 | `scrobble` | full | playback.ts | `submission=true` (the default) records a play with source `SUBSONIC`; `submission=false` updates now playing (§6.3). Accepts several `id` with matching `time` (epoch ms), as the spec allows; upstream reads only the first (offline clients send batches) |
| 35 | `getPlayQueue` | full | queue.ts | `playQueue current position username changed changedBy` plus `entry` songs, read from the shared `playback_state` (§6.4) |
| 36 | `getPlayQueueByIndex` | full | queue.ts | `playQueueByIndex currentIndex …` |
| 37 | `savePlayQueue` | full | queue.ts | `id` (repeated), `current`, `position` (ms). With ids, `current` must be one of them (10 otherwise). No ids clears the queue |
| 38 | `savePlayQueueByIndex` | full | queue.ts | `id`, `currentIndex` (required with ids, 0..len−1; forbidden without), `position` |
| 39 | `getBookmarks` | full | playback.ts | `bookmarks` with `bookmark position username comment created changed` and the `entry` song; tracks flagged missing are left out |
| 40 | `createBookmark` | full | playback.ts | `id`, `position` (ms, required, integer ≥ 0), `comment`; upsert per (user, track); 70 for an unknown track (upstream fails with a foreign-key error) |
| 41 | `deleteBookmark` | full | playback.ts | deletes the caller's bookmark for `id`; ok even when there was none |

### 4.6 Playlists

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 42 | `getPlaylists` | full | playlists.ts | the caller's playlists and other users' public ones (minus those the caller hid in the web app), newest first; `playlist id name songCount duration public owner created changed`. `username` is ignored |
| 43 | `getPlaylist` | full | playlists.ts | playlist plus `entry` songs in order; another user's private playlist is 70 "Playlist not found", as the web API does (D-063; upstream says 50) |
| 44 | `createPlaylist` | full | playlists.ts | `name` + `songId…` creates a private playlist; `playlistId` + `songId…` replaces its songs (owner only, 50 otherwise). Unknown, missing or repeated song ids are skipped. Answers the full playlist (OpenSubsonic 1.14) |
| 45 | `updatePlaylist` | full | playlists.ts | `name`, `public`, `songIdToAdd…` (appended in order), `songIndexToRemove…`. Indexes count over the songs the client was shown, which leaves out missing tracks (D-065). Owner only (50); `comment` is ignored, as upstream |
| 46 | `deletePlaylist` | full | playlists.ts | owner only (50); 70 for an unknown id |

### 4.7 Starring and ratings

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 47 | `star` | full | starred.ts | `id…` (songs; an album or artist id also works), `albumId…`, `artistId…`; stored in `stars` (§7.3). Unknown ids are skipped; ok when at least one id was valid or none were sent (upstream silently ignores album and artist ids) |
| 48 | `unstar` | full | starred.ts | the same parameters; removes them; always ok |
| 49 | `getStarred` | full | starred.ts | `starred` with `artist`, `album` (Child shape) and `song` lists, each with `starred` time, newest first |
| 50 | `getStarred2` | full | starred.ts | `starred2` with ID3 `artist`, `album`, `song` |
| 51 | `setRating` | full | starred.ts | `id`, `rating` 0..5 (0 clears) for a song, album or artist, stored in `ratings` (§7.3) and shown as `userRating`. Upstream maps 1..5 to "liked" and 0 to unliked; kima-rs keeps ratings and stars apart. 70 for an unknown id, 0 for a rating outside 0..5 (as upstream) |

### 4.8 Users

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 52 | `getUser` | full | profile.ts | the caller; another `username` needs admin (50); roles as upstream `mapSubsonicUser` (`adminRole` from the role, `stream/download/playlist/settings` true, the rest false, `folder` 1) |
| 53 | `getUsers` | full | userManagement.ts | admin only; ordered by username |
| 54 | `createUser` | full | userManagement.ts | admin only; `username`, `password` (`enc:` hex allowed), `adminRole`; `email` and the other role flags are ignored; same validation as `POST /api/auth/users`; 0 "Username already exists" |
| 55 | `updateUser` | full | userManagement.ts | admin only; `adminRole`, `password`; refuses removing your own admin role (50), and removing the last admin (50, D-037) |
| 56 | `deleteUser` | full | userManagement.ts | admin only; not yourself (50), not the last admin (50) |
| 57 | `changePassword` | full | userManagement.ts | yourself, or anyone as admin. A self-change made with an app token is refused (50 "Change your password on the web, not with an app key."), as upstream. Bumps `token_version` and calls `kima_api::revoke_sessions` |

### 4.9 Later-milestone stubs

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 58 | `getAlbumInfo` | stub | index.ts | `albumInfo` empty (upstream too) |
| 59 | `getAlbumInfo2` | stub | index.ts | `albumInfo` empty (upstream too) |
| 60 | `getArtistInfo` | stub | artistInfo.ts | `artistInfo` empty for a known artist until M3; 70 for an unknown one; 10 when `id` is missing |
| 61 | `getArtistInfo2` | stub | artistInfo.ts | `artistInfo2`, the same |
| 62 | `getLyrics` | stub | playback.ts | `lyrics` empty until M3 |
| 63 | `getLyricsBySongId` | stub | lyrics.ts | `lyricsList` empty until M3; 10 when `id` is missing |
| 64 | `getPodcasts` | stub | podcasts.ts | `podcasts` with no channels until M6 |
| 65 | `getNewestPodcasts` | stub | podcasts.ts | `newestPodcasts` empty until M6 |
| 66 | `getPodcastEpisode` | stub | podcasts.ts | 70 "Podcast episode not found" until M6 |
| 67 | `refreshPodcasts` | stub | podcasts.ts | ok: there are no subscriptions to refresh |
| 68 | `getShares` | stub | compat.ts | `shares` empty until M8 (upstream lists real share links) |
| 69 | `createShare` | stub | compat.ts | 0 "Sharing is not supported by this server yet" until M8 |
| 70 | `updateShare` | stub | compat.ts | the same |
| 71 | `deleteShare` | stub | compat.ts | the same |

### 4.10 Compat stubs (upstream has none of these either)

| # | Endpoint | Status | Upstream file | Answer / notes |
|---|---|---|---|---|
| 72 | `getInternetRadioStations` | stub | compat.ts | `internetRadioStations` empty |
| 73 | `createInternetRadioStation` | stub | compat.ts | 0 "Internet radio is not supported by this server" |
| 74 | `updateInternetRadioStation` | stub | compat.ts | the same |
| 75 | `deleteInternetRadioStation` | stub | compat.ts | the same |
| 76 | `getChatMessages` | stub | compat.ts | `chatMessages` empty |
| 77 | `addChatMessage` | stub | compat.ts | 0 "Chat is not supported by this server" |
| 78 | `getVideos` | stub | compat.ts | `videos` empty |
| 79 | `getVideoInfo` | stub | compat.ts | `videoInfo` empty; 10 when `id` is missing |
| 80 | `getCaptions` | stub | compat.ts | 70 "No captions available"; 10 when `id` is missing |
| 81 | `jukeboxControl` | stub | compat.ts | 0 "Jukebox mode is not supported by this server"; 10 when `action` is missing |

Totals: 81 paths (upstream registers the same 81), of which **44 full, 8 partial, 29 stub**.

Any other path under `/rest/` answers code 0 "Not implemented: <path>", after auth, as upstream does.

## 5. Auth and token storage

### 5.1 App tokens

An app token is a random secret a user creates for one client. It is the only credential that works with
token+salt auth, and it is what the settings page tells people to use.

- **Format:** 32 random bytes from the OS RNG, as 64 lowercase hex characters, as upstream. Hex survives every
  client's password field, the `enc:` hex encoding, and copy and paste.
- **Shown once:** the plaintext is returned once, in the `POST /api/api-keys` response, and never again.
- **Limits:**
  - A name is 1–100 characters after trimming.
  - A user can hold at most 50 tokens; the 51st is refused with 400 "Too many app tokens; revoke one first".
- **Revocation:** deleting the row revokes the token. Nothing caches token validity, so the next request with it
  fails.
- **Password changes:** a token survives the owner's password change, as upstream, because each token is revoked
  on its own. Deleting the user deletes their tokens (cascade).
- **Scope:** in M2 an app token authenticates `/rest` only. `kima-api` does not accept it. M8 decides whether
  `X-API-Key` on `/api` accepts the same tokens, as upstream does (D-073).

### 5.2 What the server stores, and how it is protected

Token+salt auth works like this. The client sends `s` (a random salt it chose) and `t = md5(secret + s)` as
lowercase hex. To check `t`, the server must compute `md5(secret + s)` itself, so it needs the secret in a form
it can recover. A one-way hash is not enough. This is why upstream, which stores only SHA-256 digests, refuses
`t`/`s`.

kima-rs therefore stores two things per token, in table `api_keys` (§7.1):

1. **`key_hash`**: the SHA-256 of the token, unique and indexed. It answers `apiKey=` and `p=` lookups in one
   index probe, as upstream, without decrypting anything.
2. **`secret_enc`**: the token encrypted with XChaCha20-Poly1305. This is what token+salt auth needs. The value is
   `0x01 ‖ nonce (24 bytes) ‖ ciphertext ‖ tag (16 bytes)`; the first byte is the format version.
   - **Key:** derived from `SETTINGS_ENCRYPTION_KEY` (env, or `/data/secrets.json` generated on first start;
     M0) by HKDF-SHA256 with info `"kima-rs cipher v1"`. This is the settings cipher the parent spec §5 describes.
     The roadmap planned it for M3 (the AcoustID key); an app token is now the first stored credential, so the
     cipher moves forward to M2 as `kima_core::cipher::Cipher`, and M3 reuses it (D-072).
   - **Associated data:** `"api_keys:" ‖ row id`, which binds a ciphertext to its row. Copying one row's
     `secret_enc` into another row does not decrypt.
   - **Nonces:** a fresh random 24-byte nonce for every encryption.

What this protects against:
- **A leaked database dump or backup alone** reveals no token. `key_hash` cannot be reversed (256-bit random
  input), and `secret_enc` needs the key in `/data/secrets.json` or the environment.
- **An attacker with both the database and the key** can recover the tokens, and so can call the Subsonic API
  as those users until the tokens are revoked. That is the price of supporting token+salt, and it stays within
  `/rest`, because tokens do not work on `/api` (§5.1).
- **A changed or lost `SETTINGS_ENCRYPTION_KEY`** makes `secret_enc` unreadable. Token+salt then fails with 40 for
  those tokens, while `apiKey=` and `p=` keep working through `key_hash`. The server logs one warning per token
  that fails to decrypt (at most once an hour per token), naming the token id and not its value.
- **The account password** is never usable with token+salt. It is stored as argon2id (D-005) and cannot be
  recovered.

### 5.3 How a request is authenticated

The `SubsonicUser` extractor runs before every handler except `tokenInfo` and `getOpenSubsonicExtensions`. It
reads the merged parameters (§4) and tries these, in order:

1. **`apiKey=` (OpenSubsonic `apiKeyAuthentication`).**
   - With `p`, `t` or `s` also present: 43 "Multiple conflicting authentication mechanisms provided".
   - Otherwise it looks up `key_hash = sha256(apiKey)`. Not found: 44 "Invalid API key".
   - `u` is optional. When present, it must name the token's owner, case-insensitively, else 40. OpenSubsonic
     says `u` must not be sent with `apiKey`, and upstream requires it. kima-rs accepts both (D-074).
2. **`u` is required from here on.** Missing: 10 "Required parameter is missing: u".
3. **`t=` and `s=` (token+salt).**
   - `s` missing or empty: 10. `t` that is not 32 hex characters: 40.
   - Load the user's tokens (at most 50; §5.1) and decrypt each `secret_enc`.
   - Accept when `md5(secret + s)` equals `t`, compared in constant time and case-insensitively.
   - No match: 40 "Wrong username or password. Token authentication needs an app token from Settings > Native
     Apps; your account password cannot be used with it."
   - Every check does the same work whatever the user has: 50 decrypt-and-md5 rounds, real tokens first, then
     dummy ciphertexts. An unknown user gets 50 dummy rounds. This keeps the timing from revealing which usernames
     exist or how many tokens they hold, up to the one user lookup query that every path makes.
4. **`p=` (plain or `enc:<hex>`).**
   - Decode `enc:` as upstream `decodeSubsonicPassword`.
   - First try it as an app token: `key_hash = sha256(value)` belonging to user `u`. A token that belongs to
     someone else falls through, as upstream.
   - Then try the account password: argon2id verify through `AppState::password_permit`, with
     `verify_dummy` for an unknown user, as `POST /api/auth/login` does.
   - Failure: 40 "Wrong username or password".
5. **Nothing usable:** 10 "Required parameter is missing: p, t or apiKey".

Result: `SubsonicUser { user: AuthUser, method: Password | AppToken(token_id) }`. `changePassword` reads `method`
(§4.8), and `SubsonicUser::require_admin()` answers 50 "Admin privileges required".

A token match records `last_used_at = now()`, at most once a minute per token (an in-memory map of token id →
last write), as upstream `touchApiKey`.

**Password check cache.** Subsonic clients send their credentials with every request, and Symfonium can make
thousands of requests while it syncs. One argon2id verify per request would tie up the CPU-bounded
`password_work` semaphore and slow down web logins.

- **What is cached:** after a successful password verify, the extractor keeps one entry per user:
  `(HMAC-SHA256(process key, password), token_version, expires_at = now + 10 min)`. The process key is 32 random
  bytes made at start.
- **Check:** a later `p=` for that user is accepted without argon2 when its HMAC matches, the user's current
  `token_version` (read from the users row the extractor loads anyway) equals the cached one, and the entry has
  not expired.
- **Invalidation:** a password change bumps `token_version`, so it invalidates the entry at once. The map holds at
  most one entry per user; it never holds the password.

### 5.4 Error codes

From the parent spec §10 and the OpenSubsonic error list; `kima-subsonic` maps every failure to one of these:

| Code | Meaning | When kima-rs uses it |
|---|---|---|
| 0 | Generic error | internal errors ("Internal error"; details only in the log); unsupported writes; unknown endpoint |
| 10 | Required parameter missing | missing or unparsable required parameter (upstream also uses 10 for invalid values such as `position`) |
| 20 / 30 | Client / server too old | never (the `v` parameter is not enforced, as upstream) |
| 40 | Wrong username or password | every failed `u`+`p`/`t` check, token+salt with no matching token, `apiKey` with a mismatched `u` |
| 41 | Token auth not supported | never: kima-rs supports token+salt (upstream always answers 41) |
| 42 | Auth mechanism not supported | never |
| 43 | Conflicting auth mechanisms | `apiKey` together with `p`, `t` or `s` |
| 44 | Invalid API key | `apiKey` not found (also `tokenInfo`) |
| 50 | Not authorized | admin-only endpoints, others' playlists for writes, self-change of password with a token, last-admin and self-demotion rules |
| 70 | Not found | unknown or non-UUID id, missing track, no cover art, another user's private playlist |

Domain errors are mapped in one place (`kima_subsonic::error::SubsonicError`, with `From<sqlx::Error>` and
`From<kima_api::error::ApiError>` impls), so handlers use `?`.

### 5.5 What clients need, checked

- **DSub** uses token+salt for servers reporting API 1.13 or newer, and calls `getIndexes`, `getMusicDirectory`,
  `getAlbumList`, `stream` with `maxBitRate`, and `scrobble`. With the app token as its password, its md5 matches
  §5.3 step 3.
- **Symfonium** uses `apiKey` when the server advertises `apiKeyAuthentication`, otherwise token+salt. It syncs
  through `search3` with an empty query and big counts, then `getAlbum` for each album. It uses `getPlayQueue`,
  `star` and `setRating`.
- **Feishin** uses token+salt, `getAlbumList2`, `search3`, `getPlaylists`/`getPlaylist`, `savePlayQueue`,
  `scrobble` with `submission=false` then `true`, and `getStarred2`.
- **Amperfy** sends a plain `p=` (or `enc:`), which hits step 4's token check first.

The integration test (§10.3) exercises each of these login paths.

### 5.6 Credentials and logging

Every Subsonic credential travels in the query string. `kima-server`'s `TraceLayer` gets a custom span maker that
records `method` and `path` only for requests under `/rest`, never the query string. The same applies to any
`tracing` field `kima-subsonic` writes. A unit test checks that a request with `p=`, `t=`, `s=` and `apiKey=`
produces no log line containing those values.

## 6. Behaviour details

### 6.1 Streaming quality

`stream` and `getTranscodeStream` choose the M1 `Quality` like this (`kima_subsonic::media::quality`):
1. `format=raw`: original.
2. `maxBitRate` missing or 0, or at least the source bitrate (`tracks.bitrate`; unknown counts as above 320):
   original, unless `format` names a codec other than the source's (then the next rule, with 320 as the cap).
3. Otherwise the highest of 320, 192 and 128 kbps that is not above `maxBitRate`, and 128 below that.
Transcodes are mp3 (M1). Upstream maps `maxBitRate` ≥ 192 to "high" (320 kbps), which can exceed what the
client asked for (D-085).

### 6.2 Song, album and artist fields

One mapper per shape lives in `kima_subsonic::map`, and every endpoint uses it:
- **Song (Child):**
  - `id`, `parent` (album id), `title`, `album`, `isDir=false`, `coverArt` (album id), `albumId`.
  - `artist`: the track's own artist (`tracks.artist_name`). Upstream uses the album artist, which mislabels
    compilation tracks. `artistId` is the album artist's id, since kima-rs has rows only for album artists.
  - `duration` (seconds), `bitRate` (`tracks.bitrate`, falling back to upstream's estimate by codec), `track`,
    `discNumber`, `year` (the track's year, else the album's), `size`.
  - `contentType` (from the file extension, the same table as the `/api` stream), `suffix` (the extension).
  - `type="music"`, `created` (`tracks.created_at`), `starred` (when the caller starred it), `userRating` (when
    the caller rated it).
  - No `path` and no `genre`.
- **Album (AlbumID3):** `id name artist artistId coverArt songCount duration year created starred? userRating?`.
  In Child shape it adds `isDir=true title parent`.
- **Artist (ArtistID3):** `id name albumCount coverArt="ar-<id>" starred? userRating?`. There is no
  `artistImageUrl`.
- **Missing tracks** are left out of every list, and albums or artists with no present track disappear, as in
  M1 (D-057, D-065).

`starred` and `userRating` come from joins against the caller's rows in `stars` and `ratings` (§7.3). The
queries take the user id, so one round trip serves a page.

### 6.3 Scrobble and now playing

- **`submission=true`:**
  - For each `id`, with its `time` when present (epoch ms; a time more than 60 s in the future is replaced by
    now), call `kima_db::plays::record` with `Listen::Attested` and source `Subsonic`.
  - `Listen::Attested` keeps the 30-second floor (`TooShort`) and the duplicate window, and skips the
    seconds-heard check, as upstream `clientAttestedComplete`.
  - Unknown, non-UUID or missing tracks are skipped. The answer is always ok, as upstream: a skipped play is not
    an error to the client.
- **`submission=false`:** the server records "now playing" in memory: `(user, player) → (track, started_at)`.
  - `player` is the `c` parameter (max 64 characters, default "Subsonic"), and `playerId` is a stable small hash
    of it.
  - An entry expires after the track's duration plus 60 s, or when the same user and player scrobble a
    submission.
  - It feeds `getNowPlaying` (§4.3). Nothing is written to the database. Upstream only feeds its public "live
    trip" here, which kima-rs does not have (D-062).

### 6.4 Play queue

Subsonic and the web player share the user's single `playback_state` row, as upstream. So a queue saved from
Symfonium shows up in the web player, and the other way round. The legacy `SubsonicPlayQueue` table upstream
still has, but no longer uses, is not ported.

- **Save:** writes `playback_type='track'`, `track_id` = the current id, `queue` = the ids hydrated into the
  web-player item shape upstream's `hydrateQueue` writes (`{id, title, duration, artist:{id,name},
  album:{id,title,coverArt}}`, with `coverArt` = the album id per D-049), `current_index`, `position` (ms), and
  `changed_by` = `c` (max 64, default "Subsonic").
  - Unknown ids are dropped before indexing.
  - `kima_db::playback::Patch` gains `position` and `changed_by`.
- **Read:** rebuilds each entry from the current database rows by the stored ids, through the song mapper. Gone
  tracks drop out and changed titles show their new value. Upstream echoes the stored JSON (D-088).
- **No row:** answers an empty `playQueue`/`playQueueByIndex` with `changed` = now and `changedBy` "Kima", as
  upstream.

### 6.5 Ids

- **Format:** every id is the M1 UUID as a string. UUIDv7 values do not collide across tables, so an endpoint
  that accepts "an album or an artist id" tries each table in turn.
- **Prefixes:** prefixed forms (`ar-`, `al-`, `tr-`, `artist:`, `album:`) are accepted where upstream accepts
  them.
- **Bad ids:** an id that is not a UUID is 70, as in M1 (D-050).
- **Music folder:** the only music folder id is `1`. `musicFolderId` parameters are accepted and ignored.

## 7. Data model (migrations 0009–0012)

### 7.1 `0009_api_keys.sql`

```sql
CREATE TABLE api_keys (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    key_hash     bytea NOT NULL UNIQUE CHECK (length(key_hash) = 32),   -- sha256(token)
    secret_enc   bytea NOT NULL,                                       -- §5.2
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz                                           -- null until first use
);
CREATE INDEX api_keys_user_idx ON api_keys (user_id);
```

The table keeps upstream's name (`ApiKey`) because M8's `X-API-Key` and device linking extend it rather than
adding a second credential table. Upstream's `lastUsed` defaults to the creation time; kima-rs leaves it null
until first use, and the settings page already handles null.

### 7.2 `0010_plays_source.sql`

```sql
ALTER TABLE plays ADD COLUMN source text NOT NULL DEFAULT 'LIBRARY'
    CHECK (source IN ('LIBRARY', 'SUBSONIC'));
```

This mirrors upstream's `ListenSource` (migration `add_subsonic_listen_source`), limited to the two values kima-rs
can produce. `GET /api/plays` returns the stored value instead of the constant `"LIBRARY"`, which resolves that
part of D-057. `kima_db::plays::record` takes a `source` and a `Listen` (`Seconds(f64)` or `Attested`).

### 7.3 `0011_subsonic_state.sql`

```sql
CREATE TABLE subsonic_bookmarks (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id    uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    position_ms bigint NOT NULL CHECK (position_ms >= 0),
    comment     text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    changed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, track_id)
);

-- One row per starred item. Exactly one target column is set; purging a track, album or
-- artist removes its stars.
CREATE TABLE stars (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id   uuid REFERENCES tracks (id) ON DELETE CASCADE,
    album_id   uuid REFERENCES albums (id) ON DELETE CASCADE,
    artist_id  uuid REFERENCES artists (id) ON DELETE CASCADE,
    starred_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(track_id, album_id, artist_id) = 1)
);
CREATE UNIQUE INDEX stars_track_key  ON stars (user_id, track_id)  WHERE track_id  IS NOT NULL;
CREATE UNIQUE INDEX stars_album_key  ON stars (user_id, album_id)  WHERE album_id  IS NOT NULL;
CREATE UNIQUE INDEX stars_artist_key ON stars (user_id, artist_id) WHERE artist_id IS NOT NULL;

-- Same shape for ratings 1..5 (0 deletes the row).
CREATE TABLE ratings (
    id        uuid PRIMARY KEY,
    user_id   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id  uuid REFERENCES tracks (id) ON DELETE CASCADE,
    album_id  uuid REFERENCES albums (id) ON DELETE CASCADE,
    artist_id uuid REFERENCES artists (id) ON DELETE CASCADE,
    rating    smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    rated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(track_id, album_id, artist_id) = 1)
);
-- plus the three partial unique indexes, as for stars
```

Bookmarks follow upstream's `SubsonicBookmark`, with the position renamed to say its unit.

**Starring and D-071.** Upstream has a `LikedTrack` table. Only Subsonic `star`/`setRating` write it, and the
web app never shows it. D-071 ("upstream has no library-track likes") is right about the web app but misses this
table, so its text is amended. kima-rs keeps the web app free of likes, as D-071 decided. Stars exist for
Subsonic clients only:
- They cover songs, albums and artists, because clients star all three and upstream silently drops the album and
  artist ones.
- They are returned as `starred` on every item, so every app shows the same heart.

The web frontend does not read them in M2. A later milestone can, without a schema change.

**Missing tracks.** A track flagged missing keeps its stars, ratings and bookmarks (they are hidden from lists),
and loses them only when the purge after the grace period deletes the track row (cascade). Albums and artists
behave the same way.

### 7.4 `0012_sort_names.sql`

```sql
ALTER TABLE artists ADD COLUMN sort_name text GENERATED ALWAYS AS (
    lower(regexp_replace(name, '^(the|a|an|le|la|les|los|las|die|der|das)\s+', '', 'i'))) STORED;
ALTER TABLE albums ADD COLUMN sort_name text GENERATED ALWAYS AS (
    lower(regexp_replace(title, '^(the|a|an|le|la|les|los|las|die|der|das)\s+', '', 'i'))) STORED;
CREATE INDEX artists_sort_name_idx ON artists (sort_name);
CREATE INDEX albums_sort_name_idx ON albums (sort_name);
```

The article list is upstream's `LEADING_ARTICLES` (`services/pgTextRules.ts`), and `ignoredArticles` advertises
exactly this list. Subsonic orders and buckets by `sort_name`, so "The Beatles" files under B (the upstream test
in §10.2). The web browse API keeps its M1 `lower(name)` ordering (D-050). Moving it to `sort_name` too is a
one-line change per query, left for when the frontend wants it.

### 7.5 New `kima-db` functions

- `api_keys`: `create`, `list_for_user`, `delete_owned`, `find_by_hash`, `secrets_for_user`, `touch`.
- `stars` and `ratings`: `set`, `clear`, and `list_for_user(kind)`. Starred and rating state is joined inside the
  browse queries below, not fetched per item.
- `bookmarks`: `upsert`, `delete`, `list_for_user`.
- `subsonic` (read models for Subsonic), all user-aware for `starred`/`userRating`:
  - `artists_sorted`, `artist_with_albums`, `album_with_tracks`, `track`;
  - `album_list(kind, size, offset)`, `random_tracks(size, years)`, `top_tracks(artist, count)`;
  - `tracks_by_ids(ids)` (keeps the input order), `search_all(page)` (the empty query);
  - `index_last_modified`.
- `search::{artists, albums, tracks}` gain an `offset`.
- `playlists::replace_items` (for `createPlaylist` with `playlistId`), and `playlists::remove_at_indexes`, which
  works under the playlist row lock, over present items in order.
- `plays::record` changes as in §7.2. `plays::count_present_tracks` serves `getScanStatus`.
- `playback::Patch` gains `position` and `changed_by` (§6.4).

## 8. Crate layout and shared code

### 8.1 `crates/kima-subsonic`

```
kima-subsonic/src/
  lib.rs        router(ctx) -> Router, mounted by kima-server at /rest
  ctx.rs        SubsonicCtx { app: AppState, now_playing, password_cache, touch }, FromRef<AppState>
  params.rs     Params: query + form body as a multimap; get/required/all/int/bool helpers
  auth.rs       SubsonicUser extractor (§5.3), require_admin
  response.rs   Node tree, envelope, XML and JSON writers (§8.3)
  error.rs      SubsonicError { code, message }, From impls (§5.4)
  map.rs        song / album / artist / child / playlist / user mappers (§6.2)
  media.rs      quality choice (§6.1), stream/download/cover/hls handlers over kima_api::media
  routes/       system, browse, lists, search, media, scrobble, queue, bookmarks, playlists,
                stars, users, stubs
```

Dependencies: `kima-core`, `kima-db`, `kima-library` and `kima-api`, for `AppState`, `AuthUser`,
`revoke_sessions` and the new `kima_api::media` module. `kima-api` does not depend on `kima-subsonic`.
`kima-server` mounts both:

```rust
kima_api::router(state.clone()).nest("/rest", kima_subsonic::router(state))
```

They are mounted before the frontend's SPA fallback, so `/rest/*` never falls through to `index.html`.

`/rest` responses always carry `Access-Control-Allow-Origin: *`, whatever `ALLOWED_ORIGINS` says (D-090).
Browser-based Subsonic players (Feishin web, Airsonic Refix) run on other origins and send credentials in the
URL, not in cookies.

### 8.2 Shared media code (`kima_api::media`)

The M1 stream handler (`routes/library/stream.rs`) and cover handler (`routes/cover_art.rs`) are split. The
serving core becomes a public module that both APIs call:

- `media::resolve_track(state, id) -> Result<ResolvedTrack, MediaError>`: looks up the path, refuses missing
  tracks, and applies the path rules (§9.3).
- `media::serve_track(state, &ResolvedTrack, quality, &HeaderMap) -> Result<Response, MediaError>`: picks the
  transcoder, serves the file or a growing transcode, and handles ranges and conditional headers (§9.3). The
  retry loop moves here unchanged.
- `media::cover(state, album_id, size) -> Result<Option<CoverImage>, MediaError>`.
- `MediaError` is `NotFound | BadRequest(String) | Internal(anyhow::Error)`. `/api` maps it to `ApiError` and
  `/rest` maps it to codes 70, 10 and 0.

The `/api` routes become thin wrappers. Their behaviour does not change, apart from §9.3, and the M1 HTTP tests
for streaming and cover art must pass unchanged.

### 8.3 Response format

Handlers build a format-neutral tree, and two writers serialize it:

```rust
pub struct Node {
    name: &'static str,
    attrs: Vec<(&'static str, Value)>,   // Value = Str | Int | Float | Bool
    text: Option<String>,
    children: Vec<Child>,
}
pub enum Child { One(Node), List(&'static str, Vec<Node>) }
```

**Envelope:**
- Root `subsonic-response`, attributes in this order: `xmlns="http://subsonic.org/restapi"`, `status`
  (`ok`/`failed`), `version="1.16.1"`, `type="Kima"`, `serverVersion` (the kima-rs crate version),
  `openSubsonic=true`.
- An error adds `error code=… message=…`.
- The HTTP status is always 200 for API answers and errors, as the Subsonic protocol and upstream do. That
  includes errors from binary endpoints (`stream`, `download`, `getCoverArt`, `getAvatar`), which answer with the
  envelope instead of the media.

**XML** (default, and `f=xml`):
- `Content-Type: text/xml; charset=utf-8`, with a leading `<?xml version="1.0" encoding="UTF-8"?>`.
- Attributes are escaped, booleans written `true`/`false`, `text` becomes element content, and each node of a
  `List` becomes one element with the list's name.
- An empty list writes nothing.
- Written with `quick-xml`.

**JSON** (`f=json`; `f=jsonp` is treated as `json` without a callback, as upstream):
- `Content-Type: application/json`, shaped `{"subsonic-response": {...}}`.
- Attributes become keys with typed values (numbers stay numbers, booleans stay booleans), and `text` becomes
  `"value"`, as upstream's `stripAttrPrefix`.
- A `One` child becomes an object and a `List` always becomes an array, including an empty one (`"song": []`).
  Upstream leaves out empty lists. Clients that do `response.song.map(...)` break on a missing key and not on
  `[]` (D-084).

**Invariants:** a node never has two children with the same name, and an element name is either always a `List`
or always `One` within its parent type. Both are checked in debug builds and by the structural test in §10.1.

## 9. Carried forward and folded in

### 9.1 Safety-net library sync (scheduler in `kima-jobs`)

The watcher stays primary (M1 §4.4). M1 added reconciles on startup, on watcher overflow and on demand; M2 adds
one on a schedule, to catch changes the watcher could not see. Typical cases are a network share where native
events never arrive, or edits made while kima was stopped and then left for weeks.

- **`kima_jobs::schedule`:**
  - API: `Scheduler::new().add(name, Cron, enqueue_fn)` and `spawn(pool, shutdown) -> JoinHandle`.
  - Each entry sleeps until its next fire time and calls its enqueue function. A schedule entry only ever
    enqueues a job, as parent spec §4 says. Errors are logged and the loop goes on.
  - Missed fires (the process was down) are not caught up: the startup reconcile already covers that.
  - It stops on the shutdown token.
- **Cron parsing:** the `croner` crate, standard 5-field expressions, evaluated in UTC.
- **Config:** `KIMA_LIBRARY_SYNC_CRON`, default `0 4 * * *` (04:00 UTC daily). `off` disables it. An invalid
  expression fails startup with a clear message.
- **What it queues:** `kima_library::jobs::enqueue_reconcile`, the incremental reconcile M1 already has, with its
  dedupe against queued or running reconciles. Unchanged files cost one `stat` each.
- **Status:** `GET /api/library/watch-status` gains `nextSyncAt` (ISO 8601, or null when off), and the settings
  page shows it next to the watch mode.
- **Why daily:** the reconcile is incremental (one stat per file) and dedupes against itself. Once a day at a
  quiet hour bounds how long a lost change can stay unseen, and costs nothing noticeable even for a 100k-file
  library on a network share. It does not reverse the owner's M1 decision against scheduled full rescans: the
  watcher is still what keeps the library current, and this is a cheap backstop the owner can turn off (D-091).

### 9.2 `AdminUser` extractor

- **Type:** `kima_api::auth::AdminUser(pub AuthUser)` implements `FromRequestParts<AppState>`. It first
  extracts `AuthUser`: no auth is 401 "Not authenticated", as now. It then requires `role == "admin"`, else 403
  `{"error":"Admin access required"}`.
- **Where the role comes from:** `AuthUser` already comes from the database row on every request, so the role is
  current (the M1 review checked this).
- **The copies it replaces:**
  - `require_admin` in `routes/auth.rs` and `routes/system_settings.rs`;
  - the inline checks in `routes/onboarding.rs`, `routes/library/scan.rs`, `routes/library/attention.rs` and
    `routes/library_watch.rs`;
  - `admin()` in `routes/library/organizer.rs`.
- **Behaviour:** responses do not change. The existing 403 tests are the check.
- **Subsonic:** `kima-subsonic` has its own single `SubsonicUser::require_admin()`, because its rejection is a
  Subsonic envelope, not JSON.

### 9.3 M1 review minors

The M1 final review left minors open. M2 takes the ones Subsonic would otherwise inherit, and parks the rest.

Folded into M2:
- **Minor 2, symlinks.**
  - `scan::update_path` uses `symlink_metadata` and treats a symlink like a non-audio file: it is never indexed,
    and an existing track at that path is marked missing.
  - `watch::expand` does not descend into a symlinked folder.
  - `media::resolve_track` refuses a path whose final component is a symlink: it opens with `O_NOFOLLOW` and
    answers 70 / 404.
  - It also refuses a source whose canonical path is not under the canonical library root.
  - The watcher, the reconcile and both stream APIs then agree that symlinks are not part of the library (D-093).
- **Minor 10, cover-art cache.**
  - Each `<data>/artwork/<album>/` entry gets a `source.json`: the chosen source's path relative to the library
    root, its size and mtime (ns), and the mtime of the album's folder when the entry was made.
  - On a hit, `media::cover` stats the source and the folder. If either changed, the entry is deleted and rebuilt.
    This catches re-embedded art (the track's mtime changes) and a new or renamed `cover.*` (the folder's mtime
    changes).
  - New `KIMA_ARTWORK_CACHE_MB` (default 512) caps the cache. After each write, the oldest album folders are
    evicted until it fits, by `source.json` mtime, which a hit refreshes.
  - The library purge deletes an album's artwork folder with the album row (amends D-043; D-094).
- **ETag on streams.**
  - Original files and finished transcodes send `ETag` and `Last-Modified`. The ETag is `"<size hex>-<mtime ns
    hex>"` of the served file; a cached transcode adds its quality.
  - `If-None-Match` with a matching ETag answers 304.
  - `If-Range` honours the range only when it matches the ETag or the `Last-Modified` date, and otherwise sends
    the whole file with 200.
  - A growing transcode sends neither header.
  - The same logic serves `/api` and `/rest`. Symfonium and DSub cache by it (D-092).
- **Minor 9, ffmpeg in CI.** `require_ffmpeg()` panics when `CI` is set and ffmpeg is missing, instead of
  returning early. M2 adds many ffmpeg-backed tests (stream with `maxBitRate`), so a silent skip would hide real
  failures.

Parked, with where they go:
- Minor 3 (`listen_is_retried_after_failure` passes for the wrong reason, and claiming after a reconnect) and
  Minor 4 (the watcher test timing ratio): job and watcher test hygiene, unrelated to Subsonic. They go to M3,
  which adds job lease renewal.
- Minors 5, 6 and 7 (copy-mode duplicate planning, sidecar undo content check, hardlink `waiting` set) and the
  resume-fsync nit: organizer internals. They go to M3, whose autotagger reworks the organizer's apply path.
- Minor 8, first half (organizer preview grouped by album): frontend polish. It goes to M3 with the autotag
  review page.

### 9.4 Casting (M11) and the public URL

M11 (Chromecast/AirPlay, after M9) needs short-lived stream tokens and a LAN-reachable `KIMA_PUBLIC_URL`. It does
not change any Subsonic decision: Subsonic clients fetch their own streams and must carry their credentials in the
URL. App tokens are scoped to `/rest`, so the web app's casting never hands one out.

There is one small overlap. M2 needs a server URL for the settings page (upstream
`GET /api/system-settings/public-config`) and for the absolute URL in the `hls` playlist. M2 reads it from
optional env `KIMA_PUBLIC_URL`, the name M11 will use, instead of adding upstream's admin-edited `publicUrl`
setting (D-096).
- `public-config` answers `{"publicUrl": "<KIMA_PUBLIC_URL or empty>"}` to any signed-in user.
- `hls` uses `KIMA_PUBLIC_URL` when it is set, else the request's scheme and `Host`, as upstream.

## 10. Testing

### 10.1 Snapshot tests (`crates/kima-subsonic/tests/snapshots.rs`)

- **Fixture:** a `#[sqlx::test(fixtures("subsonic"))]` SQL fixture with fixed UUIDs and fixed timestamps
  (2026-01-01T00:00:00Z). It holds:
  - 4 album artists, including "The Beatles", "La Roux" and "Various Artists";
  - 6 albums: one multi-disc, one compilation, one without a year;
  - 20 tracks, one of them flagged missing;
  - 2 users (admin and user), one app token;
  - plays, a public and a private playlist, stars, ratings, a bookmark and a saved queue.

  The fixture is seeded by SQL and not by the scanner, so snapshots do not move when scanner heuristics change.
  Streaming and cover art use small ffmpeg-generated files under a temp library root.
- **Coverage:** every endpoint in §4 is snapshotted in both formats (`<endpoint>__<case>.xml.snap` and
  `.json.snap`). Some get several cases: `getAlbumList2` once per `type`, `search3` with a query, an empty query
  and paging, and the error cases (missing parameter, unknown id, non-admin). Stubs get one combined snapshot each.
- **Redactions:** `serverVersion`, and the `changed`/`minutesAgo` values that depend on the clock.
- **Structural test:** for every snapshot pair, parsing the XML back into the JSON rules of §8.3 gives the JSON
  snapshot. This proves the writers agree.
- **Count test:** a table-driven test walks all 81 paths, with and without `.view`, and asserts that none answers
  "Not implemented". This replaces upstream's `noDuplicateRoutes.test.ts`, since axum already panics on a
  duplicate route when the router is built.

### 10.2 Upstream `__tests__` ported

| Upstream test | Port |
|---|---|
| `compatAlwaysAnswers.route.test.ts` | Every endpoint answers a failed envelope (HTTP 200, code 0, "Internal error") when the pool is closed under it, and the body contains no SQL text. Generalised from the four compat cases; the `createShare` cases become "createShare answers not supported" (shares are M8) |
| `noDuplicateRoutes.test.ts` | the count test above |
| `scrobble.route.test.ts` | `submission=true&time=1700000000000` writes one `SUBSONIC` play at that time; `submission=false` writes none and shows in `getNowPlaying`; a repeat inside the window writes nothing. Plus: two `id`/`time` pairs in one call write two plays. The public live-trip cases are dropped (D-062) |
| `library.sort.route.test.ts` | "The Beatles" is under `<index name="B">` and "La Roux" under `R`, never T or L; `ignoredArticles` contains every article in §7.4; `getMusicDirectory id=1`, `alphabeticalByArtist`, `alphabeticalByName` and `starred` order by `sort_name` (asserted on the output order, not on the query). The `displayName` case is dropped (kima-rs has no display-name override). `byGenre` answers an empty list |
| `subsonicAuth2fa.test.ts` | An app token as `p=` is accepted; another user's token as `p=` is refused (40); a `p=` that is neither token nor password is refused (40). `startScan` as a non-admin gives 50, as admin it does not. A self `changePassword` with an app token via `apiKey`, via `p=` and via `t`/`s` gives 50, and the password is unchanged; a self change with the password works; an admin with a token changes another user's password. The 2FA cases are noted for M8 |
| `updatePlaylist.route.test.ts` | Removing index 1 of [a,b,c] and adding [d,e] gives [a,c,d,e] with dense, unique `sort`; four concurrent `updatePlaylist` adds of three songs each keep all 13 songs with unique order. Plus: indexes skip a missing track |
| `artistInfo.route.test.ts` | dropped until M3 (similar artists); the stub is snapshotted instead |

New unit tests:
- Params: repeated keys, POST body merge, `.view` stripping, `enc:` decoding.
- Quality choice table (§6.1).
- The cipher: round trip, a wrong AAD fails, a wrong key fails, the version byte.
- t+s verification against known vectors.
- The password cache: hit, expiry, invalidation on a `token_version` change.
- Credentials never appear in the trace output (§5.6).
- The scheduler: next fire time, `off`, and stopping on shutdown.

### 10.3 Integration test: a real t+s handshake (`crates/kima-subsonic/tests/client.rs`)

A small hand-written Subsonic client, kept in the test crate. It uses `reqwest`, `md5` and a random 12-character
salt per request, and talks to a real `kima` router served on an ephemeral port. Postgres is real, and the
library is a fixture library of ffmpeg-generated tones that the real scanner has indexed. The steps:

1. Log in through `/api/auth/login` and create a token with `POST /api/api-keys`.
2. `ping` with `u`+`t`+`s`; then with a wrong `t` (40), with the account password as `t` source (40, with the
   message from §5.3), with `apiKey` alone, with `apiKey`+`p` (43), and with `p=enc:<hex token>`.
3. `getArtists` → `getArtist` → `getAlbum` → `getSong`, in both formats, following ids from the previous answer.
4. `stream` with `Range: bytes=100-199` gives 206 and the right bytes. `If-None-Match` with the ETag gives 304.
   `maxBitRate=128` gives `audio/mpeg`.
5. `getCoverArt size=128` returns a JPEG.
6. `scrobble submission=true`, then `GET /api/plays` shows the play with `source: "SUBSONIC"`.
7. `savePlayQueue` with 3 ids, then `getPlayQueue`, then `GET /api/playback-state`, which shows the same queue.
8. `star albumId=…`, then `getStarred2` lists it; `setRating`, then `userRating` appears on `getAlbum`.
9. `DELETE /api/api-keys/:id`, then `ping` with the same `t`/`s` gives 40.

A third-party client library was considered. The Rust ones (`sunk`, `submarine`) are unmaintained or incomplete,
and a Python one would add a runtime to CI for one test.

### 10.4 Frontend

- Playwright smoke (CI): open Settings > Native Apps, create a token named "e2e", and see it shown once and then
  listed. A `fetch` to `/rest/ping.view?u=…&p=<token>&f=json` from the page gives `status: "ok"`. Revoke the
  token, and the same fetch gives `status: "failed"`.
- `cargo test` covers the `/api/api-keys` handlers: create, list, delete, someone else's id (404), the 51st
  token (400), and an empty name (400 "Device name is required").

## 11. Frontend

The forked frontend already contains upstream's `SubsonicSection.tsx` ("Native Apps") at the top of the settings
page. It renders in M1 but its calls fail. M2 makes its backend real and changes little in the page itself.

Backend for the page (`kima-api`, new `routes/api_keys.rs`, upstream shapes):
- `POST /api/api-keys {deviceName}` → 201 `{apiKey, name, createdAt, message}`; 400 "Device name is required";
  400 for over 100 characters or the 51st token.
- `GET /api/api-keys` → `{apiKeys: [{id, name, createdAt, lastUsed}]}`, newest first; `lastUsed` may be null.
- `DELETE /api/api-keys/:id` → `{message: "API key revoked successfully"}`; 404 "API key not found or already
  deleted" for an unknown id, someone else's, or a non-UUID.
- `GET /api/system-settings/public-config` → `{publicUrl}` (§9.4), for any signed-in user.

Page changes (`frontend/features/settings/components/sections/SubsonicSection.tsx`):
- The setup guide says what kima-rs supports: "Use the token as the password. It works with apps that send
  hashed passwords (token and salt) and with apps that have an API key field."
- The guide, the token box and the token list are shown always, not only after generating.
- A token's row shows "Never used" when `lastUsed` is null.
- No other layout change. The page stays a mirror of upstream's.

Docs: `docs/subsonic.md` is ported from upstream and edited for kima-rs. It covers token+salt support, the
supported list from §4, stubs by milestone, and `KIMA_PUBLIC_URL`. The README lists the new env vars
(`KIMA_LIBRARY_SYNC_CRON`, `KIMA_ARTWORK_CACHE_MB`, `KIMA_PUBLIC_URL`).

## 12. Divergences to log

Each row goes into `docs/DIVERGENCES.md` in the commit that introduces it. Status is `open` unless noted.

| ID | Area | Upstream | kima-rs |
|---|---|---|---|
| D-072 | Subsonic auth | `t`/`s` always refused with 41; tokens stored as SHA-256 only | `t`/`s` accepted for app tokens (§5.3); each token stored as SHA-256 plus an XChaCha20-Poly1305 ciphertext under a key derived from `SETTINGS_ENCRYPTION_KEY`; the account password still cannot be used with `t`/`s`; the settings cipher arrives in M2 instead of M3 |
| D-073 | Subsonic auth | API keys also work as `X-API-Key` on the REST API; no per-user cap; `lastUsed` defaults to creation time | App tokens work on `/rest` only until M8 decides; 50 per user; names 1–100 characters; `lastUsed` null until first use |
| D-074 | Subsonic auth | `apiKey` requires `u`; `apiKey` with `p` is not checked | `apiKey` alone is accepted; with `u` it must match the owner; with `p`/`t`/`s` it is 43 |
| D-075 | Subsonic API | `getOpenSubsonicExtensions` needs auth; advertises `apiKeyAuthentication`, `songLyrics`, `indexBasedQueue`, `getPodcastEpisode`; parameters only in the query string | No auth; advertises `apiKeyAuthentication`, `formPost`, `indexBasedQueue`; POST form bodies accepted |
| D-076 | Subsonic API | `star` stores songs in `LikedTrack`, ignores album and artist ids; `starred` appears only in `getStarred*`; `getAlbumList type=starred` lists albums containing a liked song | `stars` table for songs, albums and artists; `starred` on every item for the caller; `type=starred` lists starred albums. The web app still has no likes (D-071, text amended: upstream's `LikedTrack` exists but only Subsonic writes it) |
| D-077 | Subsonic API | `setRating` 1..5 likes the song, 0 unlikes; songs only | Ratings stored per song, album or artist in `ratings`, shown as `userRating`; `getAlbumList type=highest` orders by them; stars untouched |
| D-078 | Subsonic API | `scrobble` reads one `id`/`time`; `submission=false` feeds the public live trip only | Every `id`/`time` pair recorded; a `time` more than 60 s ahead becomes now; `submission=false` feeds `getNowPlaying` in memory |
| D-079 | Subsonic API | `getNowPlaying` from web `playbackState` only, `playerName` "Kima" | Also from Subsonic now-playing scrobbles, one entry per user and player, named by `c` |
| D-080 | Subsonic API | `search*` with an empty query answers empty; legacy `search` reads `query` only | An empty query lists the whole library, paged; legacy `search` falls back to `any`/`title`/`album`/`artist` |
| D-081 | Subsonic API | Ordering and index buckets by `sortName`, maintained by the app, which follows `displayName` | Generated `sort_name` columns (same article list); Subsonic only, the web browse keeps D-050 |
| D-082 | Subsonic API | Song `artist` is the album artist; `bitRate` estimated from the codec; `contentType` from a codec-name table; no `created` on songs, albums or playlists | Song `artist` is the track artist; `bitRate` from the file when known; `contentType`/`suffix` from the extension; `created` on songs and albums, `created`/`changed` on playlists |
| D-083 | Subsonic API | `getAlbumList` (folder variant) sends the ID3 album shape | Sends the Child shape with `title`, `isDir`, `parent`, plus the ID3 fields |
| D-084 | Subsonic API | JSON omits empty lists | JSON lists are always arrays, `[]` when empty |
| D-085 | Subsonic API | `maxBitRate` ≥ 320 or 0 is original, ≥ 192 is the 320 kbps transcode, ≥ 128 is 192 kbps | The highest of 320/192/128 not above `maxBitRate`; original when `maxBitRate` is at least the source bitrate |
| D-086 | Subsonic API | Genres, lyrics, artist info, similar songs, podcasts and shares backed by real data | Stubs until M3 (genres, lyrics, artist info), M5 (similar songs), M6 (podcasts), M8 (shares), as listed in §4; artist cover art answers 70 until M3 |
| D-087 | Subsonic API | `getScanStatus count` = queued scan jobs | `count` = present tracks; `scanning` from the reconcile job state |
| D-088 | Subsonic API | Play queue entries echo the stored JSON; playlists: another user's private one is 50; `songIndexToRemove` counts all items; `createBookmark` on an unknown track fails generically | Queue entries rebuilt from current rows; another user's private playlist is 70 (as D-063); indexes count visible items (D-065); unknown bookmark track is 70; `getIndexes` honours `ifModifiedSince` |
| D-089 | Subsonic API | `/rest` rate-limited to 1500 requests a minute per IP | No rate limit until M8 (`to-fix`) |
| D-090 | Subsonic API | CORS as for the whole app | `/rest` always sends `Access-Control-Allow-Origin: *` (credentials ride in the URL, never cookies) |
| D-091 | Library | Scheduled rescans | Watcher primary; plus a daily incremental reconcile, `KIMA_LIBRARY_SYNC_CRON` (default `0 4 * * *` UTC, `off` disables); `watch-status` reports `nextSyncAt`. Amends D-044 and D-047 ("never on a schedule") |
| D-092 | Streaming | No `ETag`; ranges always honoured | `ETag`/`Last-Modified` on originals and finished transcodes, `If-None-Match` → 304, `If-Range` honoured; on `/api` and `/rest` |
| D-093 | Library | Follows symlinks when scanning | Symlinked files and folders are never indexed (watcher and reconcile agree); streams refuse a symlinked file or a source outside the canonical root |
| D-094 | Library | Covers are stored under a content-derived name (`coverUrl`), so new art gets a new cache key; M1 cached by album id and never invalidated | Cache entries checked against their source and the album folder, rebuilt when either changes, capped by `KIMA_ARTWORK_CACHE_MB` (default 512), deleted with the album. Amends D-043 |
| D-095 | Playback | Plays have a `ListenSource` with four values | `plays.source` is `LIBRARY` or `SUBSONIC`; `GET /api/plays` returns it (resolves the `source` half of D-057) |
| D-096 | Settings | `publicUrl` is an admin system setting, read by `/system-settings/public-config` | Read from env `KIMA_PUBLIC_URL` (the name M11 uses); no setting |

D-057 gets its `source` clause marked resolved by D-095, and D-024 notes that API keys exist from M2 for Subsonic
only.

## 13. Risks

| Risk | Mitigation |
|---|---|
| Recoverable token secrets widen the blast radius of a full server compromise | Encrypted at rest with a key kept outside the database (§5.2); tokens scoped to `/rest`; one-click revoke; the threat model written down in `docs/subsonic.md` |
| Client quirks the spec does not cover (id prefixes, missing `.view`, `getIndexes` polling, empty-query sync, JSON array expectations) | §5.5 checks named clients against the design; the integration test exercises the same flows; the structural XML/JSON test; prefixes accepted where upstream accepts them |
| The stream refactor (§8.2) breaks the web player | The M1 stream and cover-art HTTP tests must pass unchanged before Subsonic uses the module; the Playwright smoke still plays a track |
| `starred`/`userRating` joins slow big listings (a 10k-song `search3` sync page) | Partial unique indexes on `(user_id, track_id)` etc.; joins scoped to one user; `search_all` pages capped at 500; a timing check in the test suite on a 10k-track generated fixture (not a CI gate) |
| Argon2 per request for clients that send the account password | The 10-minute password cache (§5.3); the settings page points people at tokens |
| Credentials leak into logs or traces | The path-only span for `/rest` and its test (§5.6) |
| Losing `SETTINGS_ENCRYPTION_KEY` silently breaks token+salt clients | `apiKey`/`p=` keep working; a rate-limited warning names the affected token; `docs/subsonic.md` says that rotating the key needs new tokens for t+s apps |
| The scheduled reconcile surprises an owner who wanted none | It is incremental and dedupes; `KIMA_LIBRARY_SYNC_CRON=off`; documented next to the watch modes (D-091) |
| The OpenSubsonic spec moves while M2 is being built | Pinned to API 1.16.1 plus the three advertised extensions; new extensions only once real |

## 14. Design calls

These are calls I made; none is an owner decision. Each says what it costs if it turns out wrong.

1. **Token+salt works for app tokens only, with the secret encrypted at rest (XChaCha20-Poly1305) next to a
   SHA-256 lookup hash.** If wrong (the owner prefers upstream's hash-only storage): drop `secret_enc`, answer 41
   to `t`/`s` again, and tell DSub/Feishin users to switch to plain password mode. That is one migration and the
   auth step 3, about a day.
2. **The settings cipher moves forward from M3 to M2 as `kima_core::cipher`, keyed by HKDF from
   `SETTINGS_ENCRYPTION_KEY`.** If wrong (M3 wants a different construction): re-encrypt `api_keys.secret_enc` in
   one migration-time pass, using the format version byte.
3. **App tokens reuse upstream's `api_keys` table and `/api/api-keys` endpoints, and are Subsonic-only until M8.**
   If wrong (M8 wants separate Subsonic and API credentials): add a `scope` column then, and every existing row
   defaults to `subsonic`. Cheap.
4. **Token format 64 hex characters; at most 50 per user; tokens survive a password change.** If wrong: the cap
   and the format are constants. Making a password change revoke tokens is a one-line `DELETE` in the change
   paths, but it disconnects every app.
5. **A plain `p=` account password is accepted, with a 10-minute in-memory verified-password cache.** If wrong
   (the cache is judged risky): remove it, and pay one argon2 verify per request for password-mode clients, which
   is slow but correct.
6. **`apiKey` without `u` is accepted, `apiKey` with `u` must match, and `apiKey` with `p`/`t`/`s` is 43.** If
   wrong (a client sends both and breaks): relax 43 to "ignore `p`". One branch.
7. **Stars live in a new `stars` table for songs, albums and artists, and show as `starred` everywhere; the web
   app still has no likes.** If wrong (the owner wants web likes too): the table is ready, and the cost is
   frontend work. If the owner wants stars dropped: the endpoints fall back to upstream's song-only behaviour.
8. **`setRating` stores real 1–5 ratings in `ratings` instead of mapping them to stars.** If wrong: map 1–5 to a
   star as upstream, and drop the table (one migration).
9. **The play queue shares the web player's `playback_state` row; upstream's unused `SubsonicPlayQueue` is not
   ported.** If wrong (people want separate queues per app): add a per-client queue table and key it by `c`. A
   medium change.
10. **An empty `search3` query lists everything.** If wrong (some client floods the server): cap pages lower. It is
    paged already.
11. **JSON always emits lists as arrays, `[]` when empty.** If wrong (a client chokes on `[]`, unlikely): one
    writer flag to omit empty lists, and the snapshots are re-accepted.
12. **A song's `artist` is the track artist, while `artistId` is the album artist.** If wrong (a client groups
    badly): switch to the album artist (one mapper line), and compilation tracks show "Various Artists" again.
13. **`sort_name` comes from generated columns, used by Subsonic only.** If wrong (the web app should sort the
    same way): change the browse ORDER BYs. If the article list needs locale rules: replace the expression with an
    immutable SQL function.
14. **`maxBitRate` maps to the highest bucket not above it, and to original when the request is at least the
    source bitrate.** If wrong: a single function (§6.1).
15. **Now playing is kept in memory, fed by `submission=false`, and lost on restart.** If wrong (it should survive
    restarts or several instances): move it to an unlogged table.
16. **Every `id`/`time` pair of a scrobble is recorded.** If wrong: take only the first, as upstream. One loop.
17. **Shares stay stubs until M8, though upstream has them.** If wrong (the owner wants Subsonic shares now): that
    pulls M8's share links forward, a separate feature of a few days.
18. **The safety-net sync runs daily at 04:00 UTC, uses `croner` with a small loop in `kima-jobs`, and can be
    turned off.** The parent spec named `tokio-cron-scheduler`, which brings its own job store we do not need. If
    wrong: change the default expression, or swap the parser crate; the API in §9.1 hides it.
19. **`kima-subsonic` depends on `kima-api` for `AppState`, `AuthUser` and the new `media` module.** If wrong
    (the dependency grows awkward): extract a `kima-http` crate holding the state and media code. Mechanical.
20. **`/rest` sends `Access-Control-Allow-Origin: *` whatever `ALLOWED_ORIGINS` says.** If wrong (an owner wants
    `/rest` locked to origins): apply `ALLOWED_ORIGINS` there too. Browser Subsonic clients then need listing.
21. **`/rest` request logs carry the path only.** If wrong (debugging needs parameters): log the query with the
    credential keys redacted.
22. **M2 folds in M1 minors 2, 9 and 10 plus `ETag`, and parks 3–8 for M3.** If wrong: the parked items are small
    and listed with their target in §9.3.
23. **The integration test uses a hand-written t+s client, not a third-party library.** If wrong (a real
    client's quirk is missed): add a Python `py-opensonic` job to CI later.
24. **The snapshot fixture is seeded by SQL with fixed UUIDs, not by the scanner.** If wrong (the snapshots miss a
    scanner-shaped edge case): the integration test (§10.3) uses the scanner, and more cases go there.
25. **`getOpenSubsonicExtensions` needs no auth, and `formPost` is supported and advertised.** If wrong: put it
    behind auth (one line), or drop the extension (the body merge only adds parameters).
26. **The public URL comes from env `KIMA_PUBLIC_URL`, not an admin setting.** If wrong (owners want it in the
    UI): add a `system_settings.public_url` column that overrides the env. M11 reads the same accessor.
27. **`getCoverArt` for artists answers 70 until M3.** If wrong (a client shows ugly errors): serve the artist's
    newest album cover instead. A few lines.
28. **Legacy `getAlbumList` answers the Child shape.** If wrong: send only the ID3 shape, as upstream (one mapper
    choice).
29. **`getIndexes` honours `ifModifiedSince`, using the newest track `updated_at`.** If wrong (a client stops
    refreshing): ignore the parameter, as upstream.
30. **There is no rate limiting on `/rest` in M2.** If wrong (brute force on `p=` passwords is a concern before
    M8): add a per-IP failed-auth limiter in the extractor, about half a day.
31. **Invalid parameter values answer 10, as upstream; unknown endpoints answer 0, as upstream (not HTTP 404).**
    If wrong: both are single mapping sites.
32. **Stars, ratings and bookmarks of a missing track are kept (hidden) until the purge.** If wrong: delete them
    when the track is marked missing; a returning file would then lose them.
