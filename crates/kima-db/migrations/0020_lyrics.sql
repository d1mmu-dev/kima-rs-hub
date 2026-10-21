-- Upstream track_lyrics (schema.prisma:1069-1080), same column names.
CREATE TABLE track_lyrics (
    track_id      uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    plain_lyrics  text,
    synced_lyrics text,                             -- LRC text
    source        text NOT NULL CHECK (source IN ('embedded', 'lrclib', 'none')),
    lrclib_id     int,
    fetched_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
