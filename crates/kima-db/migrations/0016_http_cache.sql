CREATE TABLE http_cache (
    key        text PRIMARY KEY,               -- "<source>:" || hex(sha256(canonical request))
    source     text NOT NULL,                  -- musicbrainz | acoustid | wikidata | fanart | deezer |
                                               -- listenbrainz | coverartarchive
    request    text NOT NULL,                  -- canonical request, for debugging (secrets stripped)
    outcome    text NOT NULL CHECK (outcome IN ('answered', 'absent')),
    body       jsonb NOT NULL,                 -- the parsed answer, or the absence evidence
    fetched_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX http_cache_expires_idx ON http_cache (expires_at);

-- A source that rate-limited us; survives restarts.
CREATE TABLE source_pauses (
    source       text PRIMARY KEY,
    paused_until timestamptz NOT NULL,
    reason       text NOT NULL,                -- e.g. "HTTP 503", "Retry-After 20"
    consecutive  int NOT NULL DEFAULT 1,       -- consecutive rate-limited outcomes
    since        timestamptz NOT NULL DEFAULT now()
);
