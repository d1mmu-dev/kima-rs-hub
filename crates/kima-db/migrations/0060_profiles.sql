-- Profiles per account (M-PROF, D-240). An account (`users` row) holds up to 5 profiles:
-- exactly one `main`, any mix of `kids` / `standard`. Every personal table moves from
-- `user_id -> users(id)` to `profile_id -> profiles(id)`.
--
-- The main profile's id IS the account id (CHECK below), so this upgrade rewrites no data
-- row: every stored user id, JWT, Subsonic session and queued job payload already names the
-- account's main profile. The re-key is a column rename plus an FK swap.

CREATE TABLE profiles (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       text NOT NULL CHECK (name = btrim(name) AND length(name) BETWEEN 1 AND 50),
    kind       text NOT NULL CHECK (kind IN ('main', 'kids', 'standard')),
    -- A preset key (colour/icon); NULL = initial on a colour derived from the id.
    avatar     text CHECK (avatar ~ '^[a-z0-9-]{1,32}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    -- The main profile is the account: same id. With the primary key this gives exactly
    -- one main profile per account.
    CONSTRAINT profiles_main_is_account CHECK ((kind = 'main') = (id = user_id))
);
-- Names are unique per account regardless of case ("Kids" and "kids" are the same).
CREATE UNIQUE INDEX profiles_user_name_key ON profiles (user_id, lower(name));
CREATE INDEX profiles_user_idx ON profiles (user_id);

-- The main profile's name: the username (at most 50 characters, trimmed by the app), with a
-- fallback for rows written by SQL that the app would have refused.
CREATE FUNCTION profiles_main_name(username text) RETURNS text
    LANGUAGE sql IMMUTABLE AS
$$ SELECT coalesce(nullif(btrim(left(btrim(username), 50)), ''), 'Main') $$;

-- Backfill: one main profile per existing account, named after the account.
INSERT INTO profiles (id, user_id, name, kind, created_at)
SELECT id, id, profiles_main_name(username), 'main', created_at FROM users;

-- At most 5 profiles per account, race-free: lock the account row (the same lock
-- `api_keys::create` takes), then count. A concurrent insert waits for the lock and then
-- counts the committed rows (READ COMMITTED: a fresh snapshot per statement).
CREATE FUNCTION profiles_limit() RETURNS trigger
    LANGUAGE plpgsql AS
$$
BEGIN
    PERFORM 1 FROM users WHERE id = NEW.user_id FOR UPDATE;
    IF (SELECT count(*) FROM profiles WHERE user_id = NEW.user_id) >= 5 THEN
        RAISE EXCEPTION 'profile limit reached'
            USING ERRCODE = 'P0001', HINT = 'kima:profile_limit';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER profiles_limit BEFORE INSERT ON profiles
    FOR EACH ROW EXECUTE FUNCTION profiles_limit();

-- Every new account gets its main profile in the same statement (register, create-user,
-- Subsonic createUser and SQL fixtures alike: no code path can forget it).
CREATE FUNCTION users_main_profile() RETURNS trigger
    LANGUAGE plpgsql AS
$$
BEGIN
    INSERT INTO profiles (id, user_id, name, kind, avatar, created_at)
    VALUES (NEW.id, NEW.id, profiles_main_name(NEW.username), 'main', NULL, NEW.created_at);
    RETURN NULL;
END
$$;
CREATE TRIGGER users_main_profile AFTER INSERT ON users
    FOR EACH ROW EXECUTE FUNCTION users_main_profile();

-- The main profile goes only with its account: deleting the account cascades here after
-- the `users` row is gone, so the check lets that delete through.
CREATE FUNCTION profiles_keep_main() RETURNS trigger
    LANGUAGE plpgsql AS
$$
BEGIN
    IF OLD.kind = 'main' AND EXISTS (SELECT 1 FROM users WHERE id = OLD.user_id) THEN
        RAISE EXCEPTION 'main profile cannot be deleted'
            USING ERRCODE = 'P0001', HINT = 'kima:profile_main';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER profiles_keep_main BEFORE DELETE ON profiles
    FOR EACH ROW EXECUTE FUNCTION profiles_keep_main();

-- Re-key the 13 personal tables. FK names are the Postgres defaults for inline
-- `REFERENCES` (asserted at version 39 by `upgrade_preserves_fk_names_assumption`).
-- Index names keep their `user` wording; the columns inside follow the rename.
ALTER TABLE plays RENAME COLUMN user_id TO profile_id;
ALTER TABLE plays DROP CONSTRAINT plays_user_id_fkey,
    ADD CONSTRAINT plays_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE playback_state RENAME COLUMN user_id TO profile_id;
ALTER TABLE playback_state DROP CONSTRAINT playback_state_user_id_fkey,
    ADD CONSTRAINT playback_state_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE playlists RENAME COLUMN user_id TO profile_id;
ALTER TABLE playlists DROP CONSTRAINT playlists_user_id_fkey,
    ADD CONSTRAINT playlists_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE hidden_playlists RENAME COLUMN user_id TO profile_id;
ALTER TABLE hidden_playlists DROP CONSTRAINT hidden_playlists_user_id_fkey,
    ADD CONSTRAINT hidden_playlists_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE api_keys RENAME COLUMN user_id TO profile_id;
ALTER TABLE api_keys DROP CONSTRAINT api_keys_user_id_fkey,
    ADD CONSTRAINT api_keys_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE subsonic_bookmarks RENAME COLUMN user_id TO profile_id;
ALTER TABLE subsonic_bookmarks DROP CONSTRAINT subsonic_bookmarks_user_id_fkey,
    ADD CONSTRAINT subsonic_bookmarks_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE stars RENAME COLUMN user_id TO profile_id;
ALTER TABLE stars DROP CONSTRAINT stars_user_id_fkey,
    ADD CONSTRAINT stars_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE ratings RENAME COLUMN user_id TO profile_id;
ALTER TABLE ratings DROP CONSTRAINT ratings_user_id_fkey,
    ADD CONSTRAINT ratings_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE user_settings RENAME COLUMN user_id TO profile_id;
ALTER TABLE user_settings DROP CONSTRAINT user_settings_user_id_fkey,
    ADD CONSTRAINT user_settings_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE play_audit_ignore RENAME COLUMN user_id TO profile_id;
ALTER TABLE play_audit_ignore DROP CONSTRAINT play_audit_ignore_user_id_fkey,
    ADD CONSTRAINT play_audit_ignore_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE podcast_subscriptions RENAME COLUMN user_id TO profile_id;
ALTER TABLE podcast_subscriptions DROP CONSTRAINT podcast_subscriptions_user_id_fkey,
    ADD CONSTRAINT podcast_subscriptions_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE podcast_progress RENAME COLUMN user_id TO profile_id;
ALTER TABLE podcast_progress DROP CONSTRAINT podcast_progress_user_id_fkey,
    ADD CONSTRAINT podcast_progress_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;

ALTER TABLE podcast_downloads RENAME COLUMN user_id TO profile_id;
ALTER TABLE podcast_downloads DROP CONSTRAINT podcast_downloads_user_id_fkey,
    ADD CONSTRAINT podcast_downloads_profile_id_fkey FOREIGN KEY (profile_id)
        REFERENCES profiles (id) ON DELETE CASCADE;
