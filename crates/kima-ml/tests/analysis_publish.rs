//! Synthetic wiring tests for the analysis-stage publish adapter
//! (`kima_ml::analysis_publish`): bridges already-computed `MusicnnAggregate`
//! / DSP `LoudnessAnalysis`/`KeyAnalysis`/`TempoAnalysis` results to
//! `kima_db::analysis::store_*`. No real-model/Essentia parity exercised
//! here (out of scope); this only exercises the real adapter/storage
//! boundary and its independent-per-group failure semantics, mirroring
//! `tests/pg_publish.rs`'s convention for the embedding stage.
#![cfg(feature = "pg-publish")]

use kima_db::{PgPool, analysis};
use kima_ml::analysis_publish::{
    AnalysisGroupInputs, AnalysisPublishError, publish_key, publish_loudness, publish_musicnn,
    publish_tempo, publish_track_analysis, publish_track_analysis_blocking, to_db_key,
    to_db_loudness, to_db_musicnn, to_db_tempo,
};
use kima_ml::dsp::{KeyAnalysis, KeyMode, LoudnessAnalysis, TempoAnalysis};
use kima_ml::musicnn::MusicnnAggregate;
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

fn musicnn() -> MusicnnAggregate {
    MusicnnAggregate {
        mood_happy: 0.1,
        mood_sad: 0.2,
        mood_relaxed: 0.3,
        mood_aggressive: 0.4,
        mood_party: 0.5,
        mood_acoustic: 0.6,
        mood_electronic: 0.7,
        danceability: 0.5,
        valence: 0.5,
        arousal: 0.5,
    }
}

fn loudness() -> LoudnessAnalysis {
    LoudnessAnalysis {
        integrated_lufs: -14.2,
        loudness_range_lu: 6.3,
    }
}

fn key() -> KeyAnalysis {
    KeyAnalysis {
        tonic: 9,
        mode: KeyMode::Minor,
        strength: 0.87,
    }
}

fn tempo() -> TempoAnalysis {
    TempoAnalysis {
        bpm: 128.0,
        confidence: 0.91,
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn all_four_groups_publish_independently_and_are_readable(pool: PgPool) {
    let track_id = track(&pool).await;
    let gen_ = generation(&pool, track_id).await;
    let (musicnn_result, loudness_result) = (musicnn(), loudness());
    let (key_result, tempo_result) = (key(), tempo());
    let inputs = AnalysisGroupInputs {
        musicnn: Some(("musicnn-v1", &musicnn_result)),
        loudness: Some(("dsp-loudness-v1", &loudness_result)),
        key: Some(("dsp-key-v1", &key_result)),
        tempo: Some(("dsp-tempo-v1", &tempo_result)),
    };
    let report = publish_track_analysis(&pool, track_id, gen_, &inputs).await;
    assert!(report.musicnn.unwrap().is_ok());
    assert!(report.loudness.unwrap().is_ok());
    assert!(report.key.unwrap().is_ok());
    assert!(report.tempo.unwrap().is_ok());

    let row = analysis::analysis(&pool, track_id).await.unwrap().unwrap();
    assert_eq!(row.musicnn.unwrap().1, to_db_musicnn(&musicnn_result));
    assert_eq!(row.loudness.unwrap().1, to_db_loudness(&loudness_result));
    assert_eq!(
        row.key.unwrap(),
        ("dsp-key-v1".to_string(), to_db_key(&key_result))
    );
    assert_eq!(row.tempo.unwrap().1, to_db_tempo(&tempo_result));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_group_is_not_attempted_while_others_still_publish(pool: PgPool) {
    let track_id = track(&pool).await;
    let gen_ = generation(&pool, track_id).await;
    let loudness_result = loudness();
    let tempo_result = tempo();
    let inputs = AnalysisGroupInputs {
        musicnn: None,
        loudness: Some(("dsp-loudness-v1", &loudness_result)),
        key: None,
        tempo: Some(("dsp-tempo-v1", &tempo_result)),
    };
    let report = publish_track_analysis(&pool, track_id, gen_, &inputs).await;
    assert!(report.musicnn.is_none());
    assert!(report.loudness.unwrap().is_ok());
    assert!(report.key.is_none());
    assert!(report.tempo.unwrap().is_ok());

    let row = analysis::analysis(&pool, track_id).await.unwrap().unwrap();
    assert!(row.musicnn.is_none());
    assert!(row.loudness.is_some());
    assert!(row.key.is_none());
    assert!(row.tempo.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn one_groups_stale_generation_rejection_does_not_block_a_sibling_group(pool: PgPool) {
    let track_id = track(&pool).await;
    let gen_ = generation(&pool, track_id).await;
    let stale = gen_ - 1;
    let musicnn_result = musicnn();
    let loudness_result = loudness();

    // MusiCNN computed against a now-stale generation: rejected, not stored.
    let musicnn_outcome =
        publish_musicnn(&pool, track_id, stale, "musicnn-v1", &musicnn_result).await;
    assert!(matches!(
        musicnn_outcome,
        Err(AnalysisPublishError::Rejected)
    ));

    // Loudness computed against the current generation: still succeeds,
    // independently of the sibling group's rejection above.
    publish_loudness(&pool, track_id, gen_, "dsp-loudness-v1", &loudness_result)
        .await
        .unwrap();

    let row = analysis::analysis(&pool, track_id).await.unwrap().unwrap();
    assert!(row.musicnn.is_none(), "rejected group must not be stored");
    assert_eq!(row.loudness.unwrap().1, to_db_loudness(&loudness_result));

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_analysis")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "exactly one row, with the loudness group filled");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn key_group_rejection_through_direct_adapter_does_not_mutate_storage(pool: PgPool) {
    let track_id = track(&pool).await;
    let gen_ = generation(&pool, track_id).await;
    let key_result = key();
    let result = publish_key(&pool, track_id, gen_ + 1, "dsp-key-v1", &key_result).await;
    assert!(matches!(result, Err(AnalysisPublishError::Rejected)));
    assert!(analysis::analysis(&pool, track_id).await.unwrap().is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tempo_group_rejection_through_direct_adapter_does_not_mutate_storage(pool: PgPool) {
    let track_id = track(&pool).await;
    let gen_ = generation(&pool, track_id).await;
    let tempo_result = tempo();
    let result = publish_tempo(&pool, track_id, gen_ + 1, "dsp-tempo-v1", &tempo_result).await;
    assert!(matches!(result, Err(AnalysisPublishError::Rejected)));
    assert!(analysis::analysis(&pool, track_id).await.unwrap().is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn blocking_bridge_publishes_from_a_dedicated_blocking_task(pool: PgPool) {
    let track_id = track(&pool).await;
    let gen_ = generation(&pool, track_id).await;
    let musicnn_result = musicnn();
    let handle = tokio::runtime::Handle::current();
    let p = pool.clone();
    let report = tokio::task::spawn_blocking(move || {
        let inputs = AnalysisGroupInputs {
            musicnn: Some(("musicnn-v1", &musicnn_result)),
            loudness: None,
            key: None,
            tempo: None,
        };
        publish_track_analysis_blocking(&handle, &p, track_id, gen_, &inputs)
    })
    .await
    .unwrap();
    assert!(report.musicnn.unwrap().is_ok());
    let row = analysis::analysis(&pool, track_id).await.unwrap().unwrap();
    assert_eq!(row.musicnn.unwrap().1, to_db_musicnn(&musicnn_result));
}
