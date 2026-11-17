-- Task 4 (M-SYNC, D-163): a playlist the person wants shown newest first. Display only: the
-- stored order (`sort`) and the move rule do not change.
ALTER TABLE playlists ADD COLUMN reverse_order boolean NOT NULL DEFAULT false;
