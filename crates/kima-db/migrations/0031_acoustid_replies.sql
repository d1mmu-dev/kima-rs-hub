-- What AcoustID answered for a fingerprint of a given length (upstream keeps these in Redis as
-- `acoustid:lookup:<seconds>:<sha256>`): a song is asked once. `results` holds the parsed
-- results as kima-rs reads them, never the raw response.
CREATE TABLE acoustid_replies (
    fingerprint_hash bytea       NOT NULL,         -- sha256(trimmed fingerprint): small key
    duration_secs    integer     NOT NULL,
    status           text        NOT NULL CHECK (status IN ('matched', 'unmatched', 'rejected')),
    results          jsonb       NOT NULL DEFAULT '[]',
    saved_at         timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    PRIMARY KEY (fingerprint_hash, duration_secs)
);
CREATE INDEX acoustid_replies_expiry_idx ON acoustid_replies (expires_at);
