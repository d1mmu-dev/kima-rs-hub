-- Library: artists, albums, tracks. Search vectors use the `simple` config
-- (no stemming, no accent folding); the name or title weighs A.

CREATE TABLE artists (
    id         uuid PRIMARY KEY,
    name       text NOT NULL,
    name_key   text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    search     tsvector NOT NULL DEFAULT ''::tsvector
);

CREATE TABLE albums (
    id             uuid PRIMARY KEY,
    artist_id      uuid NOT NULL REFERENCES artists (id) ON DELETE RESTRICT,
    title          text NOT NULL,
    title_key      text NOT NULL,
    year           integer CHECK (year IS NULL OR year BETWEEN 1 AND 9999),
    is_compilation boolean NOT NULL DEFAULT false,
    cover_path     text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    search         tsvector NOT NULL DEFAULT ''::tsvector
);
-- Identity is (artist, title, year when known); no year merges with no year.
CREATE UNIQUE INDEX albums_identity_key ON albums (artist_id, title_key, (COALESCE(year, -1)));
CREATE INDEX albums_artist_idx ON albums (artist_id);

CREATE TABLE tracks (
    id            uuid PRIMARY KEY,
    album_id      uuid NOT NULL REFERENCES albums (id) ON DELETE RESTRICT,
    artist_name   text NOT NULL,
    title         text NOT NULL,
    year          integer CHECK (year IS NULL OR year BETWEEN 1 AND 9999), -- the track's own year
    disc          integer NOT NULL DEFAULT 1,
    track_no      integer NOT NULL DEFAULT 0,
    duration_secs integer NOT NULL DEFAULT 0,
    codec         text NOT NULL,
    bitrate       integer,
    path          text NOT NULL UNIQUE,
    size          bigint NOT NULL,
    mtime         timestamptz NOT NULL,
    fingerprint   bytea NOT NULL,
    guessed       jsonb NOT NULL DEFAULT '{}'::jsonb,
    missing_since timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    search        tsvector NOT NULL DEFAULT ''::tsvector
);
CREATE INDEX tracks_album_idx ON tracks (album_id);
CREATE INDEX tracks_missing_fingerprint_idx ON tracks (fingerprint) WHERE missing_since IS NOT NULL;

CREATE INDEX artists_search_idx ON artists USING gin (search);
CREATE INDEX albums_search_idx ON albums USING gin (search);
CREATE INDEX tracks_search_idx ON tracks USING gin (search);

CREATE FUNCTION artists_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', NEW.name), 'A');
    RETURN NEW;
END $$;
CREATE TRIGGER artists_search BEFORE INSERT OR UPDATE OF name ON artists
    FOR EACH ROW EXECUTE FUNCTION artists_search_update();

CREATE FUNCTION albums_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', NEW.title), 'A')
        || setweight(to_tsvector('simple', COALESCE((SELECT name FROM artists WHERE id = NEW.artist_id), '')), 'B');
    RETURN NEW;
END $$;
CREATE TRIGGER albums_search BEFORE INSERT OR UPDATE OF title, artist_id ON albums
    FOR EACH ROW EXECUTE FUNCTION albums_search_update();

CREATE FUNCTION tracks_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', NEW.title), 'A')
        || setweight(to_tsvector('simple', NEW.artist_name), 'B')
        || setweight(to_tsvector('simple', COALESCE((SELECT title FROM albums WHERE id = NEW.album_id), '')), 'C');
    RETURN NEW;
END $$;
CREATE TRIGGER tracks_search BEFORE INSERT OR UPDATE OF title, artist_name, album_id ON tracks
    FOR EACH ROW EXECUTE FUNCTION tracks_search_update();

-- Renames refresh dependent vectors. Touching `title` fires albums_search; the second
-- trigger then refreshes the album's tracks.
CREATE FUNCTION artists_rename_refresh() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE albums SET title = title WHERE artist_id = NEW.id;
    RETURN NULL;
END $$;
CREATE TRIGGER artists_rename AFTER UPDATE OF name ON artists
    FOR EACH ROW WHEN (OLD.name IS DISTINCT FROM NEW.name)
    EXECUTE FUNCTION artists_rename_refresh();

CREATE FUNCTION albums_rename_refresh() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE tracks SET album_id = album_id WHERE album_id = NEW.id;
    RETURN NULL;
END $$;
CREATE TRIGGER albums_rename AFTER UPDATE OF title, artist_id ON albums
    FOR EACH ROW WHEN (OLD.title IS DISTINCT FROM NEW.title OR OLD.artist_id IS DISTINCT FROM NEW.artist_id
                       OR OLD.search IS DISTINCT FROM NEW.search)
    EXECUTE FUNCTION albums_rename_refresh();
