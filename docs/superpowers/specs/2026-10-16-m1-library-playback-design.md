# M1: Library & playback — Design

Date: 2026-10-16
Status: Draft, builder-checked, pending owner approval
Parent spec: `docs/superpowers/specs/2026-10-16-kima-rust-design.md` (§8 row M1)
Roadmap: `docs/superpowers/plans/2026-10-16-kima-rs-roadmap.md`
Builds on: M0 as merged on `main` (crates kima-core, kima-db, kima-jobs, kima-api, kima-server;
forked frontend as static export).

## 1. Intent

M1 makes kima-rs useful for the first time. You point it at a music folder, it builds a
library, and you can browse, search and play from the forked frontend.

The owner's library is messy: inconsistent tags, inconsistent folders, files from different
sources. So M1 has two jobs beyond what the parent spec lists:

1. **Always show a clean structure.** Whatever the files look like on disk, kima presents
   *Artist → Year – Album → songs*. This is a view built from tags, with folder names as a
   fallback. It never touches the files.
2. **Optionally make the files clean too.** An *organizer* proposes a tidy layout on disk
   (`Artist/YYYY - Album/NN - Title.ext`), shows a preview, and copies (or, if asked, moves)
   the files into it.

Success: after M1, the owner can run `docker compose up`, scan their real library, find any
artist/album/song, play it in the browser (seeking works, low-bandwidth transcodes work), build
playlists and likes, and get a preview of how their files would be organized, then apply it.

## 2. Goals

From the parent spec's M1 row:
- User management: admin creates/lists/deletes users, changes roles, toggles registration;
  any user changes their own password (bumps `token_version`).
- Scanner (lofty) that keeps the library current by watching for changes; artwork; browse; tsvector search.
- Streaming with HTTP range requests; ffmpeg transcode at 320/192/128 kbps.
- Plays (listening history), playlists, likes, playback state (resume/queue sync).

Added for the messy library:
- The clean *Artist → Year – Album → songs* view (§4).
- A "needs attention" report listing what the scanner had to guess (§4.5).
- The organizer: preview, then copy/move into the clean layout, with an undo journal (§5).

Carried forward from M0 (roadmap "Notes carried forward", all must land in M1):
- Close a user's open SSE streams when their `token_version` is bumped.
- Gate unported frontend sections on `/api/system/features` (D-030); stub or gate the
  post-login polls `/api/notifications/downloads/active` and `/api/discover/batch-status`.
- Accept `?token=` (stream token) on media/stream paths.
- Wire the kima-jobs worker into `main` with the shared shutdown token and a bounded drain;
  retry LISTEN when it failed at startup; emit a resync event to SSE clients on broadcast lag.
- Decide spec §10 `{error, code}` vs upstream `{error}` (see §9).
- Same-route id→id client navigation (e.g. similar album) resets page state: verify in Playwright.
- Playwright smoke runs in CI (spec §11 says "from M1 on").

## 3. Non-goals (M1)

- MusicBrainz / AcoustID identification, artist bios, genres, lyrics: M3. M1 trusts tags and
  folder names; it does not look anything up online.
- Rewriting tags inside audio files. The organizer moves/copies files and renames them; it
  never edits their contents.
- The beets-style autotagger. The owner wants kima to tidy the library "just like beets":
  match albums against MusicBrainz, propose corrected tags, write them into the files, then
  re-organize. Agreed order: M1 ships the organizer driven by current tags plus folder-name
  guesses (this document), and M3 adds the autotag step on top of the same organizer
  (MusicBrainz match, review, tag writing, re-organize). The M1 organizer must therefore keep
  its path-building and apply/journal code separate from where field values come from, so
  M3 can feed it corrected values.
- Subsonic API (M2), ML analysis (M4), vibe/radio/mixes (M5), podcasts/audiobooks (M6),
  Lidarr/Soulseek/downloads/discovery (M7).
- Waveforms and "prewarm" endpoints, unless a page the M1 frontend shows cannot work without them.

## 4. The library view

### 4.1 Where each field comes from

Per audio file (lofty reads tags; formats: everything lofty supports, at minimum mp3, flac,
ogg/opus, m4a/aac, wav):

