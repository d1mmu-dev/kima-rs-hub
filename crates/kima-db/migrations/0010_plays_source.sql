-- Where a play came from. Mirrors upstream's ListenSource, limited to the two values kima-rs
-- can produce.
ALTER TABLE plays ADD COLUMN source text NOT NULL DEFAULT 'LIBRARY'
    CHECK (source IN ('LIBRARY', 'SUBSONIC'));
