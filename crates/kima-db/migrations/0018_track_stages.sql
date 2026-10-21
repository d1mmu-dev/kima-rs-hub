CREATE TABLE track_stages (
    track_id        uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    stage           text NOT NULL CHECK (stage IN
                        ('fingerprint', 'recording', 'analysis', 'embedding', 'similarity')),
    status          text NOT NULL CHECK (status IN ('pending', 'working', 'done', 'failed')),
    attempts        smallint NOT NULL DEFAULT 0,  -- reported failures; rate limits never count
    redelivered     smallint NOT NULL DEFAULT 0,  -- lapsed claims re-offered
    source          text,                         -- ffmpeg | file_tag | tracklist | isrc | search | acoustid
    last_error      text,                         -- raw message, <= 2000 chars
    error_code      text,                         -- TIMEOUT, HTTP_503, RATE_LIMITED, NO_CLIENT_KEY, LOST_WORKER, …
    error_class     text CHECK (error_class IN ('DAMAGED', 'UNSUPPORTED', 'MULTICANNEL', 'INCONCLUSIVE',
                                                'PERMANENT', 'FILE_MISSING', 'DEPENDENCY_CLOSED')),
    evidence        jsonb,
    next_attempt_at timestamptz,
    started_at      timestamptz,
    finished_at     timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (track_id, stage)
);
CREATE INDEX track_stages_due_idx ON track_stages (stage, status, next_attempt_at);

-- The coordinator's control state (upstream's Redis key enrichment:state). One row.
CREATE TABLE enrichment_state (
    id            boolean PRIMARY KEY DEFAULT true CHECK (id),
    status        text NOT NULL DEFAULT 'idle' CHECK (status IN ('idle', 'running', 'paused', 'stopping')),
    halted        boolean NOT NULL DEFAULT false,   -- set by stop, cleared by sync (§10.7)
    current_phase text CHECK (current_phase IN ('artists', 'tracks', 'fingerprint')),
    started_at    timestamptz,
    paused_at     timestamptz,
    stopped_at    timestamptz,
    last_error    text,
    last_activity timestamptz NOT NULL DEFAULT now()
);
INSERT INTO enrichment_state DEFAULT VALUES;
