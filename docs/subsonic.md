# Subsonic and OpenSubsonic clients

kima-rs speaks the [OpenSubsonic](https://opensubsonic.netlify.app/) API (version 1.16.1) at `/rest`, so music apps that support Subsonic can play from your library. The Kima web app does not use this API; it is there for other apps. Every difference from upstream Kima is logged in [`DIVERGENCES.md`](DIVERGENCES.md) (D-072 to D-103).

## Connect a client

1. In Kima, open **Settings > Native Apps**.
2. Type a name for the client, such as "Phone", and choose **Generate Token**.
3. Copy the token. It is shown once.
4. In the client, enter:
   - **Server address:** where you open Kima, such as `http://192.168.1.10:3030`
   - **Username:** your Kima username (case does not matter)
   - **Password:** the token

Give each client its own token. The same page lists your tokens (at most 50 per user), and revoking one disconnects only that client. Clients that have a separate "API key" field can use the token there (`apiKey=`, with or without the username).

## How sign-in works

A request may carry one of these:

1. An API key: `apiKey=`.
2. The password field: `p=`. Kima first checks whether the value is one of your tokens, then checks it against your account password. `enc:` hex-encoded passwords are accepted.
3. Token and salt: `t=` and `s=`, where `t = md5(token + s)`. This works for app tokens only. Your account password cannot be used with it, and the error says so.

Mixing mechanisms (for example `apiKey` with `p`) is error 43. Empty values count as absent.

If you turn on two-factor authentication later (a planned milestone), your account password will stop working in Subsonic clients, because the protocol has no way to ask for a code. Use a token instead.

### Why the server can decrypt tokens

Token and salt sign-in is the older scheme most apps still use. To check `t`, the server must compute `md5(token + s)` itself, so it has to recover the token; a one-way hash is not enough. Upstream Kima stores only SHA-256 digests and so refuses `t`/`s`. kima-rs stores two things per token:

- the SHA-256 of the token, which answers `apiKey=` and `p=` in one lookup without decrypting anything;
- the token encrypted with XChaCha20-Poly1305 under a key derived from `SETTINGS_ENCRYPTION_KEY`, bound to its database row.

What this protects against:

- A leaked database dump or backup on its own reveals no token.
- Someone who has both the database and `SETTINGS_ENCRYPTION_KEY` (kept in `/data/secrets.json` or the environment) can recover tokens and call `/rest` as those users, until the tokens are revoked. That is the price of supporting `t`/`s`. The damage stays small: tokens do not work on `/api`, and a token can never create, change or delete users or change a password, even for an admin.
- Your account password is stored as argon2id and can never be recovered, and is never usable with `t`/`s`.

### Key rotation and password changes

- **Rotating or losing `SETTINGS_ENCRYPTION_KEY`** makes the stored ciphertexts unreadable. Apps that use `t`/`s` then fail with "Wrong username or password" until you generate new tokens for them. Apps that send the token as `apiKey=` or `p=` keep working. The server logs a warning naming the affected token id (never its value), at most once an hour per token.
- **Changing your password disconnects every app.** All of your tokens are deleted in the same step, whether you change it on the web or through Subsonic. Changing the password is the usual response to a compromise, and a token an intruder made must not outlive it. Generate new tokens afterwards. The settings page warns about this.

## Failed sign-in limiter and reverse proxies

Wrong password guesses (on `/rest` with `p=`, and on the web login) are limited per client address and username: the first 5 failures are free, then the pair is blocked for 2, 4, 8 seconds and so on, up to 15 minutes. A success clears it. Blocked requests answer Subsonic error 41 on `/rest` and HTTP 429 with `Retry-After` on the web login. App tokens are never limited, so apps that use them keep working while a password is blocked.

The limiter uses the TCP peer address and does not trust `X-Forwarded-For`. Behind a reverse proxy, every client shares the proxy's address, so the key degenerates to the username: someone guessing wrongly at your username can block password logins for it for up to 15 minutes. Tokens are unaffected. Trusting forwarded headers from configured proxies is planned for later.

## `KIMA_PUBLIC_URL`

Optional. The address clients use to reach Kima, such as `https://music.example.com` (must start with `http://` or `https://`; a trailing `/` is removed). The `hls` endpoint uses it to build the stream URL in its playlist; if it is unset, Kima uses the scheme and `Host` of the request. The settings page shows it to help with client setup. Set it when Kima is behind a proxy that changes the host name.

## What is supported

All 81 paths upstream registers are answered, each with and without `.view`, by GET, HEAD or POST (form bodies are merged with the query string). 44 are complete, 8 partial and 29 stubs.

- **System:** `ping`, `getLicense`, `getMusicFolders` (one folder, id `1`), `getOpenSubsonicExtensions` (no sign-in needed; advertises `apiKeyAuthentication`, `formPost`, `indexBasedQueue`), `tokenInfo`, `getScanStatus`, `startScan` (admin).
- **Browsing:** `getArtists`, `getIndexes`, `getArtist`, `getMusicDirectory`, `getAlbum`, `getSong`, `getAlbumList` and `getAlbumList2` (every list type except `byGenre`, which is empty until M3).
- **Search:** `search`, `search2` and `search3`. An empty query lists the whole library page by page, which is how Symfonium and others sync. `getRandomSongs` (the `genre` filter waits for M3), `getTopSongs`, `getNowPlaying`.
- **Playing:** `stream`, `download`, `hls`, `getTranscodeDecision`, `getTranscodeStream` (songs; podcasts wait for M6), `getCoverArt` (albums and tracks; artist images wait for M3), `getAvatar`, `scrobble` (batches and now playing).
- **Queue and bookmarks:** `getPlayQueue`, `savePlayQueue`, the by-index variants, and bookmarks.
- **Playlists:** list, read, create, update and delete.
- **Starring and ratings:** `star`, `unstar`, `getStarred`, `getStarred2` and `setRating`, for songs, albums and artists.
- **Users:** `getUser`; an admin signed in with a password can use `getUsers`, `createUser`, `updateUser`, `deleteUser` and `changePassword`. These last four refuse any app token with error 50.

Stubs, by the milestone that makes them real:

| Milestone | Endpoints |
|---|---|
| M3 (identification) | `getGenres`, `getSongsByGenre`, `getLyrics`, `getLyricsBySongId`, `getArtistInfo`, `getArtistInfo2`; `byGenre` lists; artist cover art. These answer empty results (or 70 for artist art) |
| M5 (vibe engine) | `getSimilarSongs`, `getSimilarSongs2` (empty) |
| M6 (podcasts) | `getPodcasts`, `getNewestPodcasts`, `getPodcastEpisode` (70), `refreshPodcasts` (ok, nothing to refresh) |
| M8 (hardening) | `getShares` (empty), `createShare`, `updateShare`, `deleteShare` (error 0, "Sharing is not supported by this server yet") |
| never planned | internet radio, chat, video, captions and the jukebox: listing calls answer empty and calls that change something answer error 0 |
| none (as upstream) | `getAlbumInfo`, `getAlbumInfo2`: empty |

Any other path under `/rest/` answers error 0 "Not implemented: `<path>`".

## Notes

- Song `artist` is the track artist; ids are the web app's UUIDs. The prefixes `ar-`, `al-`, `tr-`, `artist:` and `album:` are accepted where clients send them.
- Ratings and stars are separate from each other and from the web app, which has no track likes (D-071).
- Credentials are never written to the logs: request logging records the method and path only.
- The code lives in `crates/kima-subsonic`; the shared sign-in limiter in `kima_api::login_limit`.
