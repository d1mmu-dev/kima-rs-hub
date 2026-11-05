-- Stored MusiCNN + DSP analysis result (bpm/key/scale, LUFS/LRA, MusiCNN
-- per-track aggregate). Standalone schema slice: no attempt.rs/scheduler
-- wiring, no real-model/Essentia parity gate. See D-151/D-152 (MusiCNN) and
-- D-008/D-153 (DSP key/tempo/loudness) in docs/DIVERGENCES.md for the
-- grounding of every field below; this migration adds no new algorithm
-- decision, only persistence for already-accepted compute contracts.
--
-- One row per track, keyed like track_embeddings (0024_ml_analysis.sql).
-- Unlike track_embeddings (one model, one vector, all-or-nothing), the
-- "analysis" stage covers four independently-computable results that can
-- each succeed or fail on their own within the same shared-decode attempt
-- (grounded in the owner's shared-decode design, D-009, and this task's
-- explicit requirement that DSP key/tempo may fail independently of
-- MusiCNN): the MusiCNN per-track aggregate (D-151/D-152), DSP loudness
-- (LUFS/LRA), DSP key (tonality) and DSP tempo (BPM). Each group gets its
-- own nullable column set, its own pipeline_version and its own
-- computed_at, so a partial result (e.g. MusiCNN succeeded, tempo failed
-- because the tail was too short for the autocorrelation lag range) is
-- representable without inventing a placeholder value for the group that
-- did not complete. A CHECK constraint per group enforces "all columns in
-- the group are set, or none are" so application code cannot write a
-- half-populated group by mistake.
--
-- file_generation is one column for the whole row (not per group): all
-- four results come from the same per-attempt shared decode of the same
-- file bytes (D-009), so they share one generation fence. invalidate_track_ml()
-- (originally 0024_ml_analysis.sql, replaced here) now also deletes the
-- whole row on any tracked identity change, exactly like track_embeddings;
-- this guarantees a row can never mix columns from two different
-- generations — a write for a new generation always starts from no row (or
-- a row already deleted by the trigger), never from stale leftover groups.
CREATE TABLE track_analysis (
    track_id                  uuid PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    file_generation           bigint NOT NULL CHECK (file_generation >= 0),

    -- MusiCNN per-track aggregate (D-151 compute, D-152 aggregation).
    -- Values are already upstream's round(clamp(mean(...), 0, 1), 3); no
    -- further scaling/rounding should happen on read. `danceability`
    -- dedupes upstream's `danceability`/`danceabilityMl` (same value, two
    -- Python columns); not divergent, just not duplicated here.
    musicnn_pipeline_version  text CHECK (musicnn_pipeline_version IS NULL OR btrim(musicnn_pipeline_version) <> ''),
    musicnn_danceability      real CHECK (musicnn_danceability BETWEEN 0 AND 1),
    musicnn_valence           real CHECK (musicnn_valence BETWEEN 0 AND 1),
    musicnn_arousal           real CHECK (musicnn_arousal BETWEEN 0 AND 1),
    musicnn_mood_happy        real CHECK (musicnn_mood_happy BETWEEN 0 AND 1),
    musicnn_mood_sad          real CHECK (musicnn_mood_sad BETWEEN 0 AND 1),
    musicnn_mood_relaxed      real CHECK (musicnn_mood_relaxed BETWEEN 0 AND 1),
    musicnn_mood_aggressive   real CHECK (musicnn_mood_aggressive BETWEEN 0 AND 1),
    musicnn_mood_party        real CHECK (musicnn_mood_party BETWEEN 0 AND 1),
    musicnn_mood_acoustic     real CHECK (musicnn_mood_acoustic BETWEEN 0 AND 1),
    musicnn_mood_electronic   real CHECK (musicnn_mood_electronic BETWEEN 0 AND 1),
    musicnn_computed_at       timestamptz,

    -- DSP loudness (D-153: ebur128 crate, EBU R128/Tech 3341+3342, exact
    -- standard match to upstream's LoudnessEBUR128, library-only divergence).
    dsp_loudness_pipeline_version text CHECK (dsp_loudness_pipeline_version IS NULL OR btrim(dsp_loudness_pipeline_version) <> ''),
    integrated_lufs           double precision,
    loudness_range_lu         double precision CHECK (loudness_range_lu IS NULL OR loudness_range_lu >= 0),
    loudness_computed_at      timestamptz,

    -- DSP key/tonality (D-008 accepted-with-params: chroma + Krumhansl-Schmuckler).
    dsp_key_pipeline_version  text CHECK (dsp_key_pipeline_version IS NULL OR btrim(dsp_key_pipeline_version) <> ''),
    key_tonic                 smallint CHECK (key_tonic BETWEEN 0 AND 11),  -- 0=C .. 11=B
    key_mode                  text CHECK (key_mode IN ('major', 'minor')),
    key_strength              double precision CHECK (key_strength BETWEEN -1 AND 1),  -- Pearson correlation
    key_computed_at           timestamptz,

    -- DSP tempo/BPM (D-008 accepted-with-params: onset-strength + autocorrelation/comb).
    dsp_tempo_pipeline_version text CHECK (dsp_tempo_pipeline_version IS NULL OR btrim(dsp_tempo_pipeline_version) <> ''),
    tempo_bpm                 double precision CHECK (tempo_bpm > 0 AND tempo_bpm <= 400),
    tempo_confidence          double precision CHECK (tempo_confidence BETWEEN 0 AND 1),
    tempo_computed_at         timestamptz,

    created_at                timestamptz NOT NULL DEFAULT now(),
    updated_at                timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT track_analysis_musicnn_all_or_nothing CHECK (
        (musicnn_pipeline_version IS NULL AND musicnn_danceability IS NULL AND musicnn_valence IS NULL
            AND musicnn_arousal IS NULL AND musicnn_mood_happy IS NULL AND musicnn_mood_sad IS NULL
            AND musicnn_mood_relaxed IS NULL AND musicnn_mood_aggressive IS NULL AND musicnn_mood_party IS NULL
            AND musicnn_mood_acoustic IS NULL AND musicnn_mood_electronic IS NULL AND musicnn_computed_at IS NULL)
        OR
        (musicnn_pipeline_version IS NOT NULL AND musicnn_danceability IS NOT NULL AND musicnn_valence IS NOT NULL
            AND musicnn_arousal IS NOT NULL AND musicnn_mood_happy IS NOT NULL AND musicnn_mood_sad IS NOT NULL
            AND musicnn_mood_relaxed IS NOT NULL AND musicnn_mood_aggressive IS NOT NULL AND musicnn_mood_party IS NOT NULL
            AND musicnn_mood_acoustic IS NOT NULL AND musicnn_mood_electronic IS NOT NULL AND musicnn_computed_at IS NOT NULL)
    ),
    CONSTRAINT track_analysis_loudness_all_or_nothing CHECK (
        (dsp_loudness_pipeline_version IS NULL AND integrated_lufs IS NULL AND loudness_range_lu IS NULL AND loudness_computed_at IS NULL)
        OR
        (dsp_loudness_pipeline_version IS NOT NULL AND integrated_lufs IS NOT NULL AND loudness_range_lu IS NOT NULL AND loudness_computed_at IS NOT NULL)
    ),
    CONSTRAINT track_analysis_key_all_or_nothing CHECK (
        (dsp_key_pipeline_version IS NULL AND key_tonic IS NULL AND key_mode IS NULL AND key_strength IS NULL AND key_computed_at IS NULL)
        OR
        (dsp_key_pipeline_version IS NOT NULL AND key_tonic IS NOT NULL AND key_mode IS NOT NULL AND key_strength IS NOT NULL AND key_computed_at IS NOT NULL)
    ),
    CONSTRAINT track_analysis_tempo_all_or_nothing CHECK (
        (dsp_tempo_pipeline_version IS NULL AND tempo_bpm IS NULL AND tempo_confidence IS NULL AND tempo_computed_at IS NULL)
        OR
        (dsp_tempo_pipeline_version IS NOT NULL AND tempo_bpm IS NOT NULL AND tempo_confidence IS NOT NULL AND tempo_computed_at IS NOT NULL)
    )
);

