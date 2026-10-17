-- Library organizer (spec §5): one row per run, one journal row per file it plans to place.
-- Journal paths are relative: `from_path` to the run's `source_root`, `to_path` to its
-- `target_root` (the same folder for a move).

CREATE TABLE organizer_runs (
    id          uuid PRIMARY KEY,
    mode        text NOT NULL CHECK (mode IN ('copy', 'move')),
    -- planning -> previewed -> applying -> done | failed; done | failed -> undoing -> undone.
    state       text NOT NULL DEFAULT 'planning'
                CHECK (state IN ('planning', 'previewed', 'applying', 'done', 'failed',
                                 'undoing', 'undone')),
    source_root text NOT NULL,
    target_root text NOT NULL,
    created_by  uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    -- Set once the library root was switched to this (copy) run's target.
    switched_at timestamptz,
    summary     jsonb NOT NULL DEFAULT '{}'::jsonb,
    error       text
);
CREATE INDEX organizer_runs_created_idx ON organizer_runs (created_at DESC);

CREATE SEQUENCE organizer_applied_seq;

CREATE TABLE organizer_journal (
    id          bigserial PRIMARY KEY,
    run_id      uuid NOT NULL REFERENCES organizer_runs (id) ON DELETE CASCADE,
    -- NULL for a sidecar (cover image, .cue, .log) travelling with its album.
    track_id    uuid REFERENCES tracks (id) ON DELETE SET NULL,
    from_path   text NOT NULL,
    to_path     text NOT NULL,
    -- SHA-256 of the whole file, written before the file is touched.
    sha256      bytea,
    -- staged: parked under a temporary name in the target folder (swap cycles, case-only
    -- renames); failed: the copy did not verify, nothing was left at `to_path`; undo_staged:
    -- the same on the way back during an undo.
    state       text NOT NULL DEFAULT 'planned'
                CHECK (state IN ('planned', 'staged', 'done', 'skipped', 'failed', 'undo_staged',
                                 'undone')),
    note        text,
    collision   boolean NOT NULL DEFAULT false,
    -- Order in which rows reached `done`; undo walks it backwards.
    applied_seq bigint
);
CREATE INDEX organizer_journal_run_idx ON organizer_journal (run_id, id);
