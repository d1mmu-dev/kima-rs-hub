use kima_db::{
    PgPool,
    stages::{self, FailureRecord, Stage},
};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

async fn setup(pool: &PgPool) -> stages::Claim {
    let artist = Uuid::new_v4();
    let album = Uuid::new_v4();
    let track = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id,name,name_key) VALUES ($1,'a',$2)")
        .bind(artist)
        .bind(artist.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id,artist_id,title,title_key) VALUES ($1,$2,'t',$3)")
        .bind(album)
        .bind(artist)
        .bind(album.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,codec,path,size,mtime,fingerprint) VALUES ($1,$2,'a','t','flac',$3,1,now(),'\\x00')").bind(track).bind(album).bind(track.to_string()).execute(pool).await.unwrap();
    stages::claim(pool, Stage::Embedding, 1, true)
        .await
        .unwrap()
        .remove(0)
}
fn failure() -> FailureRecord {
    FailureRecord {
        pending_until: None,
        retry_at: None,
        count_attempt: true,
        code: "FAILED".into(),
        class: None,
        message: "failure".into(),
    }
}
async fn assert_unchanged(pool: &PgPool, claim: &stages::Claim) {
    let row = sqlx::query("SELECT status, attempts, error_code, started_at FROM track_stages WHERE track_id=$1 AND stage='embedding'").bind(claim.track_id).fetch_one(pool).await.unwrap();
    assert_eq!(row.get::<String, _>("status"), "working");
    assert_eq!(row.get::<i16, _>("attempts"), 0);
    assert_eq!(row.get::<Option<String>, _>("error_code"), None);
    assert_eq!(
        row.get::<time::OffsetDateTime, _>("started_at"),
        claim.started_at
    );
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn expired_embedding_failure_without_redelivery_is_rejected(pool: PgPool) {
    let claim = setup(&pool).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1 AND stage='embedding'").bind(claim.track_id).execute(&pool).await.unwrap();
    assert!(
        !stages::report_failure(&pool, &claim, &failure())
            .await
            .unwrap()
    );
    assert_unchanged(&pool, &claim).await;
}
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_failure_expiring_while_waiting_for_stage_lock_is_rejected(pool: PgPool) {
    let claim = setup(&pool).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()+interval '2 seconds' WHERE track_id=$1 AND stage='embedding'").bind(claim.track_id).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT track_id FROM track_stages WHERE track_id=$1 AND stage='embedding' FOR UPDATE",
    )
    .bind(claim.track_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let p = pool.clone();
    let c = claim.clone();
    let task =
        tokio::spawn(async move { stages::report_failure(&p, &c, &failure()).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%track_stages%' AND pid <> pg_backend_pid())").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        loop {
            let expired: bool = sqlx::query_scalar("SELECT clock_timestamp() >= next_attempt_at FROM track_stages WHERE track_id=$1 AND stage='embedding'").bind(claim.track_id).fetch_one(&pool).await.unwrap();
            if expired { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    tx.commit().await.unwrap();
    assert!(!task.await.unwrap());
    assert_unchanged(&pool, &claim).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_stolen_token_is_rejected_and_current_token_can_report(pool: PgPool) {
    let old = setup(&pool).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1 AND stage='embedding'").bind(old.track_id).execute(&pool).await.unwrap();
    let current = stages::claim(&pool, Stage::Embedding, 1, true)
        .await
        .unwrap()
        .remove(0);
    assert_ne!(old.started_at, current.started_at);
    assert!(
        !stages::report_failure(&pool, &old, &failure())
            .await
            .unwrap()
    );
    assert_unchanged(&pool, &current).await;
    assert!(
        stages::report_failure(&pool, &current, &failure())
            .await
            .unwrap()
    );
    let r = sqlx::query(
        "SELECT status, attempts FROM track_stages WHERE track_id=$1 AND stage='embedding'",
    )
    .bind(old.track_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<i16, _>("attempts"), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn expired_fingerprint_failure_retains_existing_semantics(pool: PgPool) {
    let embedding = setup(&pool).await;
    let claim = stages::claim(&pool, Stage::Fingerprint, 1, true)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(embedding.track_id, claim.track_id);
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1 AND stage='fingerprint'").bind(claim.track_id).execute(&pool).await.unwrap();
    assert!(
        stages::report_failure(&pool, &claim, &failure())
            .await
            .unwrap()
    );
}
