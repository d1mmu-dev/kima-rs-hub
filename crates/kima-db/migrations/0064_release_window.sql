-- M5b Task 15: what the release window reads about the server (upstream `SystemSettings.createdAt` and
-- `SystemSettings.albumsRefiledAt`, `services/releases/index.ts`).
--
-- `created_at`: when this server was set up. The singleton row was made by migration 0001 when the server was
-- installed, but kept no time; an existing server takes the time its first account was made (onboarding), else now.
-- A fresh install runs every migration at setup, so the default is its setup time.
ALTER TABLE system_settings ADD COLUMN created_at timestamptz;
UPDATE system_settings SET created_at = COALESCE((SELECT min(created_at) FROM users), now());
ALTER TABLE system_settings ALTER COLUMN created_at SET DEFAULT now(), ALTER COLUMN created_at SET NOT NULL;

-- `albums_refiled_at`: the last album re-file that finished with every file read (upstream `refileProcessor.ts`
-- stamps it only when no album failed); null until then.
ALTER TABLE system_settings ADD COLUMN albums_refiled_at timestamptz;
