-- Per-user Subsonic state: bookmarks, stars and ratings. A track flagged missing keeps its rows
-- (hidden from lists); the purge that deletes the track row removes them by cascade.

CREATE TABLE subsonic_bookmarks (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id    uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    position_ms bigint NOT NULL CHECK (position_ms >= 0),
    comment     text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    changed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, track_id)
);

-- One row per starred item. Exactly one target column is set; purging a track, album or
-- artist removes its stars.
CREATE TABLE stars (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id   uuid REFERENCES tracks (id) ON DELETE CASCADE,
    album_id   uuid REFERENCES albums (id) ON DELETE CASCADE,
    artist_id  uuid REFERENCES artists (id) ON DELETE CASCADE,
    starred_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(track_id, album_id, artist_id) = 1)
);
CREATE UNIQUE INDEX stars_track_key  ON stars (user_id, track_id)  WHERE track_id  IS NOT NULL;
CREATE UNIQUE INDEX stars_album_key  ON stars (user_id, album_id)  WHERE album_id  IS NOT NULL;
CREATE UNIQUE INDEX stars_artist_key ON stars (user_id, artist_id) WHERE artist_id IS NOT NULL;

-- Same shape for ratings 1..5 (0 deletes the row).
CREATE TABLE ratings (
    id        uuid PRIMARY KEY,
    user_id   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id  uuid REFERENCES tracks (id) ON DELETE CASCADE,
    album_id  uuid REFERENCES albums (id) ON DELETE CASCADE,
    artist_id uuid REFERENCES artists (id) ON DELETE CASCADE,
    rating    smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    rated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(track_id, album_id, artist_id) = 1)
);
CREATE UNIQUE INDEX ratings_track_key  ON ratings (user_id, track_id)  WHERE track_id  IS NOT NULL;
CREATE UNIQUE INDEX ratings_album_key  ON ratings (user_id, album_id)  WHERE album_id  IS NOT NULL;
CREATE UNIQUE INDEX ratings_artist_key ON ratings (user_id, artist_id) WHERE artist_id IS NOT NULL;
