-- Per-user settings (upstream `UserSettings`). No ListenBrainz token column: see D-103.
CREATE TABLE user_settings (
    user_id              uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    playback_quality     text    NOT NULL DEFAULT 'original'
        CHECK (playback_quality IN ('original', 'high', 'medium', 'low')),
    wifi_only            boolean NOT NULL DEFAULT false,
    offline_enabled      boolean NOT NULL DEFAULT false,
    max_cache_size_mb    integer NOT NULL DEFAULT 10240 CHECK (max_cache_size_mb >= 0),
    share_live_listening boolean NOT NULL DEFAULT true
);
