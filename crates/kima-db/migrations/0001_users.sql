CREATE TABLE users (
    id                  uuid PRIMARY KEY,
    username            text NOT NULL,
    password_hash       text NOT NULL,
    role                text NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    onboarding_complete boolean NOT NULL DEFAULT false,
    token_version       integer NOT NULL DEFAULT 0,
    created_at          timestamptz NOT NULL DEFAULT now()
);

-- Usernames are unique regardless of case: "Ana" and "ana" are the same person.
CREATE UNIQUE INDEX users_username_lower_key ON users (lower(username));

-- Exactly one row (id is always true).
CREATE TABLE system_settings (
    id                boolean PRIMARY KEY DEFAULT true CHECK (id),
    registration_open boolean NOT NULL DEFAULT false
);
INSERT INTO system_settings DEFAULT VALUES;
