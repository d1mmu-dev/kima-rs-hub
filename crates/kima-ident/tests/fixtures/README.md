# Source fixtures (Task 20)

`wikidata/`, `fanart/`, `deezer/`, `listenbrainz/` and `coverartarchive/` hold **hand-written** replay
entries for The Beatles (`b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d`, release group Abbey Road
`9162580e-5df4-32de-80cc-f45a8d8a9b1d`). They were not recorded: they follow each service's documented
answer shape and upstream's test bodies, and ids, hashes and counts in them are illustrative. Nothing
was sent to the real services while writing them.

Each directory's `requests.json` lists the requests in `tools/record-fixtures` format, so a person can
record the real answers later:

```sh
cargo run -p record-fixtures -- crates/kima-ident/tests/fixtures/deezer/requests.json /tmp/deezer.json
```

For fanart, put a throwaway key in place of `<throwaway key>`; the recorder writes `api_key=REDACTED`,
which is what the hand-written entries use (the tests pass the key `REDACTED`). The 404 and 429 entries
in `fanart/beatles.json` are hand-written on purpose and stay so.

# LRCLIB fixtures (Task 24)

`lrclib/lyrics.json` is **hand-written** too, from LRCLIB's documented `GET /api/get` answer
(`{id, name, trackName, artistName, albumName, duration, instrumental, plainLyrics, syncedLyrics}`)
and its 404 body. The lyrics are the public-domain first verse of "Amazing Grace" (John Newton,
1779); the id, duration and timestamps are illustrative. The 500 entry is hand-written on purpose and
stays so. Nothing was sent to lrclib.net. `lrclib/requests.json` lists the hit and the 404 for a
later recording; the recorder sees production's `/api/get` path, so recorded URLs need the `/api`
prefix dropped to match the tests' replay base.

# Autotag fixtures (M3b)

`musicbrainz/autotag.json` is **hand-written** from MusicBrainz's documented JSON answers; nothing was
sent to musicbrainz.org and every id, barcode, ISRC and count is made up. It holds three releases
("Fixture Band - Fixture Album": 10 tracks on one medium, 2001; "Double Fixture": 2 media of 3 tracks;
"Fixture Hits": a Various Artists compilation, 3 tracks with per-track credits and the VA MBID), all
tracks 2000 ms long so the 2 s ffmpeg test tones match; their release groups with genres and counts;
one release search answer each (`limit=5`, with `tracks:<n>`); a `recording?inc=releases` answer; and a
release 404. `musicbrainz/autotag-requests.json` lists the same requests in `tools/record-fixtures`
format, for recording the real answers later.
