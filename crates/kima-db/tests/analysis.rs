//! TDD for the standalone `track_analysis` schema/migration slice
//! (`crates/kima-db/migrations/0026_track_analysis.sql`,
//! `kima_db::analysis`). No attempt.rs/scheduler wiring, no real-model
//! parity gate exercised here (out of scope for this slice).
use kima_db::{PgPool, analysis};
use uuid::Uuid;

async fn track(pool: &PgPool) -> Uuid {
    let artist = Uuid::new_v4();
    let album = Uuid::new_v4();
    let track = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'a', $1::text)")
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $1::text)",
    )
    .bind(album)
    .bind(artist)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         VALUES ($1, $2, 'a', 't', 'flac', $1::text, 1, now(), '\\x00')",
    )
    .bind(track)
    .bind(album)
    .execute(pool)
    .await
    .unwrap();
    track
}

async fn generation(pool: &PgPool, track_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT ml_generation FROM tracks WHERE id=$1")
        .bind(track_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn musicnn() -> analysis::MusicnnAnalysis {
    analysis::MusicnnAnalysis {
        danceability: 0.5,
        valence: 0.5,
        arousal: 0.5,
        mood_happy: 0.1,
        mood_sad: 0.2,
        mood_relaxed: 0.3,
        mood_aggressive: 0.4,
        mood_party: 0.5,
        mood_acoustic: 0.6,
        mood_electronic: 0.7,
    }
}

fn loudness() -> analysis::LoudnessAnalysis {
    analysis::LoudnessAnalysis {
        integrated_lufs: -14.2,
        loudness_range_lu: 6.5,
    }
}

fn key() -> analysis::KeyAnalysis {
    analysis::KeyAnalysis {
        tonic: 0,
        mode: analysis::KeyMode::Major,
        strength: 0.87,
    }
}

fn tempo() -> analysis::TempoAnalysis {
    analysis::TempoAnalysis {
        bpm: 120.0,
        confidence: 0.92,
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_database_has_track_analysis_table_with_generation_index(pool: PgPool) {
    let definition: Option<String> = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes WHERE indexname = 'track_analysis_generation_idx'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        definition.is_some(),
        "missing track_analysis generation index"
    );
    let pk_column: String = sqlx::query_scalar(
        "SELECT a.attname FROM pg_index i
         JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey)
         WHERE i.indrelid = 'track_analysis'::regclass AND i.indisprimary",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pk_column, "track_id");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_row_reads_as_none_not_a_fabricated_default(pool: PgPool) {
    let t = track(&pool).await;
    assert!(analysis::analysis(&pool, t).await.unwrap().is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn each_group_stores_and_reads_back_independently(pool: PgPool) {
    let t = track(&pool).await;
    let gener = generation(&pool, t).await;

    assert!(
        analysis::store_musicnn(&pool, t, gener, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    let row = analysis::analysis(&pool, t).await.unwrap().unwrap();
    assert_eq!(row.musicnn, Some(("musicnn-v1".into(), musicnn())));
    assert!(row.loudness.is_none());
    assert!(row.key.is_none());
    assert!(row.tempo.is_none());

    assert!(
        analysis::store_loudness(&pool, t, gener, "dsp-loudness-v1", &loudness())
            .await
            .unwrap()
    );
    assert!(
        analysis::store_key(&pool, t, gener, "dsp-key-v1", &key())
            .await
            .unwrap()
    );
    assert!(
        analysis::store_tempo(&pool, t, gener, "dsp-tempo-v1", &tempo())
            .await
            .unwrap()
    );

    let row = analysis::analysis(&pool, t).await.unwrap().unwrap();
    assert_eq!(row.musicnn, Some(("musicnn-v1".into(), musicnn())));
    assert_eq!(row.loudness, Some(("dsp-loudness-v1".into(), loudness())));
    assert_eq!(row.key, Some(("dsp-key-v1".into(), key())));
    assert_eq!(row.tempo, Some(("dsp-tempo-v1".into(), tempo())));
    assert_eq!(row.file_generation, gener);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tempo_failing_leaves_musicnn_loudness_and_key_intact(pool: PgPool) {
    // DSP key/tempo can fail independently of MusiCNN (and of each other):
    // never storing tempo must never block or clear the groups that did
    // succeed in the same attempt.
    let t = track(&pool).await;
    let gener = generation(&pool, t).await;
    assert!(
        analysis::store_musicnn(&pool, t, gener, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    assert!(
        analysis::store_loudness(&pool, t, gener, "dsp-loudness-v1", &loudness())
            .await
            .unwrap()
    );
    assert!(
        analysis::store_key(&pool, t, gener, "dsp-key-v1", &key())
            .await
            .unwrap()
    );
    // Simulated tempo failure: caller simply never calls store_tempo.
    let row = analysis::analysis(&pool, t).await.unwrap().unwrap();
    assert!(row.musicnn.is_some());
    assert!(row.loudness.is_some());
    assert!(row.key.is_some());
    assert!(row.tempo.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_generation_write_is_rejected_without_partial_mutation(pool: PgPool) {
    let t = track(&pool).await;
    let stale_gen = generation(&pool, t).await;
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !analysis::store_musicnn(&pool, t, stale_gen, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    assert!(analysis::analysis(&pool, t).await.unwrap().is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_analysis")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_identity_change_deletes_the_whole_row_not_just_one_group(pool: PgPool) {
    let t = track(&pool).await;
    let gener = generation(&pool, t).await;
    assert!(
        analysis::store_musicnn(&pool, t, gener, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    assert!(
        analysis::store_loudness(&pool, t, gener, "dsp-loudness-v1", &loudness())
            .await
            .unwrap()
    );
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    assert!(analysis::analysis(&pool, t).await.unwrap().is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_analysis WHERE track_id = $1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "trigger must delete the entire row, not leave orphaned groups"
    );
    // The 'analysis' stage is reset to pending by the same trigger, same as
    // 'embedding'/'similarity' (pre-existing 0024 behavior, unchanged).
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'analysis', 'working')",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    let status: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'analysis'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "pending");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn later_generation_write_fully_replaces_the_row_not_merges_stale_groups(pool: PgPool) {
    let t = track(&pool).await;
    let gen1 = generation(&pool, t).await;
    assert!(
        analysis::store_musicnn(&pool, t, gen1, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    assert!(
        analysis::store_tempo(&pool, t, gen1, "dsp-tempo-v1", &tempo())
            .await
            .unwrap()
    );
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    let gen2 = generation(&pool, t).await;
    assert!(gen2 > gen1);
    // Only loudness computed for the new generation; musicnn/tempo from the
    // old generation must not leak through (the trigger already deleted the
    // row; this would also fail if it hadn't).
    assert!(
        analysis::store_loudness(&pool, t, gen2, "dsp-loudness-v1", &loudness())
            .await
            .unwrap()
    );
    let row = analysis::analysis(&pool, t).await.unwrap().unwrap();
    assert_eq!(row.file_generation, gen2);
    assert!(
        row.musicnn.is_none(),
        "stale generation's musicnn must not leak"
    );
    assert!(
        row.tempo.is_none(),
        "stale generation's tempo must not leak"
    );
    assert!(row.loudness.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_deletion_cascades_to_analysis_row(pool: PgPool) {
    let t = track(&pool).await;
    let gener = generation(&pool, t).await;
    assert!(
        analysis::store_musicnn(&pool, t, gener, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_analysis")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn half_populated_group_is_rejected_by_the_database_not_just_app_code(pool: PgPool) {
    // Exercise the migration's own all-or-nothing CHECK constraints
    // directly with raw SQL, bypassing kima_db::analysis's helpers, so the
    // guarantee holds even against a future caller bug, not only against
    // this module's current write path.
    let t = track(&pool).await;
    let gener = generation(&pool, t).await;
    let result = sqlx::query(
        "INSERT INTO track_analysis (track_id, file_generation, musicnn_pipeline_version, musicnn_danceability)
         VALUES ($1, $2, 'v1', 0.5)",
    )
    .bind(t)
    .bind(gener)
    .execute(&pool)
    .await;
    assert!(
        result.is_err(),
        "half-populated musicnn group must violate a CHECK constraint"
    );

    let result = sqlx::query(
        "INSERT INTO track_analysis (track_id, file_generation, dsp_key_pipeline_version, key_tonic, key_mode)
         VALUES ($1, $2, 'v1', 0, 'major')",
    )
    .bind(t)
    .bind(gener)
    .execute(&pool)
    .await;
    assert!(
        result.is_err(),
        "half-populated key group (missing strength) must violate a CHECK constraint"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn out_of_range_values_are_rejected_by_check_constraints(pool: PgPool) {
    let t = track(&pool).await;
    let gener = generation(&pool, t).await;

    for key_mode in ["dorian", "MAJOR", ""] {
        let result = sqlx::query(
            "INSERT INTO track_analysis (track_id, file_generation, dsp_key_pipeline_version,
                 key_tonic, key_mode, key_strength, key_computed_at)
             VALUES ($1, $2, 'v1', 0, $3, 0.5, now())",
        )
        .bind(t)
        .bind(gener)
        .bind(key_mode)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "key_mode={key_mode:?} must be rejected");
    }

    for tonic in [-1_i16, 12] {
        let result = sqlx::query(
            "INSERT INTO track_analysis (track_id, file_generation, dsp_key_pipeline_version,
                 key_tonic, key_mode, key_strength, key_computed_at)
             VALUES ($1, $2, 'v1', $3, 'major', 0.5, now())",
        )
        .bind(t)
        .bind(gener)
        .bind(tonic)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "key_tonic={tonic} must be rejected");
    }

    for bpm in [0.0_f64, -5.0, 401.0] {
        let result = sqlx::query(
            "INSERT INTO track_analysis (track_id, file_generation, dsp_tempo_pipeline_version,
                 tempo_bpm, tempo_confidence, tempo_computed_at)
             VALUES ($1, $2, 'v1', $3, 0.5, now())",
        )
        .bind(t)
        .bind(gener)
        .bind(bpm)
        .execute(&pool)
        .await;
        assert!(result.is_err(), "tempo_bpm={bpm} must be rejected");
    }

    for danceability in [-0.01_f32, 1.01] {
        let result = sqlx::query(
            "INSERT INTO track_analysis (track_id, file_generation, musicnn_pipeline_version,
                 musicnn_danceability, musicnn_valence, musicnn_arousal, musicnn_mood_happy,
                 musicnn_mood_sad, musicnn_mood_relaxed, musicnn_mood_aggressive,
                 musicnn_mood_party, musicnn_mood_acoustic, musicnn_mood_electronic,
                 musicnn_computed_at)
             VALUES ($1, $2, 'v1', $3, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, now())",
        )
        .bind(t)
        .bind(gener)
        .bind(danceability)
        .execute(&pool)
        .await;
        assert!(
            result.is_err(),
            "musicnn_danceability={danceability} must be rejected"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn upgrade_from_previous_migration_set_preserves_existing_rows_then_adds_track_analysis(
    pool: PgPool,
) {
    // Forward-only migration convention (no .down.sql files anywhere in
    // this project, confirmed against crates/kima-db/migrations/): "backward
    // semantics" for this slice means verifying an *upgrade* from the
    // pre-0026 schema doesn't disturb existing track_embeddings/track_stages
    // data, not a reverse migration that does not exist in this codebase.
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version < 26)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await.unwrap();
    let t = track(&pool).await;
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'embedding', 'done')",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256,
             pipeline_version, embedding)
         VALUES ($1, 0, 'm', $2, 'v1', $3::real[]::vector)",
    )
    .bind(t)
    .bind(&[0u8; 32][..])
    .bind({
        let mut v = vec![0.0_f32; 512];
        v[0] = 1.0;
        v
    })
    .execute(&pool)
    .await
    .unwrap();

    // The table must not exist yet before 0026 runs.
    let exists_before: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'track_analysis')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!exists_before);

    kima_db::MIGRATOR.run(&pool).await.unwrap();
    // Idempotent re-run (sqlx re-applying an already-applied migration set
    // must be a no-op, matching the existing upgrade test's convention).
    kima_db::MIGRATOR.run(&pool).await.unwrap();

    let exists_after: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'track_analysis')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(exists_after);

    // Pre-existing data survives the upgrade untouched.
    let embedding_status: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'embedding'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(embedding_status, "done");
    let embedding_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM track_embeddings WHERE track_id = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(embedding_count, 1);

    // The upgraded, still-replaced invalidate_track_ml() now also reaches
    // track_analysis for a track created after the upgrade.
    let gener = generation(&pool, t).await;
    assert!(
        analysis::store_musicnn(&pool, t, gener, "musicnn-v1", &musicnn())
            .await
            .unwrap()
    );
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    assert!(analysis::analysis(&pool, t).await.unwrap().is_none());
}
