CREATE TABLE api_keys (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    key_hash     bytea NOT NULL UNIQUE CHECK (length(key_hash) = 32),   -- sha256(token)
    secret_enc   bytea NOT NULL,                                       -- spec §5.2
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz                                           -- null until first use
);
CREATE INDEX api_keys_user_idx ON api_keys (user_id);
