mod vibe_common;
use kima_db::{PgPool, vibe::readings};
use kima_vibe::zero_shot::Readings;
use vibe_common::*;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn readings_stored_with_embedded_at_and_rewritten_on_new_vector(pool: PgPool) {
    let t = track(&pool).await;
    embed(&pool, t.track, &axis(511)).await;
    let pending = readings::pending(&pool, 500).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].embedding[511], 1.);
    let r = Readings::from_similarities([0.; 8]).unwrap();
    let rows = vec![(pending[0].track_id, pending[0].embedded_at, r)];
    assert_eq!(readings::upsert(&pool, &rows).await.unwrap(), 1);
    assert!(readings::pending(&pool, 500).await.unwrap().is_empty());
    let stored: (time::OffsetDateTime, f32) =
        sqlx::query_as("SELECT embedded_at,valence_raw FROM track_clap_readings WHERE track_id=$1")
            .bind(t.track)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, pending[0].embedded_at);
    assert!((f64::from(stored.1) - r.valence_raw).abs() < 1e-6);
    embed(&pool, t.track, &axis(0)).await;
    // An old computation cannot overwrite the new vector's readings.
    assert_eq!(readings::upsert(&pool, &rows).await.unwrap(), 0);
    let next = readings::pending(&pool, 500).await.unwrap();
    assert_eq!(next.len(), 1);
    assert_ne!(next[0].embedded_at, stored.0);
    let r = Readings::from_similarities([0., 1., 0., 0., 1., 0., 1., 0.]).unwrap();
    assert_eq!(
        readings::upsert(&pool, &[(t.track, next[0].embedded_at, r)])
            .await
            .unwrap(),
        1
    );
    let at: time::OffsetDateTime =
        sqlx::query_scalar("SELECT embedded_at FROM track_clap_readings")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(at, next[0].embedded_at);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn publication_locks_track_before_embedding_like_invalidation(pool: PgPool) {
    let t = track(&pool).await;
    embed(&pool, t.track, &axis(0)).await;
    let pending = readings::pending(&pool, 500).await.unwrap();
    let mut invalidator = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM tracks WHERE id=$1 FOR UPDATE")
        .bind(t.track)
        .execute(&mut *invalidator)
        .await
        .unwrap();
    let p = pool.clone();
    let row = (
        t.track,
        pending[0].embedded_at,
        Readings::from_similarities([0.; 8]).unwrap(),
    );
    let task = tokio::spawn(async move { readings::upsert(&p, &[row]).await });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FOR SHARE%')").fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    // Same lock order as the track invalidation trigger: track, then vector.
    sqlx::query("DELETE FROM track_embeddings WHERE track_id=$1")
        .bind(t.track)
        .execute(&mut *invalidator)
        .await
        .unwrap();
    invalidator.commit().await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), 0);
    assert_eq!(count(&pool, "track_clap_readings").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn readings_ignore_wrong_model_and_missing_tracks(pool: PgPool) {
    let t = track(&pool).await;
    let (_, sha, pipeline) = identity();
    embed_as(&pool, t.track, "hf_audio", &sha, pipeline, &axis(0))
        .await
        .unwrap();
    assert!(readings::pending(&pool, 500).await.unwrap().is_empty());
    embed(&pool, t.track, &axis(1)).await;
    let pending = readings::pending(&pool, 500).await.unwrap();
    sqlx::query("UPDATE tracks SET missing_since=now() WHERE id=$1")
        .bind(t.track)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        readings::upsert(
            &pool,
            &[(
                t.track,
                pending[0].embedded_at,
                Readings::from_similarities([0.; 8]).unwrap()
            )]
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(count(&pool, "track_clap_readings").await, 0);
}
