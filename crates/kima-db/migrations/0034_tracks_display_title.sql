-- Task 17 (M-SYNC Task 17, D-170): the title a person edited in the MetadataEditor; empty
-- means the file's own title. Shown through Subsonic as `title` (COALESCE), because
-- `displayTitle` is stored on the same row as `displayTitle` in upstream.
ALTER TABLE tracks ADD COLUMN display_title text;