| Field | First choice | Fallback | Last resort |
|---|---|---|---|
| Album artist (groups the album) | `albumartist` tag | `artist` tag | folder name (see 4.2), then "Unknown Artist" |
| Track artist | `artist` tag | album artist | — |
| Album | `album` tag | folder name | "Unknown Album" |
| Year | `year`/`date` tag (first 4 digits) | a 4-digit year in the album folder name, e.g. `1997 - OK Computer` or `OK Computer (1997)` | none (shown as "—") |
| Disc / track number | tags | leading number in the file name (`03 - …`, `1-03 …`) | 0 |
| Title | `title` tag | file name without extension and leading number | — |

This follows upstream's scanner (`musicScanner.ts`): album artist first, track artist second,
folder third; "feat." tracks stay on the album artist's album; band names are never split on
"&" or "and". Compilations (`compilation` tag, or album artist "Various Artists" in any common
spelling) group under one canonical "Various Artists".

### 4.2 Folder fallback

When tags are missing, the scanner reads the path relative to `MUSIC_PATH` the same way
upstream's `extractArtistFromRelativePath`/`extractAlbumFromRelativePath` do. It handles the
common layouts `Artist/Album/track`, `Artist/Year - Album/track`, and `Artist - Album/track`.
The builder ports those helpers and their unit tests.

### 4.3 Grouping and identity

- Artists are matched case-insensitively after Unicode normalization (NFC) and whitespace
  trimming. The best-capitalized spelling wins as the display name (as upstream does).
- An album is identified by (album artist, normalized album title, album year), as decided in
  §9. The album year is the most common year among that album's tracks within one folder. The
  scanner computes it per (folder, album artist, album title) group before writing. So one
  album folder whose tracks carry different years stays one album, and the conflict is
  reported (§4.5). The same title in two folders with different years stays two albums
  (remasters).
- A track is identified by its file path. Rescans update rows in place by path; a file whose
  size and mtime are unchanged is skipped.
- Files that disappear are marked missing, not deleted, so plays, likes and playlist entries
  survive a temporarily unmounted drive. They are removed after a grace period (builder's call:
  keep the upstream behaviour if it has one, else 7 days).

### 4.4 Keeping the library up to date (watching)

The owner's requirement: kima watches the library and, on every change, adds, updates or
removes only the files that changed. There is no nightly full rescan.

- **Watcher.** The `notify` crate watches the library root recursively. Events are debounced
  (about 2 s after the last event for a path), so a file being copied in is read once it is
  complete, not halfway through. The debounced paths are queued as small jobs on the `library`
  queue. Each job is a single-file update:
  - a new file is added;
  - a changed file (size or mtime differs) has its tags re-read;
  - a deleted file is marked missing;
  - a renamed or moved file is re-pointed by its fingerprint (§9), so its history is kept.
  Folder renames are expanded into their files.
- **Fallback mode.** Network shares (NFS/SMB) and some Docker bind mounts deliver no change
  events. `KIMA_WATCH_MODE` sets the behaviour:
  - `auto` (default) uses native events where they work;
  - `poll` uses notify's polling watcher at `KIMA_WATCH_POLL_SECS` (default 60) for such mounts;
  - `off` disables watching.
  The settings page shows which mode is active.
- **Safety-net reconcile, not a schedule.** Change events can be lost: the watcher can overflow
  when many files change at once, inotify can hit its watch limit, or kima can be stopped
  while files change. So kima runs an incremental reconcile in three cases: on startup, when
  the watcher reports an overflow or error, and on demand from a "Rescan" button. A reconcile
  walks the tree but skips every file whose size and mtime are unchanged. It is a full read
  only for files that actually differ. If the watch limit is too low for the library, the
  settings page says so and names the sysctl to raise.
- **kima's own changes.** While the organizer applies a move, the watcher ignores paths in the
  run's journal. The organizer updates those rows itself (§5.2).

### 4.5 "Needs attention" report

An admin-only page and endpoint that lists, per album, what the scanner had to guess:
- artist or album taken from folder names, or "Unknown …"
- missing year, or tracks of one album carrying different years
- missing or duplicate track numbers
- tracks of one folder landing in different albums (often a sign of bad tags)

