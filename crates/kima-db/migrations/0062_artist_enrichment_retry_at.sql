-- When a `failed` artist is due again (upstream `artistEnrichment.ts`, `unifiedEnrichment.ts`): a run whose
-- source did not answer completes and is asked again on the next cycle (NULL: due now); only kima-rs's
-- whole-run time cap and the album-cover retry wait out the 15 min grace (`FAILED_JOB_RETRY_GRACE_MS`, which
-- upstream applies to a thrown run's failed job). Rows already `failed` keep the grace they were given.
ALTER TABLE artists ADD COLUMN enrichment_retry_at timestamptz;

UPDATE artists SET enrichment_retry_at = last_enriched_at + interval '15 minutes'
 WHERE enrichment_status = 'failed' AND last_enriched_at IS NOT NULL;
