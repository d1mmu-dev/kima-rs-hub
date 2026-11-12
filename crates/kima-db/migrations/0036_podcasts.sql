-- M6a podcasts (plan 2026-11-12-m6-podcasts.md, Task 1). Upstream models `Podcast`,
-- `PodcastEpisode`, `PodcastSubscription`, `PodcastProgress`, `PodcastDownload`
-- (`schema.prisma:471-566` at v1.12.3) in snake_case; ids are UUIDv7 (D-016).
-- Search vectors use `simple` + `unaccent` like the library (D-209), not upstream's `english`.

CREATE TABLE podcasts (
    id                 uuid PRIMARY KEY,
    feed_url           text NOT NULL UNIQUE,
    title              text NOT NULL,
    author             text,
    description        text,
    image_url          text,
    itunes_id          text UNIQUE,
    -- Apple podcast category ids, most specific first.
    category_ids       integer[] NOT NULL DEFAULT '{}',
    language           text,
    explicit           boolean NOT NULL DEFAULT false,
    episode_count      integer NOT NULL DEFAULT 0,
    last_refreshed     timestamptz NOT NULL DEFAULT now(),
    feed_etag          text,
    feed_last_modified text,
    -- The last refresh failure (D-205; upstream keeps it in `EnrichmentFailure`).
    refresh_error      text,
    refresh_failed_at  timestamptz,
    refresh_failures   integer NOT NULL DEFAULT 0,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    search             tsvector NOT NULL DEFAULT ''::tsvector
);
CREATE INDEX podcasts_last_refreshed ON podcasts (last_refreshed);
CREATE INDEX podcasts_search ON podcasts USING gin (search);

CREATE TABLE podcast_episodes (
    id             uuid PRIMARY KEY,
    podcast_id     uuid NOT NULL REFERENCES podcasts (id) ON DELETE CASCADE,
    guid           text NOT NULL,
    title          text NOT NULL,
    description    text,
    audio_url      text NOT NULL,
    duration       integer NOT NULL DEFAULT 0,
    published_at   timestamptz NOT NULL,
    episode_number integer,
    season         integer,
    image_url      text,
    -- bigint: upstream's Int overflows past 2 GiB.
    file_size      bigint,
    mime_type      text DEFAULT 'audio/mpeg',
    created_at     timestamptz NOT NULL DEFAULT now(),
    search         tsvector NOT NULL DEFAULT ''::tsvector,
    UNIQUE (podcast_id, guid)
);
CREATE INDEX podcast_episodes_show_published ON podcast_episodes (podcast_id, published_at);
CREATE INDEX podcast_episodes_search ON podcast_episodes USING gin (search);

CREATE TABLE podcast_subscriptions (
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    podcast_id    uuid NOT NULL REFERENCES podcasts (id) ON DELETE CASCADE,
    subscribed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, podcast_id)
);
CREATE INDEX podcast_subscriptions_podcast ON podcast_subscriptions (podcast_id);

-- "current_time" is quoted: CURRENT_TIME is an SQL keyword.
CREATE TABLE podcast_progress (
    user_id        uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    episode_id     uuid NOT NULL REFERENCES podcast_episodes (id) ON DELETE CASCADE,
    "current_time" double precision NOT NULL DEFAULT 0,
    duration       double precision NOT NULL DEFAULT 0,
    is_finished    boolean NOT NULL DEFAULT false,
    last_played_at timestamptz NOT NULL DEFAULT now(),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, episode_id)
);
CREATE INDEX podcast_progress_recent ON podcast_progress (user_id, last_played_at);

CREATE TABLE podcast_downloads (
    user_id          uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    episode_id       uuid NOT NULL REFERENCES podcast_episodes (id) ON DELETE CASCADE,
    -- Relative to the data directory (D-204).
    local_path       text NOT NULL,
    file_size_mb     double precision NOT NULL,
    downloaded_at    timestamptz NOT NULL DEFAULT now(),
    last_accessed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, episode_id)
);
CREATE INDEX podcast_downloads_accessed ON podcast_downloads (last_accessed_at);

-- Upstream `20251230000000_add_podcast_audiobook_search_vectors`: show title A, author B,
-- description C; episode title A, description B; rebuilt when one of those columns changes.
CREATE FUNCTION podcasts_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', unaccent(COALESCE(NEW.title, ''))), 'A')
        || setweight(to_tsvector('simple', unaccent(COALESCE(NEW.author, ''))), 'B')
        || setweight(to_tsvector('simple', unaccent(COALESCE(NEW.description, ''))), 'C');
    RETURN NEW;
END $$;

CREATE TRIGGER podcasts_search_update
    BEFORE INSERT OR UPDATE OF title, author, description ON podcasts
    FOR EACH ROW EXECUTE FUNCTION podcasts_search_update();

CREATE FUNCTION podcast_episodes_search_update() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.search := setweight(to_tsvector('simple', unaccent(COALESCE(NEW.title, ''))), 'A')
        || setweight(to_tsvector('simple', unaccent(COALESCE(NEW.description, ''))), 'B');
    RETURN NEW;
END $$;

CREATE TRIGGER podcast_episodes_search_update
    BEFORE INSERT OR UPDATE OF title, description ON podcast_episodes
    FOR EACH ROW EXECUTE FUNCTION podcast_episodes_search_update();
