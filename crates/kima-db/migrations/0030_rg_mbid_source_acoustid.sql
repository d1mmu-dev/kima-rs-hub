-- An album's release-group id may come from AcoustID's vote (upstream `Album.rgMbidSource =
-- 'acoustid'`, `cb21c307`). The weakest source: acoustid < tag < autotag < user.
ALTER TABLE albums DROP CONSTRAINT albums_release_group_mbid_source_check;
ALTER TABLE albums ADD CONSTRAINT albums_release_group_mbid_source_check
    CHECK (release_group_mbid_source IN ('tag', 'acoustid', 'autotag', 'user'));
