-- CLAP persistence only: final musicnn/DSP result contracts are not defined yet.
CREATE EXTENSION IF NOT EXISTS vector;

ALTER TABLE tracks ADD COLUMN ml_generation bigint NOT NULL DEFAULT 0 CHECK (ml_generation >= 0);

CREATE TABLE track_embeddings (
    track_id uuid PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    file_generation bigint NOT NULL CHECK (file_generation >= 0),
    model_id text NOT NULL CHECK (btrim(model_id) <> ''),
    model_sha256 bytea NOT NULL CHECK (octet_length(model_sha256) = 32),
    pipeline_version text NOT NULL CHECK (btrim(pipeline_version) <> ''),
    -- Same tolerance as ClapEmbedding::from_normalized; never silently normalize here.
    embedding vector(512) NOT NULL CHECK (abs(vector_norm(embedding) - 1.0) <= 0.001),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX track_embeddings_cosine_idx ON track_embeddings USING hnsw (embedding vector_cosine_ops);

-- All writers (scanner, restore/repoint, organizer, autotag) pass through this fence.
-- Conservatively invalidate on metadata identity changes, including missing -> restored.
CREATE FUNCTION invalidate_track_ml() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(OLD.path, OLD.size, OLD.mtime, OLD.fingerprint, OLD.missing_since, OLD.ml_generation)
       IS DISTINCT FROM
       ROW(NEW.path, NEW.size, NEW.mtime, NEW.fingerprint, NEW.missing_since, NEW.ml_generation) THEN
        NEW.ml_generation := OLD.ml_generation + 1;
        DELETE FROM track_embeddings WHERE track_id = OLD.id;
        UPDATE track_stages SET status='pending', attempts=0, redelivered=0,
            next_attempt_at=NULL, started_at=NULL, finished_at=NULL,
            last_error=NULL, error_code=NULL, error_class=NULL, source=NULL,
            evidence='{"reason":"file changed"}'::jsonb, updated_at=clock_timestamp()
        WHERE track_id=OLD.id AND stage IN ('analysis', 'embedding', 'similarity');
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tracks_ml_identity BEFORE UPDATE ON tracks
    FOR EACH ROW EXECUTE FUNCTION invalidate_track_ml();
