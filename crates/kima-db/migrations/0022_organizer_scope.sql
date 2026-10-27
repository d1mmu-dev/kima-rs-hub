ALTER TABLE organizer_runs ADD COLUMN album_ids uuid[];   -- NULL = whole library (M1 behaviour)
ALTER TABLE organizer_runs ADD COLUMN origin text NOT NULL DEFAULT 'user'
    CHECK (origin IN ('user', 'autotag'));
ALTER TABLE organizer_runs DROP CONSTRAINT organizer_runs_state_check;   -- name as generated in 0008
ALTER TABLE organizer_runs ADD CONSTRAINT organizer_runs_state_check CHECK (state IN
    ('planning', 'previewed', 'applying', 'done', 'failed', 'undoing', 'undone', 'superseded'));