This is how the owner sees how messy the library is, and what the organizer will do with it.
It is a list, not an editor; there is no tag editing in M1.

### 4.6 Artwork

Embedded cover art first; else `cover.*`, `folder.*`, `front.*` in the album folder. Images are
resized (the `image` crate) to the sizes the frontend asks for and cached under
`KIMA_DATA_DIR`. Artist images: a local `artist.*` in the artist folder if present (as
upstream), else none until M3.

## 5. The organizer

### 5.1 What it does

It proposes a target path for every track, from the same fields the library view uses:

```
<Album artist>/<Year> - <Album>/<NN> - <Title>.<ext>
```

- Multi-disc albums: `<D>-<NN> - <Title>.<ext>` (e.g. `2-05 - …`).
- No year: `<Album artist>/<Album>/…`.
- Compilations: `Various Artists/<Year> - <Album>/<NN> - <Track artist> - <Title>.<ext>`.
- Names are made safe for Linux, Windows and macOS filesystems: forbidden characters
  replaced, trailing dots and spaces removed, reserved names avoided, each component kept to
  a sane length. Two tracks that would get the same target get a ` (2)` suffix, and both are
  flagged in the preview.
- Files the scanner does not treat as audio, but which sit in an album folder (cover images,
  `.cue`, `.log`), travel with the album.

### 5.2 How it runs

1. **Preview** (always first). The admin opens the organizer page and sees a table: current
   path → proposed path, grouped by album, with warnings (guessed fields from §4.5,
   collisions, unchanged files). Nothing touches the disk. The preview is a job, so large
   libraries don't block a request; progress arrives over SSE.
2. **Apply**, with one of two modes:
   - **Copy (default, as in beets).** Files are copied into a separate, writable target folder
     (`KIMA_ORGANIZE_TARGET`, e.g. `/organized`). The original library stays untouched. This
     works with the default compose setup, which mounts `/music` read-only. When the copy is
     verified, the page offers **"Switch library to the organized copy"**. That re-points every
     copied track to its new path using the journal, and stores the new library root in system
     settings (overriding `MUSIC_PATH`), so plays, likes and playlists carry over. The owner can
     unmount the old folder afterwards. The library has one root at a time.
   - **Move (opt-in).** Files are renamed in place inside `MUSIC_PATH`. This needs the mount
     to be writable, and the page says so plainly. After a move, kima updates track paths
     directly (no full rescan needed), and plays/likes/playlists keep pointing at the same tracks.
3. **Journal and undo.** Every apply writes a journal (old path, new path, checksum) under
   `KIMA_DATA_DIR`. A move can be undone from the journal. A copy can be undone by deleting
   the target, which the page offers.

Safety rules, all of them tested:
- Never overwrite an existing file; skip it and report it.
- Copy: verify each copy by size and checksum before counting it done.
- Move: same filesystem uses rename; across filesystems it copies, verifies, then deletes the source.
- The organizer runs as a background job, one at a time, resumable after a crash from the journal.
- Empty folders left behind by a move are removed; non-empty ones are left alone.

### 5.3 Where it lives

A new admin page in the forked frontend ("Organize library", under settings), since upstream
has no such feature. A new divergence row records it.

## 6. Playback

- `GET` stream endpoint with byte-range support (206, `Accept-Ranges`, `Content-Range`) for
  the original file.
- Transcoding with ffmpeg to 320, 192 and 128 kbps (format as upstream; builder checks what
  upstream uses and what the player requests). Transcodes are cached under `KIMA_DATA_DIR`
  with a size cap, so seeking inside a transcoded stream works on a second play. The first
  play streams while transcoding.
- Stream auth: Bearer header, or the stream token in `?token=` (4 h, `scope: "stream"`),
  issued as upstream does.
- Plays are recorded when upstream records them (builder checks the threshold), and feed
  "recently listened".
- Playback state (current track, position, queue) is synced per user, as upstream's
  `playbackState` routes do.

## 7. API surface

Port what the frontend's M1 pages call, with upstream's request and response shapes. At
minimum, from `frontend/lib/api.ts`:
- `/library/artists`, `/library/artists/:id`, `/library/albums`, `/library/albums/:id`,
  `/library/tracks`, `/library/tracks/:id`, `/library/tracks/shuffle`,
  `/library/recently-added`, `/library/recently-listened`, `/library/scan…`
