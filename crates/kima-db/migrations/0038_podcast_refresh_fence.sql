-- Refresh fence and attempt bookkeeping for followed podcast feeds:
-- refresh_token fences in-flight refreshes (see kima-podcast::refresh);
-- refresh_attempted_at records the dispatch attempt so the sweep's hourly
-- backoff can read it via GREATEST(last_refreshed, refresh_attempted_at).
-- IF NOT EXISTS keeps the migration re-runnable against an already-applied 0038.
ALTER TABLE podcasts ADD COLUMN IF NOT EXISTS refresh_token uuid;
ALTER TABLE podcasts ADD COLUMN IF NOT EXISTS refresh_attempted_at timestamptz;
