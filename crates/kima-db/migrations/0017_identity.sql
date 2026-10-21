ALTER TABLE artists
    ADD COLUMN mbid                uuid,
    ADD COLUMN mbid_source         text CHECK (mbid_source IN ('tag', 'search', 'user', 'autotag')),
    ADD COLUMN bio                 text,          -- Wikipedia summary (upstream Artist.summary)
    ADD COLUMN image_path          text,          -- relative to <data_dir>/artist-images
    ADD COLUMN image_source        text CHECK (image_source IN ('local', 'wikidata', 'fanart', 'deezer', 'user')),
    ADD COLUMN "similar"           jsonb,         -- [{name, mbid|null, match}] (upstream similarArtistsJson)
    ADD COLUMN enrichment_status   text NOT NULL DEFAULT 'pending'
        CHECK (enrichment_status IN ('pending', 'enriching', 'completed', 'failed', 'unresolvable')),
    ADD COLUMN enrichment_failures smallint NOT NULL DEFAULT 0,
    ADD COLUMN last_enriched_at    timestamptz,
    ADD COLUMN last_error          text,          -- last enrichment failure, for the failures list
    ADD COLUMN last_error_code     text,
    ADD COLUMN display_name        text,          -- user overrides (MetadataEditor)
    ADD COLUMN user_bio            text,
    ADD COLUMN user_genres         text[];
CREATE INDEX artists_mbid_idx ON artists (mbid) WHERE mbid IS NOT NULL;
CREATE INDEX artists_enrichment_idx ON artists (enrichment_status, last_enriched_at);

ALTER TABLE albums
    ADD COLUMN release_mbid       uuid,           -- from tags, the autotagger or a user (upstream stores none)
    ADD COLUMN release_mbid_source text CHECK (release_mbid_source IN ('tag', 'autotag', 'user')),
    ADD COLUMN release_group_mbid uuid,           -- upstream Album.rgMbid
    ADD COLUMN release_group_mbid_source text CHECK (release_group_mbid_source IN ('tag', 'autotag', 'user')),
    ADD COLUMN label              text,
    ADD COLUMN image_path         text,           -- downloaded cover, relative to <data_dir>/album-images
    ADD COLUMN image_source       text CHECK (image_source IN ('deezer', 'coverartarchive', 'fanart', 'user')),
    ADD COLUMN display_title      text,
    ADD COLUMN display_year       int CHECK (display_year BETWEEN 1 AND 9999),
    ADD COLUMN user_genres        text[];
CREATE INDEX albums_release_group_idx ON albums (release_group_mbid) WHERE release_group_mbid IS NOT NULL;
CREATE INDEX albums_release_idx ON albums (release_mbid) WHERE release_mbid IS NOT NULL;
-- albums.cover_path (0004, never used) is dropped:
ALTER TABLE albums DROP COLUMN cover_path;

ALTER TABLE tracks
    ADD COLUMN tag_recording_mbid     uuid,       -- as read from the file (§8.1)
    ADD COLUMN tag_release_track_mbid uuid,
    ADD COLUMN tag_release_mbid       uuid,
    ADD COLUMN tag_release_group_mbid uuid,
    ADD COLUMN tag_artist_mbids       uuid[] NOT NULL DEFAULT '{}',
    ADD COLUMN tag_album_artist_mbids uuid[] NOT NULL DEFAULT '{}',
    ADD COLUMN disc_total             int,        -- read from tags (organizer naming, §20.5)
    ADD COLUMN track_total            int,
    ADD COLUMN isrc                   text CHECK (isrc ~ '^[A-Z]{2}[A-Z0-9]{3}[0-9]{7}$'),
    ADD COLUMN isrc_source            text CHECK (isrc_source IN ('id3', 'musicbrainz')),
    ADD COLUMN recording_mbid         uuid,       -- the identification answer (upstream Track.recordingMbid)
    ADD COLUMN recording_source       text CHECK (recording_source IN
        ('file_tag', 'tracklist', 'isrc', 'search', 'acoustid', 'autotag'));
CREATE INDEX tracks_recording_mbid_idx ON tracks (recording_mbid) WHERE recording_mbid IS NOT NULL;
CREATE INDEX tracks_isrc_idx ON tracks (isrc) WHERE isrc IS NOT NULL;

-- The AcoustID fingerprint lives apart from tracks: ~3.4 kB per row, read only by the recording stage.
CREATE TABLE track_acoustid (
    track_id      uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    fingerprint   text NOT NULL,                  -- compressed, base64url (AcoustID's format)
    duration_secs int NOT NULL CHECK (duration_secs > 0),
    computed_at   timestamptz NOT NULL DEFAULT now()
);

-- MusicBrainz's Various Artists id; never looked up (names::key(VARIOUS_ARTISTS)).
UPDATE artists SET mbid = '89ad4ac3-39f7-470e-963a-56509c546377', enrichment_status = 'completed'
WHERE name_key = 'various artists';
