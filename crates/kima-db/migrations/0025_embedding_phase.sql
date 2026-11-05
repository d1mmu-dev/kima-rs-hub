-- The coordinator's embedding phase (M4): a new phase inside the existing enrich cycle,
-- independent of the MusicBrainz on/off switch, so it needs its own `current_phase` value.
ALTER TABLE enrichment_state DROP CONSTRAINT enrichment_state_current_phase_check;
ALTER TABLE enrichment_state ADD CONSTRAINT enrichment_state_current_phase_check
    CHECK (current_phase IN ('artists', 'tracks', 'fingerprint', 'embedding'));
