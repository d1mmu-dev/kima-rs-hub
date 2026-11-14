-- Test asset, NOT a migration: reverses 0060_profiles.sql while only main profiles hold
-- data (a main profile's id is its account's id, so every row maps back unchanged).
-- Production rollback is the `pg_dump -Fc` taken before the upgrade; this script exists so
-- `migrations_m_prof::down_script_restores_0039_schema` can prove the upgrade is reversible.
-- Refuses when a non-main profile owns a row, unless run after `SET kima.drop_profiles = 'on'`
-- (then those rows are deleted with their profiles).

DO $$
DECLARE
    t text;
    owned boolean;
BEGIN
    IF coalesce(current_setting('kima.drop_profiles', true), '') <> 'on' THEN
        FOREACH t IN ARRAY ARRAY['plays', 'playback_state', 'playlists', 'hidden_playlists',
            'api_keys', 'subsonic_bookmarks', 'stars', 'ratings', 'user_settings',
            'play_audit_ignore', 'podcast_subscriptions', 'podcast_progress', 'podcast_downloads']
        LOOP
            EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I x JOIN profiles p ON p.id = x.profile_id '
                           'WHERE p.kind <> ''main'')', t) INTO owned;
            IF owned THEN
                RAISE EXCEPTION 'non-main profile owns rows in %', t;
            END IF;
        END LOOP;
    END IF;
    DELETE FROM profiles WHERE kind <> 'main';
END
$$;

DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['plays', 'playback_state', 'playlists', 'hidden_playlists',
        'api_keys', 'subsonic_bookmarks', 'stars', 'ratings', 'user_settings',
        'play_audit_ignore', 'podcast_subscriptions', 'podcast_progress', 'podcast_downloads']
    LOOP
        EXECUTE format('ALTER TABLE %I DROP CONSTRAINT %I', t, t || '_profile_id_fkey');
        EXECUTE format('ALTER TABLE %I RENAME COLUMN profile_id TO user_id', t);
        EXECUTE format('ALTER TABLE %I ADD CONSTRAINT %I FOREIGN KEY (user_id) '
                       'REFERENCES users (id) ON DELETE CASCADE', t, t || '_user_id_fkey');
    END LOOP;
END
$$;

DROP TRIGGER users_main_profile ON users;
DROP FUNCTION users_main_profile();
DROP TABLE profiles;
DROP FUNCTION profiles_limit();
DROP FUNCTION profiles_keep_main();
DROP FUNCTION profiles_main_name(text);
DELETE FROM _sqlx_migrations WHERE version = 60;
