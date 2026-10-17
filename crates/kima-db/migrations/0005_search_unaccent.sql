-- Accent-insensitive search: vectors are built from unaccent()ed text, and queries are
-- unaccented the same way, so "beyonce" finds "Beyoncé".
CREATE EXTENSION IF NOT EXISTS unaccent;

CREATE OR REPLACE FUNCTION artists_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', unaccent(NEW.name)), 'A');
    RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION albums_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', unaccent(NEW.title)), 'A')
        || setweight(to_tsvector('simple', unaccent(COALESCE((SELECT name FROM artists WHERE id = NEW.artist_id), ''))), 'B');
    RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION tracks_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', unaccent(NEW.title)), 'A')
        || setweight(to_tsvector('simple', unaccent(NEW.artist_name)), 'B')
        || setweight(to_tsvector('simple', unaccent(COALESCE((SELECT title FROM albums WHERE id = NEW.album_id), ''))), 'C');
    RETURN NEW;
END $$;

-- Back-fill. Setting the column fires its BEFORE trigger even when the value is unchanged.
UPDATE artists SET name = name;
UPDATE albums SET title = title;
UPDATE tracks SET title = title;
