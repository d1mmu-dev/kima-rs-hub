-- M6 Task 8 (D-200, D-207): AI-content labels. Kima-only; upstream has no AI concept.
-- `ai_feed` is the publisher's `podcast:txt purpose="ai-content"` tag (true/false, NULL = no
-- signal), at channel level on `podcasts` and item level on `podcast_episodes`. `ai_override`
-- is an admin's decision per show or per episode. The effective label is resolved in queries
-- (`kima_db::podcasts::{show_label_sql, episode_label_sql}`), never stored.
ALTER TABLE podcasts
    ADD COLUMN ai_feed boolean,
    ADD COLUMN ai_override text CHECK (ai_override IN ('ai', 'human')),
    ADD COLUMN ai_override_by uuid REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN ai_override_at timestamptz;

-- Feeds stored before this migration were never read for the tags, and a host that honours the
-- stored validators would answer every later refresh with 304, so drop them: the next refresh of
-- every existing show is one full read that stores its labels (D-206).
UPDATE podcasts SET feed_etag = NULL, feed_last_modified = NULL
 WHERE feed_etag IS NOT NULL OR feed_last_modified IS NOT NULL;

ALTER TABLE podcast_episodes
    ADD COLUMN ai_feed boolean,
    ADD COLUMN ai_override text CHECK (ai_override IN ('ai', 'human')),
    ADD COLUMN ai_override_by uuid REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN ai_override_at timestamptz;

-- Whether a user's lists hide AI-labelled shows and episodes (default) or show them.
ALTER TABLE user_settings
    ADD COLUMN podcast_ai_filter text NOT NULL DEFAULT 'hide'
        CHECK (podcast_ai_filter IN ('hide', 'show'));

-- Channel tags read by a preview for shows that are not stored, so discovery items (charts,
-- for-you, similar, Apple search) can carry a label without crawling feeds (spec S3).
CREATE TABLE podcast_feed_labels (
    itunes_id text PRIMARY KEY,
    ai_feed boolean,
    read_at timestamptz NOT NULL DEFAULT now()
);
