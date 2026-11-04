//! Synthetic typed vectors exercise the real adapter/storage boundary, not model parity.
#![cfg(feature = "pg-publish")]

use kima_db::{PgPool, ml};
use kima_ml::{
    clap::ClapEmbedding,
    publish::{PublishError, publish_clap_embedding, publish_clap_embedding_blocking},
};

fn provenance() -> ml::Provenance {
    ml::Provenance {
        model_id: "synthetic-publish-test".into(),
        model_sha256: [42; 32],
        pipeline_version: "synthetic-v1".into(),
    }
}

fn embedding() -> ClapEmbedding {
    let mut vector = [0.; 512];
    vector[0] = 0.6;
    vector[511] = 0.8;
    ClapEmbedding::from_normalized(vector).unwrap()
}

async fn working(pool: &PgPool) -> ml::EmbeddingClaim {
    let artist: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO artists(id,name,name_key) VALUES (gen_random_uuid(),'publish artist','publish artist') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let album: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO albums(id,artist_id,title,title_key) VALUES (gen_random_uuid(),$1,'publish album','publish album') RETURNING id",
    ).bind(artist).fetch_one(pool).await.unwrap();
    let track: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO tracks(id,album_id,artist_name,title,codec,path,size,mtime,fingerprint)
         VALUES (gen_random_uuid(),$1,'publish artist','publish track','flac','/synthetic/publish.flac',1,now(),'\\x00') RETURNING id",
    ).bind(album).fetch_one(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO track_stages(track_id,stage,status,started_at,next_attempt_at)
         VALUES ($1,'embedding','working',clock_timestamp(),clock_timestamp()+interval '15 minutes')",
    ).bind(track).execute(pool).await.unwrap();
    ml::embedding_claim(pool, track).await.unwrap().unwrap()
}

async fn assert_published(pool: &PgPool, claim: &ml::EmbeddingClaim) {
    assert_eq!(
        ml::embedding(pool, claim.track_id, &provenance())
            .await
            .unwrap(),
        Some(embedding().as_slice().to_vec())
    );
    let row: (i64, String, Vec<u8>, String) = sqlx::query_as(
        "SELECT file_generation,model_id,model_sha256,pipeline_version FROM track_embeddings WHERE track_id=$1",
    ).bind(claim.track_id).fetch_one(pool).await.unwrap();
    assert_eq!(
        row,
        (
            claim.file_generation,
            provenance().model_id,
            vec![42; 32],
            provenance().pipeline_version
        )
    );
    let stage: (String, bool, bool, String, String) = sqlx::query_as(
        "SELECT status,next_attempt_at IS NULL,finished_at IS NOT NULL,source,evidence->>'pipeline_version'
         FROM track_stages WHERE track_id=$1 AND stage='embedding'",
    ).bind(claim.track_id).fetch_one(pool).await.unwrap();
    assert_eq!(
        stage,
        (
            "done".into(),
            true,
            true,
            provenance().model_id,
            provenance().pipeline_version
        )
    );
    assert!(
        ml::embedding_claim(pool, claim.track_id)
            .await
            .unwrap()
            .is_none()
    );
    let wrong = ml::Provenance {
        pipeline_version: "other".into(),
        ..provenance()
    };
    assert!(
        ml::embedding(pool, claim.track_id, &wrong)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn async_publication_persists_vector_provenance_and_stage_success(pool: PgPool) {
    let claim = working(&pool).await;
    assert!(
        ml::embedding(&pool, claim.track_id, &provenance())
            .await
            .unwrap()
            .is_none()
    );
    publish_clap_embedding(&pool, &claim, &provenance(), &embedding())
        .await
        .unwrap();
    assert_published(&pool, &claim).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn blocking_publication_from_spawn_blocking_persists_result(pool: PgPool) {
    let claim = working(&pool).await;
    let handle = tokio::runtime::Handle::current();
    let p = pool.clone();
    let c = claim.clone();
    tokio::task::spawn_blocking(move || {
        publish_clap_embedding_blocking(&handle, &p, &c, &provenance(), &embedding())
    })
    .await
    .unwrap()
    .unwrap();
    assert_published(&pool, &claim).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn superseded_claim_is_rejected_without_changing_replacement(pool: PgPool) {
    let stale = working(&pool).await;
    sqlx::query("UPDATE track_stages SET started_at=started_at+interval '1 second' WHERE track_id=$1 AND stage='embedding'")
        .bind(stale.track_id).execute(&pool).await.unwrap();
    let replacement = ml::embedding_claim(&pool, stale.track_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(replacement.started_at, stale.started_at);
    assert!(matches!(
        publish_clap_embedding(&pool, &stale, &provenance(), &embedding()).await,
        Err(PublishError::Rejected)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        ml::embedding_claim(&pool, stale.track_id).await.unwrap(),
        Some(replacement.clone())
    );
    publish_clap_embedding(&pool, &replacement, &provenance(), &embedding())
        .await
        .unwrap();
    assert_published(&pool, &replacement).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn expired_claim_is_rejected_through_blocking_bridge_without_stage_success(pool: PgPool) {
    let claim = working(&pool).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1 AND stage='embedding'")
        .bind(claim.track_id).execute(&pool).await.unwrap();
    assert!(
        ml::embedding_claim(&pool, claim.track_id)
            .await
            .unwrap()
            .is_none()
    );
    let handle = tokio::runtime::Handle::current();
    let p = pool.clone();
    let c = claim.clone();
    let result = tokio::task::spawn_blocking(move || {
        publish_clap_embedding_blocking(&handle, &p, &c, &provenance(), &embedding())
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(PublishError::Rejected)));
    let state: (String, bool, bool) = sqlx::query_as(
        "SELECT status,finished_at IS NULL,next_attempt_at < clock_timestamp()
         FROM track_stages WHERE track_id=$1 AND stage='embedding'",
    )
    .bind(claim.track_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ("working".into(), true, true));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        ml::embedding(&pool, claim.track_id, &provenance())
            .await
            .unwrap()
            .is_none()
    );
}
