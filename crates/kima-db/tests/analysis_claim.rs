//! Claim/lease snapshot tests for `kima_db::analysis_claim`, mirroring the relevant
//! read-side scenarios from `kima-db/tests/ml.rs`'s embedding claim tests exactly: a
//! worker's token binds to the claim it actually holds, file-identity changes move the
//! generation the next claim observes, and a stolen/lapsed token is not a live claim.
use kima_db::{
    PgPool, analysis_claim,
    stages::{self, Stage},
};
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
    sqlx::query("INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
                 VALUES ($1, $2, 'a', 't', 'flac', $1::text, 1, now(), '\\x00')")
        .bind(track).bind(album).execute(pool).await.unwrap();
    track
}

async fn working(pool: &PgPool, track: Uuid) -> analysis_claim::AnalysisClaim {
    stages::claim(pool, Stage::Analysis, 1, true)
        .await
        .unwrap()
        .remove(0);
    analysis_claim::analysis_claim(pool, track)
        .await
        .unwrap()
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_claim_before_scheduling_claims_the_row(pool: PgPool) {
    let t = track(&pool).await;
    assert!(
        analysis_claim::analysis_claim(&pool, t)
            .await
            .unwrap()
            .is_none()
    );
    let claim = working(&pool, t).await;
    assert_eq!(claim.track_id, t);
    assert_eq!(claim.file_generation, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn worker_token_binds_to_the_exact_claim_it_holds(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let bound = analysis_claim::analysis_claim_for_worker(&pool, t, claim.started_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound, claim);
    // A different (wrong) token never matches the live row.
    let wrong = claim.started_at - time::Duration::seconds(1);
    assert!(
        analysis_claim::analysis_claim_for_worker(&pool, t, wrong)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn input_for_worker_pairs_path_and_generation_across_invalidation(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let (old_path, old) = analysis_claim::analysis_input_for_worker(&pool, t, claim.started_at)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE tracks SET path='new-input.wav', size=size+1 WHERE id=$1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE track_stages SET status='working', started_at=$2, next_attempt_at=now()+interval '15 minutes' WHERE track_id=$1 AND stage='analysis'")
        .bind(t).bind(claim.started_at).execute(&pool).await.unwrap();
    let (new_path, new) = analysis_claim::analysis_input_for_worker(&pool, t, claim.started_at)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(old_path, new_path);
    assert_eq!(new_path, "new-input.wav");
    assert!(new.file_generation > old.file_generation);
    assert_eq!(old.file_generation, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lapsed_lease_is_not_a_live_claim_for_the_old_token(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1 AND stage='analysis'")
        .bind(t).execute(&pool).await.unwrap();
    assert!(
        analysis_claim::analysis_claim(&pool, t)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        analysis_claim::analysis_claim_for_worker(&pool, t, claim.started_at)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        analysis_claim::analysis_input_for_worker(&pool, t, claim.started_at)
            .await
            .unwrap()
            .is_none()
    );
}

/// A re-delivered row is a fresh token: the old worker's token must not still observe a
/// live claim, mirroring `ml.rs`'s stolen-token coverage for embedding.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redelivery_invalidates_the_old_workers_token(pool: PgPool) {
    let t = track(&pool).await;
    let old = working(&pool, t).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1 AND stage='analysis'")
        .bind(t).execute(&pool).await.unwrap();
    let current = stages::claim(&pool, Stage::Analysis, 1, true)
        .await
        .unwrap()
        .remove(0);
    assert_ne!(old.started_at, current.started_at);
    assert!(
        analysis_claim::analysis_claim_for_worker(&pool, t, old.started_at)
            .await
            .unwrap()
            .is_none()
    );
    let new_claim = analysis_claim::analysis_claim_for_worker(&pool, t, current.started_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new_claim.started_at, current.started_at);
}
