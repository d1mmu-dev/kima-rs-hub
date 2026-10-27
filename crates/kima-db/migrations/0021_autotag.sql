CREATE TABLE autotag_albums (
    album_id         uuid PRIMARY KEY REFERENCES albums (id) ON DELETE CASCADE,
    state            text NOT NULL CHECK (state IN ('queued', 'searching', 'proposed', 'no_candidates',
                                                    'failed', 'applying', 'applied', 'skipped')),
    recommendation   text CHECK (recommendation IN ('strong', 'medium', 'none')),
    best_candidate   uuid,                  -- FK added below (the tables reference each other)
    chosen_candidate uuid,
    error            text,
    searched_at      timestamptz,
    applied_at       timestamptz,
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX autotag_albums_state_idx ON autotag_albums (state, recommendation);

CREATE TABLE autotag_candidates (
    id                 uuid PRIMARY KEY,
    album_id           uuid NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    release_mbid       uuid NOT NULL,
    release_group_mbid uuid NOT NULL,
    origin             text NOT NULL CHECK (origin IN ('tag', 'search', 'recording', 'manual')),
    distance           double precision NOT NULL CHECK (distance BETWEEN 0 AND 1),
    recommendation     text NOT NULL CHECK (recommendation IN ('strong', 'medium', 'none')),
    penalties          jsonb NOT NULL,   -- {"album":0.0,"artist":0.02,"year":0.0,"tracks":0.01,…} weighted shares
    mapping            jsonb NOT NULL,   -- [{trackId, medium, position, releaseTrackMbid, distance}]
    extra_tracks       jsonb NOT NULL,   -- local track ids with no release track
    missing_tracks     jsonb NOT NULL,   -- release tracks with no local track: [{medium, position, title, length}]
    release            jsonb NOT NULL,   -- the release as fetched (§19.2)
    release_group      jsonb NOT NULL,   -- snapshot: {id, title, primaryType, firstReleaseDate, genres:[{name,count}]}
    fetched_at         timestamptz NOT NULL DEFAULT now(),  -- snapshot age (§19.2, R3)
    created_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (album_id, release_mbid)
);
ALTER TABLE autotag_albums
    ADD FOREIGN KEY (best_candidate)   REFERENCES autotag_candidates (id) ON DELETE SET NULL,
    ADD FOREIGN KEY (chosen_candidate) REFERENCES autotag_candidates (id) ON DELETE SET NULL;

-- Album identity set by the autotagger (§19.7, R1).
ALTER TABLE albums
    ADD COLUMN identity_source  text NOT NULL DEFAULT 'scan' CHECK (identity_source IN ('scan', 'autotag')),
    ADD COLUMN applied_identity jsonb,     -- {artistKey, titleKey, year} while identity_source = 'autotag'
    ADD CHECK ((identity_source = 'autotag') = (applied_identity IS NOT NULL));

CREATE TABLE autotag_runs (
    id               uuid PRIMARY KEY,
    album_id         uuid NOT NULL REFERENCES albums (id) ON DELETE CASCADE,  -- stable across apply/undo (R1)
    candidate_id     uuid REFERENCES autotag_candidates (id) ON DELETE SET NULL,  -- display only; apply copies it
    applied          jsonb,          -- the §19.6 values used, copied from the candidate at apply (undo needs no candidate)
    album_before     jsonb,          -- the album row's identity fields, MBIDs and sources before apply
    artist_action    text CHECK (artist_action IN ('none', 'reparented', 'created', 'renamed')),
    old_artist_id    uuid REFERENCES artists (id) ON DELETE SET NULL,
    new_artist_id    uuid REFERENCES artists (id) ON DELETE SET NULL,
    old_artist_name  text,           -- for undoing a rename in place
    target           text NOT NULL CHECK (target IN ('files', 'copy', 'db')),
    reorganize       boolean NOT NULL DEFAULT false,
    state            text NOT NULL CHECK (state IN ('queued', 'writing', 'organizing', 'done', 'failed',
                                                    'undoing', 'undone', 'undo_failed')),
    organizer_run_id uuid REFERENCES organizer_runs (id) ON DELETE SET NULL,
    copy_run_id      uuid REFERENCES organizer_runs (id) ON DELETE SET NULL,  -- copy target's run
    created_by       uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    finished_at      timestamptz,
    summary          jsonb,
    error            text
);

CREATE TABLE autotag_journal (
    id          bigserial PRIMARY KEY,
    run_id      uuid NOT NULL REFERENCES autotag_runs (id) ON DELETE CASCADE,
    track_id    uuid REFERENCES tracks (id) ON DELETE SET NULL,
    root        text NOT NULL CHECK (root IN ('library', 'copy')),
    path        text NOT NULL,                  -- relative to that root
    before_sha  bytea,                          -- whole file, before (NULL for target 'db')
    after_sha   bytea,                          -- whole file, after the write
    audio_md5   bytea,                          -- §19.8; equal before and after by construction
    old_tags    jsonb NOT NULL,                 -- native key -> [values] for every touched key; [] = absent
    new_tags    jsonb NOT NULL,
    id3_version text CHECK (id3_version IN ('v3', 'v4')),  -- the file's ID3v2 version before the write (R4)
    state       text NOT NULL CHECK (state IN ('planned', 'written', 'skipped', 'failed', 'undone',
                                               'undo_failed')),
    note        text,
    applied_seq bigint
);
CREATE SEQUENCE autotag_applied_seq;
CREATE INDEX autotag_journal_run_idx ON autotag_journal (run_id, applied_seq);

-- Database-only matches survive rescans: the scanner merges these over the file's tags (§19.7).
CREATE TABLE track_tag_overrides (
    track_id   uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    run_id     uuid NOT NULL REFERENCES autotag_runs (id) ON DELETE CASCADE,
    fields     jsonb NOT NULL,                  -- the RawTags fields to replace, camelCase (§19.6)
    created_at timestamptz NOT NULL DEFAULT now()
);
