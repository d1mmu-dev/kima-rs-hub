-- Album-cover retries of a completed artist get their own count (was a negative
-- `enrichment_failures`, D-113).
ALTER TABLE artists ADD COLUMN cover_deferrals smallint NOT NULL DEFAULT 0 CHECK (cover_deferrals >= 0);
UPDATE artists SET cover_deferrals = -enrichment_failures, enrichment_failures = 0 WHERE enrichment_failures < 0;
ALTER TABLE artists ADD CHECK (enrichment_failures >= 0);
