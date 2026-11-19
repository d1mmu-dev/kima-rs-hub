-- M5b (mixes, radio, what's-new): the saved mood mix, the mix id of a saved mix playlist, the
-- release acknowledgements and choices, and the mood tags the moods phase writes. Everything a
-- listener owns is keyed by the profile (M-PROF), where upstream keys it by the user.

-- Upstream `UserMoodMix` (schema.prisma, one per user). No surrogate id: it is never exposed.
-- The track ids are not foreign keys; a mix is re-read through the live tracks, as upstream.
CREATE TABLE user_mood_mix (
    profile_id       uuid PRIMARY KEY REFERENCES profiles (id) ON DELETE CASCADE,
    mood             text NOT NULL,
    track_ids        uuid[] NOT NULL,
    cover_urls       text[] NOT NULL,
    served_track_ids uuid[] NOT NULL,
    generated_at     timestamptz NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- Upstream `Playlist.mixId` with `@@unique([userId, mixId])`: a mix is saved once per listener.
ALTER TABLE playlists ADD COLUMN mix_id text;
CREATE UNIQUE INDEX playlists_profile_mix_idx ON playlists (profile_id, mix_id)
    WHERE mix_id IS NOT NULL;

-- Upstream `ReleaseAcknowledgement`: a listener dismissed a release window.
CREATE TABLE release_acknowledgements (
    profile_id   uuid NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    version      text NOT NULL,
    dismissed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (profile_id, version)
);

-- Upstream `ReleaseChoice` (`@@unique([userId, key])`, enum `ChoiceDecidedBy` as checked text).
CREATE TABLE release_choices (
    profile_id uuid NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    key        text NOT NULL,
    version    text NOT NULL,
    enabled    boolean NOT NULL DEFAULT false,
    decided_by text NOT NULL DEFAULT 'DISMISSED' CHECK (decided_by IN ('EXPLICIT', 'DISMISSED')),
    decided_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (profile_id, key)
);

-- Upstream `Track.moodTags`, written by the moods phase (M5b Task 5).
ALTER TABLE tracks ADD COLUMN mood_tags text[] NOT NULL DEFAULT '{}';
