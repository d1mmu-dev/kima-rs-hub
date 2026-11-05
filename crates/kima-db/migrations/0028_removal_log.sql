-- One row per track the reconcile retired (D-161): a snapshot that outlives the track, so an
-- admin can see what a scan removed and why. No foreign key to `tracks` on purpose.
CREATE TABLE removal_log (
    id          uuid PRIMARY KEY,
    file_path   text        NOT NULL,
    title       text        NOT NULL,
    artist_name text        NOT NULL,
    album_title text        NOT NULL,
    reason      text        NOT NULL DEFAULT 'file-missing-grace'
                CHECK (reason IN ('file-missing-grace', 'replaced-by-another-file')),
    -- [{"table": "transcodes", "path": "<absolute path of a cache file unlinked after the purge>"}]
    artifacts   jsonb       NOT NULL DEFAULT '[]'::jsonb,
    removed_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX removal_log_removed_at_idx ON removal_log (removed_at DESC);
