CREATE TABLE genres (
    id   uuid PRIMARY KEY,
    name text NOT NULL,                             -- as MusicBrainz or the tag gives it, trimmed
    key  text NOT NULL UNIQUE                       -- lower(name), whitespace collapsed
);
CREATE TABLE track_genres (
    track_id uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    genre_id uuid NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
    source   text NOT NULL CHECK (source IN ('musicbrainz', 'file')),
    rank     smallint NOT NULL,                     -- 0 = most voted / first in the tag
    PRIMARY KEY (track_id, source, genre_id)
);
CREATE INDEX track_genres_genre_idx ON track_genres (genre_id);
CREATE TABLE album_genres  (album_id  uuid NOT NULL REFERENCES albums (id)  ON DELETE CASCADE,
                            genre_id uuid NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
                            source text NOT NULL CHECK (source IN ('musicbrainz', 'file')),
                            rank smallint NOT NULL, PRIMARY KEY (album_id, source, genre_id));
CREATE TABLE artist_genres (artist_id uuid NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
                            genre_id uuid NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
                            source text NOT NULL CHECK (source IN ('musicbrainz', 'file')),
                            rank smallint NOT NULL, PRIMARY KEY (artist_id, source, genre_id));
CREATE INDEX album_genres_genre_idx ON album_genres (genre_id);
CREATE INDEX artist_genres_genre_idx ON artist_genres (genre_id);

-- Effective genres: MusicBrainz when it has any for the entity, else the file's (§13.2).
CREATE VIEW track_genres_effective AS
SELECT g.track_id, g.genre_id, g.rank FROM track_genres g
WHERE g.source = 'musicbrainz'
   OR NOT EXISTS (SELECT 1 FROM track_genres m WHERE m.track_id = g.track_id AND m.source = 'musicbrainz');
CREATE VIEW album_genres_effective AS
SELECT g.album_id, g.genre_id, g.rank FROM album_genres g
WHERE g.source = 'musicbrainz'
   OR NOT EXISTS (SELECT 1 FROM album_genres m WHERE m.album_id = g.album_id AND m.source = 'musicbrainz');
CREATE VIEW artist_genres_effective AS
SELECT g.artist_id, g.genre_id, g.rank FROM artist_genres g
WHERE g.source = 'musicbrainz'
   OR NOT EXISTS (SELECT 1 FROM artist_genres m WHERE m.artist_id = g.artist_id AND m.source = 'musicbrainz');