-- Same identity fence as track_embeddings: only rows whose file_generation
-- still matches the track's current ml_generation are a current result.
CREATE INDEX track_analysis_generation_idx ON track_analysis (track_id, file_generation);

-- Extend 0024_ml_analysis.sql's invalidate_track_ml() so a tracked identity
-- change also drops any stored analysis result, exactly like
-- track_embeddings, instead of leaving a now-orphaned row at the old
-- generation under the bumped ml_generation. The 'analysis' stage reset
-- (track_stages status back to 'pending') was already covered by the
-- existing stage list in 0024; nothing changes there.
CREATE OR REPLACE FUNCTION invalidate_track_ml() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(OLD.path, OLD.size, OLD.mtime, OLD.fingerprint, OLD.missing_since, OLD.ml_generation)
       IS DISTINCT FROM
       ROW(NEW.path, NEW.size, NEW.mtime, NEW.fingerprint, NEW.missing_since, NEW.ml_generation) THEN
        NEW.ml_generation := OLD.ml_generation + 1;
        DELETE FROM track_embeddings WHERE track_id = OLD.id;
        DELETE FROM track_analysis WHERE track_id = OLD.id;
        UPDATE track_stages SET status='pending', attempts=0, redelivered=0,
            next_attempt_at=NULL, started_at=NULL, finished_at=NULL,
            last_error=NULL, error_code=NULL, error_class=NULL, source=NULL,
            evidence='{"reason":"file changed"}'::jsonb, updated_at=clock_timestamp()
        WHERE track_id=OLD.id AND stage IN ('analysis', 'embedding', 'similarity');
    END IF;
    RETURN NEW;
END $$;
