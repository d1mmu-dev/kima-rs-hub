-- Listening history and per-user playback state.

CREATE TABLE plays (
    id        uuid PRIMARY KEY,
    user_id   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Purging a missing track (after its grace period) removes its plays.
    track_id  uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    played_at timestamptz NOT NULL DEFAULT now(),
    -- Recorded by the listen rule (enough of the track was heard).
    heard     boolean NOT NULL DEFAULT false
);
CREATE INDEX plays_user_played_idx ON plays (user_id, played_at);
CREATE INDEX plays_user_track_idx ON plays (user_id, track_id);
CREATE INDEX plays_track_idx ON plays (track_id);

CREATE TABLE playback_state (
    user_id       uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    playback_type text NOT NULL,
    -- Not a foreign key: the client clears state that points at something gone.
    track_id      text,
    audiobook_id  text,
    podcast_id    text,
    queue         jsonb,
    current_index integer NOT NULL DEFAULT 0,
    is_shuffle    boolean NOT NULL DEFAULT false,
    -- Offset into the current track, milliseconds.
    position      integer NOT NULL DEFAULT 0,
    changed_by    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
