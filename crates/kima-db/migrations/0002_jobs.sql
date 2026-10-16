CREATE TABLE jobs (
    id           uuid PRIMARY KEY,
    queue        text NOT NULL,
    payload      jsonb NOT NULL,
    state        text NOT NULL DEFAULT 'queued'
                 CHECK (state IN ('queued', 'running', 'done', 'failed')),
    attempts     integer NOT NULL DEFAULT 0,
    max_attempts integer NOT NULL DEFAULT 5 CHECK (max_attempts >= 1),
    run_at       timestamptz NOT NULL DEFAULT now(),
    locked_until timestamptz,
    last_error   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz
);

CREATE INDEX jobs_claimable ON jobs (queue, run_at) WHERE state IN ('queued', 'running');
