-- D-251: Symphonia has no Opus decoder, so every Opus track failed `analysis` and `embedding` with
-- "unsupported feature: core (codec): unsupported audio codec" (INCONCLUSIVE, retries exhausted after four
-- attempts). The decoder now registers libopus; give exactly those rows one clean retry, once (a migration
-- runs once per database, so no later failure is ever reopened by it).
--
-- Non-destructive, like the D-194 transition: only `failed` rows of the two ML stages whose stored error is the
-- decoder's missing-codec error move back to `pending` with a clean slate; vectors, analysis results, derived
-- rows, other stages and every other failure stay untouched. Other codecs still unsupported fail again under the
-- normal retry schedule.
UPDATE track_stages
SET status = 'pending', attempts = 0, redelivered = 0, next_attempt_at = NULL,
    started_at = NULL, finished_at = NULL, last_error = NULL, error_code = NULL,
    error_class = NULL, source = NULL,
    evidence = '{"reason":"decoder gained the codec"}'::jsonb, updated_at = clock_timestamp()
WHERE stage IN ('analysis', 'embedding')
  AND status = 'failed'
  AND error_code IN ('DECODE_ERROR', 'CORRUPT_AUDIO', 'UNSUPPORTED_FORMAT')
  AND last_error LIKE '%unsupported audio codec%';
