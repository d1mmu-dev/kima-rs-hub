-- The verdict of the album vote (upstream keeps it in Redis as `acoustid:album-vote:<album>`):
-- which release group an untagged album's songs agree on, decided once. `signature` hashes the
-- album's present songs (ids, fingerprints, file times): a changed song makes the verdict stale.
-- `expires_at`: a verdict is kept 7 days, "waiting for AcoustID" 2 minutes, "postponed" 15.
CREATE TABLE album_votes (
    album_id   uuid        PRIMARY KEY REFERENCES albums (id) ON DELETE CASCADE,
    signature  text        NOT NULL,
    verdict    jsonb       NOT NULL,   -- {"kind":"assign"|"none", …, "outcome":"assigned"|…}
    decided_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
