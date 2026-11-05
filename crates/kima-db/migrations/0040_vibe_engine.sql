-- M5 vibe engine: the matching state, per-track feel, the 3-D map, the learned moods and the CLAP readings.
-- Every table hangs off track_embeddings (track_id) with ON DELETE CASCADE, so a file change (which deletes the
-- embedding through invalidate_track_ml, 0024) drops the track's feel, map, mood and reading rows with it.
-- matching_state and mood_state are single-row tables; their row is also the lock that orders writers
-- (state row first, then feel/score/map rows, then track rows).

CREATE TABLE matching_feel (
    track_id    uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL,  -- track_embeddings.created_at this row was computed from
    probe       text        NOT NULL,
    valence     real        NOT NULL CHECK (valence BETWEEN 0 AND 1),
    arousal     real        NOT NULL CHECK (arousal BETWEEN 0 AND 1)
);

CREATE TABLE matching_state (
    id                  text PRIMARY KEY DEFAULT 'default' CHECK (id = 'default'),
    version             text NOT NULL,
    fingerprint         text NOT NULL,
    track_count         integer NOT NULL,
    feel_mean_valence   float8 NOT NULL,
    feel_mean_arousal   float8 NOT NULL,
    feel_sd_valence     float8 NOT NULL,
    feel_sd_arousal     float8 NOT NULL,
    sound_spread        float8 NOT NULL,
    genre_spread        float8 NOT NULL,
    genre_weight        float8 NOT NULL,
    genres              jsonb  NOT NULL,
    built_at            timestamptz NOT NULL DEFAULT now(),
    map_version         text,
    map_frame           jsonb,
    map_updated_at      timestamptz
);

CREATE TABLE matching_map (
    track_id    uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL,
    x           float8 NOT NULL,
    y           float8 NOT NULL,
    z           float8 NOT NULL,
    colour      float8 NOT NULL,
    laid_out    boolean NOT NULL
);

CREATE TABLE track_moods (
    track_id    uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at timestamptz NOT NULL,
    weights     text NOT NULL,  -- the mood heads' version these scores came from
    happy real NOT NULL,
    fun_quirky real NOT NULL,
    uplifting real NOT NULL,
    sad real NOT NULL,
    nostalgic real NOT NULL,
    dark real NOT NULL,
    trippy real NOT NULL,
    calm real NOT NULL,
    dreamy real NOT NULL,
    energetic real NOT NULL,
    aggressive real NOT NULL,
    epic real NOT NULL,
    party real NOT NULL,
    groovy real NOT NULL,
    romantic real NOT NULL,
    sexy real NOT NULL,
    summer real NOT NULL,
    night real NOT NULL,
    driving real NOT NULL,
    workout real NOT NULL,
    focus real NOT NULL,
    sleep real NOT NULL
);

CREATE TABLE mood_state (
    id          text PRIMARY KEY DEFAULT 'default' CHECK (id = 'default'),
    version     text NOT NULL,
    fingerprint text NOT NULL,
    track_count integer NOT NULL,
    computed_at timestamptz NOT NULL DEFAULT now(),
    happy_mean float8 NOT NULL,
    happy_sd float8 NOT NULL,
    fun_quirky_mean float8 NOT NULL,
    fun_quirky_sd float8 NOT NULL,
    uplifting_mean float8 NOT NULL,
    uplifting_sd float8 NOT NULL,
    sad_mean float8 NOT NULL,
    sad_sd float8 NOT NULL,
    nostalgic_mean float8 NOT NULL,
    nostalgic_sd float8 NOT NULL,
    dark_mean float8 NOT NULL,
    dark_sd float8 NOT NULL,
    trippy_mean float8 NOT NULL,
    trippy_sd float8 NOT NULL,
    calm_mean float8 NOT NULL,
    calm_sd float8 NOT NULL,
    dreamy_mean float8 NOT NULL,
    dreamy_sd float8 NOT NULL,
    energetic_mean float8 NOT NULL,
    energetic_sd float8 NOT NULL,
    aggressive_mean float8 NOT NULL,
    aggressive_sd float8 NOT NULL,
    epic_mean float8 NOT NULL,
    epic_sd float8 NOT NULL,
    party_mean float8 NOT NULL,
    party_sd float8 NOT NULL,
    groovy_mean float8 NOT NULL,
    groovy_sd float8 NOT NULL,
    romantic_mean float8 NOT NULL,
    romantic_sd float8 NOT NULL,
    sexy_mean float8 NOT NULL,
    sexy_sd float8 NOT NULL,
    summer_mean float8 NOT NULL,
    summer_sd float8 NOT NULL,
    night_mean float8 NOT NULL,
    night_sd float8 NOT NULL,
    driving_mean float8 NOT NULL,
    driving_sd float8 NOT NULL,
    workout_mean float8 NOT NULL,
    workout_sd float8 NOT NULL,
    focus_mean float8 NOT NULL,
    focus_sd float8 NOT NULL,
    sleep_mean float8 NOT NULL,
    sleep_sd float8 NOT NULL,
    -- A reading no eligible track has is absent (NULL), not 0: a recipe that blends it refuses on its own.
    arousal_mean float8,
    arousal_sd float8,
    instrumental_logit_mean float8,
    instrumental_logit_sd float8,
    acoustic_logit_mean float8,
    acoustic_logit_sd float8
);

-- CLAP zero-shot readings computed from the stored vector (spec 8.3).
CREATE TABLE track_clap_readings (
    track_id         uuid PRIMARY KEY REFERENCES track_embeddings (track_id) ON DELETE CASCADE,
    embedded_at      timestamptz NOT NULL,
    instrumentalness real NOT NULL,
    acousticness     real NOT NULL,
    valence_raw      real NOT NULL,
    arousal_raw      real NOT NULL
);

ALTER TABLE enrichment_state DROP CONSTRAINT enrichment_state_current_phase_check;
ALTER TABLE enrichment_state ADD CONSTRAINT enrichment_state_current_phase_check
    CHECK (current_phase IN ('artists', 'tracks', 'fingerprint', 'embedding', 'analysis', 'mood', 'similarity'));
