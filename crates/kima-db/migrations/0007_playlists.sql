-- Playlists. Order is a dense integer `sort` (0..n-1) kept consistent under a row lock on the
-- playlist, see kima_db::playlists.

CREATE TABLE playlists (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       text NOT NULL,
    is_public  boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX playlists_user_idx ON playlists (user_id);
CREATE INDEX playlists_public_idx ON playlists (is_public) WHERE is_public;

CREATE TABLE playlist_items (
    id          uuid PRIMARY KEY,
    playlist_id uuid NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    -- A track purged after its missing-file grace period leaves the playlist.
    track_id    uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    sort        integer NOT NULL,
    UNIQUE (playlist_id, track_id)
);
CREATE INDEX playlist_items_order_idx ON playlist_items (playlist_id, sort);
CREATE INDEX playlist_items_track_idx ON playlist_items (track_id);

CREATE TABLE hidden_playlists (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    playlist_id uuid NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, playlist_id)
);
