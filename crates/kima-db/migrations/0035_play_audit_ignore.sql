-- Tracks a user told the play-history audit to stop raising (upstream PlayAuditIgnore).
CREATE TABLE play_audit_ignore (
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id   uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    ignored_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, track_id)
);