- streaming and cover-art endpoints
- `/search` (library search only; the `discover` variants stay gated)
- `/playlists…` (CRUD, items, move, hide; "pending" items belong to M7 imports and stay gated)
- plays, likes, playback state
- user management, registration toggle, change password

The builder confirms the exact list by reading which calls the M1 pages make, and gates
everything else via `/api/system/features` and the frontend nav (D-030).

## 8. Data model (new migrations)

`artists`, `albums`, `tracks` (path, size, mtime, duration, codec, bitrate, disc/track numbers,
year, title, the source of each guessed field, missing-since), `artwork` cache metadata, `plays`,
`likes`, `playlists` and `playlist_items`, `playback_state`, `organizer_runs` and
`organizer_journal`. tsvector columns with triggers and GIN indexes on artist/album/track, per
the parent spec §3. Exact columns are the builder's call, following upstream's Prisma models
where the frontend depends on a field.

## 9. Decisions I made (owner can override)

- **Clean structure is a view, organizing files is opt-in.** The owner asked for the
  Artist → Year – Album → songs structure. kima always shows it, and physically reorganizing
  files is a separate, previewed, reversible step. Copy is the default because the image
  mounts music read-only.
- **No tag writing in M1.** The organizer renames and moves; it does not edit file contents.
- **Naming pattern** as in §5.1 (zero-padded two-digit track numbers, disc prefix only for
  multi-disc albums, track artist in the file name only for compilations).
- **Year is not part of album identity** (§4.3).
- **Error bodies stay `{error}`** (upstream's shape, which the frontend reads). The spec §10
  `code` field is added later only if a client needs it; the spec gets a note.
- **Missing files** are kept for a grace period before removal (§4.3).
- **IDs** are UUIDv7 (as in M0), serialised as strings. The frontend treats ids as opaque
  strings, so no shape change is needed.
- **Album identity, precisely:** (album artist, normalized title, year when known). Tags win
  over folder names. Two folders with the same artist, title and year merge into one album
  (e.g. `CD1/`, `CD2/` folders). The same title with different years stays separate (remasters).
  Same artist and title with no year anywhere merges. Album rows are keyed on this identity,
  not on folders, so they survive an organizer move.
- **Moved files keep their history.** Each track stores a content fingerprint: size plus a hash
  of the first and last 64 KiB, which is cheap and needs no full-file read. When a scan finds a
  new path whose fingerprint matches a track marked missing, it re-points that track instead of
  creating a new one. This covers external moves and crashes mid-move, and keeps plays, likes
  and playlists attached.
- **Watcher updates, reconciles and organizer runs never overlap.** They share one
  single-concurrency job queue (`library`), so a single-file update waits while an organizer
  run is applying. The library stays current through watching (§4.4), with no scheduled
  rescan (owner's decision). Progress for reconcile and organizer runs goes out as SSE events
  `{"type":"library:progress","job":"scan"|"organize","done":n,"total":m}`.
- **Transcode seeking:** while a transcode is still being written, the stream answers 200
  without range support. Once cached, ranges work like the original file.
- **CI** runs the Playwright smoke on every push, using the same Postgres service, ffmpeg from
  apt, and the built export.

## 10. Testing

- Unit tests: field extraction and folder fallback (ported upstream cases), grouping,
  year conflicts, organizer path building and sanitization, collision handling.
- Fixture audio generated in tests with ffmpeg (short sine tones in each format, with and
  without tags, in messy folder layouts). No copyrighted audio is committed.
- DB tests with `#[sqlx::test]`: scan → rescan → file removed → file back; organizer apply,
  crash mid-apply, resume, undo.
- HTTP tests: range requests (start, middle, suffix, invalid), transcode headers, auth via
  `?token=`.
- Playwright smoke in CI: register, scan a fixture library, open an album, play a track,
  create a playlist, open the organizer preview.

## 11. Left to the builder

Exact table columns; the transcode container/codec; the transcode cache size; scan
concurrency; the organizer page's exact layout (a table with filters is enough); how plays
are counted (follow upstream); which minor endpoints the M1 pages need beyond §7.
