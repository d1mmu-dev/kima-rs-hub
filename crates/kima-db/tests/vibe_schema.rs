//! Schema of the vibe engine (migration 0040) and the one eligibility fragment.
mod vibe_common;
use kima_db::PgPool;
use kima_db::vibe::{ELIGIBLE, MATCHING_SOUND};
use std::borrow::Cow;
use vibe_common::*;

const MOODS: [&str; 22] = [
    "happy",
    "fun_quirky",
    "uplifting",
    "sad",
    "nostalgic",
    "dark",
    "trippy",
    "calm",
    "dreamy",
    "energetic",
    "aggressive",
    "epic",
    "party",
    "groovy",
    "romantic",
    "sexy",
    "summer",
    "night",
    "driving",
    "workout",
    "focus",
    "sleep",
];

async fn columns(pool: &PgPool, table: &str) -> Vec<(String, String, String)> {
    sqlx::query_as("SELECT column_name::text, data_type::text, is_nullable::text FROM information_schema.columns WHERE table_name = $1 ORDER BY ordinal_position")
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fresh_database_has_vibe_tables(pool: PgPool) {
    assert!(
        current_database(&pool).await.starts_with("_sqlx_test"),
        "tests run in sqlx's throwaway database"
    );
    for table in [
        "matching_feel",
        "matching_state",
        "matching_map",
        "track_moods",
        "mood_state",
        "track_clap_readings",
    ] {
        assert!(!columns(&pool, table).await.is_empty(), "{table} missing");
    }
    let moods = columns(&pool, "track_moods").await;
    assert_eq!(moods.len(), 3 + 22);
    let names: Vec<&str> = moods[3..].iter().map(|c| c.0.as_str()).collect();
    assert_eq!(names, MOODS, "22 mood columns in upstream's order");
    assert!(moods[3..].iter().all(|c| c.1 == "real" && c.2 == "NO"));

    let state = columns(&pool, "mood_state").await;
    assert_eq!(state.len(), 5 + 44 + 6);
    for m in MOODS {
        for suffix in ["mean", "sd"] {
            let col = state
                .iter()
                .find(|c| c.0 == format!("{m}_{suffix}"))
                .unwrap_or_else(|| panic!("{m}_{suffix}"));
            assert_eq!((col.1.as_str(), col.2.as_str()), ("double precision", "NO"));
        }
    }
    for r in ["arousal", "instrumental_logit", "acoustic_logit"] {
        for suffix in ["mean", "sd"] {
            let col = state
                .iter()
                .find(|c| c.0 == format!("{r}_{suffix}"))
                .unwrap();
            assert_eq!(col.2, "YES", "absent readings are NULL: {r}_{suffix}");
        }
    }
    let matching = columns(&pool, "matching_state").await;
    for c in ["map_version", "map_frame", "map_updated_at"] {
        assert_eq!(matching.iter().find(|x| x.0 == c).unwrap().2, "YES");
    }
}

#[sqlx::test(migrations = false)]
async fn upgrade_from_previous_migration_set(pool: PgPool) {
    let all = &kima_db::MIGRATOR;
    let before: Vec<_> = all
        .migrations
        .iter()
        .filter(|m| m.version < 40)
        .cloned()
        .collect();
    assert!(before.len() >= 28 && before.len() < all.migrations.len());
    let old = sqlx::migrate::Migrator {
        migrations: Cow::Owned(before),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    // A pre-existing library with an embedding and a stage row survives the upgrade.
    let t = track(&pool).await;
    embed(&pool, t.track, &axis(3)).await;
    sqlx::query("UPDATE enrichment_state SET current_phase = 'analysis'")
        .execute(&pool)
        .await
        .unwrap();
    all.run(&pool).await.unwrap();
    assert_eq!(count(&pool, "track_embeddings").await, 1);
    assert_eq!(count(&pool, "matching_feel").await, 0);
    sqlx::query("UPDATE enrichment_state SET current_phase = 'similarity'")
        .execute(&pool)
        .await
        .unwrap();
    all.run(&pool).await.unwrap(); // idempotent
}

async fn fill_derived(pool: &PgPool, track: uuid::Uuid) {
    sqlx::query("INSERT INTO matching_feel (track_id, embedded_at, probe, valence, arousal) VALUES ($1, now(), 'p', 0.5, 0.5)").bind(track).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO matching_map (track_id, embedded_at, x, y, z, colour, laid_out) VALUES ($1, now(), 0, 0, 0, 0.5, true)").bind(track).execute(pool).await.unwrap();
    let cols = MOODS.join(", ");
    let zeros = vec!["0"; 22].join(", ");
    sqlx::query(&format!("INSERT INTO track_moods (track_id, embedded_at, weights, {cols}) VALUES ($1, now(), 'w', {zeros})")).bind(track).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw) VALUES ($1, now(), 0.1, 0.2, 0.3, 0.4)").bind(track).execute(pool).await.unwrap();
}

async fn derived_rows(pool: &PgPool) -> i64 {
    let mut n = 0;
    for t in [
        "matching_feel",
        "matching_map",
        "track_moods",
        "track_clap_readings",
    ] {
        n += count(pool, t).await;
    }
    n
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleting_an_embedding_cascades_feel_map_moods_readings(pool: PgPool) {
    let t = track(&pool).await;
    embed(&pool, t.track, &axis(0)).await;
    fill_derived(&pool, t.track).await;
    assert_eq!(derived_rows(&pool).await, 4);
    sqlx::query("DELETE FROM track_embeddings WHERE track_id = $1")
        .bind(t.track)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(derived_rows(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_change_trigger_cascades_through_embedding(pool: PgPool) {
    let t = track(&pool).await;
    let other = track(&pool).await;
    for s in [&t, &other] {
        embed(&pool, s.track, &axis(1)).await;
        fill_derived(&pool, s.track).await;
    }
    assert_eq!(derived_rows(&pool).await, 8);
    sqlx::query("UPDATE tracks SET size = size + 1 WHERE id = $1")
        .bind(t.track)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(&pool, "track_embeddings").await,
        1,
        "the trigger deletes the changed file's vector"
    );
    assert_eq!(
        derived_rows(&pool).await,
        4,
        "and its four dependent rows; the other track keeps its rows"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn state_tables_are_singletons(pool: PgPool) {
    let genres = serde_json::json!({});
    sqlx::query("INSERT INTO matching_state (version, fingerprint, track_count, feel_mean_valence, feel_mean_arousal, feel_sd_valence, feel_sd_arousal, sound_spread, genre_spread, genre_weight, genres) VALUES ('v','f',1,0,0,0,0,0,0,0,$1)")
        .bind(&genres).execute(&pool).await.unwrap();
    let second = sqlx::query("INSERT INTO matching_state (id, version, fingerprint, track_count, feel_mean_valence, feel_mean_arousal, feel_sd_valence, feel_sd_arousal, sound_spread, genre_spread, genre_weight, genres) VALUES ('other','v','f',1,0,0,0,0,0,0,0,$1)")
        .bind(&genres).execute(&pool).await;
    assert!(second.is_err(), "a second matching_state row is refused");
    let dup = sqlx::query("INSERT INTO matching_state (version, fingerprint, track_count, feel_mean_valence, feel_mean_arousal, feel_sd_valence, feel_sd_arousal, sound_spread, genre_spread, genre_weight, genres) VALUES ('v','f',1,0,0,0,0,0,0,0,$1)")
        .bind(&genres).execute(&pool).await;
    assert!(dup.is_err(), "the default id is unique");

    let cols: Vec<String> = MOODS
        .iter()
        .flat_map(|m| [format!("{m}_mean"), format!("{m}_sd")])
        .collect();
    let zeros = vec!["0"; 44].join(", ");
    let insert = |id: &str| {
        format!(
            "INSERT INTO mood_state (id, version, fingerprint, track_count, {}) VALUES ('{id}', 'v', 'f', 0, {zeros})",
            cols.join(", ")
        )
    };
    sqlx::query(&insert("default"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query(&insert("other")).execute(&pool).await.is_err(),
        "a second mood_state row is refused"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn phase_check_accepts_mood_and_similarity(pool: PgPool) {
    for phase in [
        "artists",
        "tracks",
        "fingerprint",
        "embedding",
        "analysis",
        "mood",
        "similarity",
    ] {
        sqlx::query("UPDATE enrichment_state SET current_phase = $1")
            .bind(phase)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{phase}: {e}"));
    }
    assert!(
        sqlx::query("UPDATE enrichment_state SET current_phase = 'bogus'")
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE enrichment_state SET current_phase = NULL")
        .execute(&pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn eligible_requires_model_gate_dims_norm_and_presence(pool: PgPool) {
    let (model, sha, pipeline) = identity();
    let good = track(&pool).await;
    embed(&pool, good.track, &axis(0)).await;

    let other_model = track(&pool).await;
    embed_as(
        &pool,
        other_model.track,
        "hf_audio",
        &sha,
        pipeline,
        &axis(1),
    )
    .await
    .unwrap();
    let other_sha = track(&pool).await;
    embed_as(
        &pool,
        other_sha.track,
        model,
        &[7u8; 32],
        pipeline,
        &axis(2),
    )
    .await
    .unwrap();
    let other_pipeline = track(&pool).await;
    embed_as(
        &pool,
        other_pipeline.track,
        model,
        &sha,
        "clap-audio-v1",
        &axis(3),
    )
    .await
    .unwrap();

    // A file that is missing: mark it first (the file-change trigger would delete its vector), then store one.
    let missing = track(&pool).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(missing.track)
        .execute(&pool)
        .await
        .unwrap();
    embed(&pool, missing.track, &axis(4)).await;
    assert_eq!(count(&pool, "track_embeddings").await, 5);

    let sql = format!(
        "SELECT te.track_id FROM {} ORDER BY te.track_id",
        &*ELIGIBLE
    );
    let eligible: Vec<uuid::Uuid> = sqlx::query_scalar(&sql).fetch_all(&pool).await.unwrap();
    assert_eq!(
        eligible,
        [good.track],
        "right model + sha + pipeline + present"
    );

    // A 256-d vector cannot be stored (vector(512)); a zero vector is refused by the unit-norm CHECK.
    let wide = track(&pool).await;
    assert!(
        embed_as(&pool, wide.track, model, &sha, pipeline, &[1.0_f32; 256])
            .await
            .is_err()
    );
    assert!(
        embed_as(
            &pool,
            wide.track,
            model,
            &sha,
            pipeline,
            &vec![0.0_f32; 512]
        )
        .await
        .is_err()
    );

    // The file goes missing through a normal update: the vector goes with it, at once.
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(good.track)
        .execute(&pool)
        .await
        .unwrap();
    let after: Vec<uuid::Uuid> = sqlx::query_scalar(&sql).fetch_all(&pool).await.unwrap();
    assert!(after.is_empty());
}

#[test]
fn matching_identity_is_one_constant_with_safe_literals() {
    // The fragment embeds these as SQL literals: keep them to a safe alphabet, and keep the hash 32 bytes.
    let safe = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    assert!(safe(MATCHING_SOUND.model_id) && safe(MATCHING_SOUND.pipeline_version));
    assert_eq!(MATCHING_SOUND.model_sha256.len(), 32);
    assert!(ELIGIBLE.contains(MATCHING_SOUND.model_id));
    assert!(
        ELIGIBLE.contains("t.missing_since IS NULL")
            && ELIGIBLE.contains("vector_dims(te.embedding) = 512")
            && ELIGIBLE.contains("vector_norm(te.embedding) > 0")
    );
}

#[test]
fn every_vibe_query_goes_through_eligible() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vibe");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") || path.file_name().unwrap() == "eligible.rs"
        {
            continue;
        }
        checked += 1;
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap();
            assert!(
                !code.to_lowercase().contains("track_embeddings"),
                "{}:{} reads track_embeddings outside ELIGIBLE: {line}",
                path.display(),
                n + 1
            );
        }
    }
    assert!(checked >= 1, "the module has files to check");
}

#[test]
fn matching_sha_is_the_music_checkpoint_of_the_design() {
    let hex: String = MATCHING_SOUND
        .model_sha256
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        "ab1bf5eba067b246198c630f153f774e8f1c3169b3924d4d6a4dc247bd73de97"
    );
}
