use kima_db::{PgPool, ml};
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_input_pairs_path_and_generation_across_invalidation(pool: PgPool) {
    let t = track(&pool).await;
    let worker = working(&pool, t).await;
    let (old_path, old) = ml::embedding_input_for_worker(&pool, t, worker.started_at)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE tracks SET path='new-input.wav', size=size+1 WHERE id=$1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE track_stages SET status='working', started_at=$2, next_attempt_at=now()+interval '15 minutes' WHERE track_id=$1 AND stage='embedding'")
        .bind(t).bind(worker.started_at).execute(&pool).await.unwrap();
    let (new_path, new) = ml::embedding_input_for_worker(&pool, t, worker.started_at)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(old_path, new_path);
    assert_eq!(new_path, "new-input.wav");
    assert!(new.file_generation > old.file_generation);
    assert!(
        !ml::complete_embedding(&pool, &old, &model("v1"), &vector())
            .await
            .unwrap()
    );
    assert!(
        ml::complete_embedding(&pool, &new, &model("v1"), &vector())
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn generation_fence_rejects_reused_stage_token_but_title_edits_preserve_results(
    pool: PgPool,
) {
    let t = track(&pool).await;
    let old = working(&pool, t).await;
    sqlx::query("UPDATE tracks SET size=size+1 WHERE id=$1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    // A duplicated timestamp must not authorize a result from the previous file.
    sqlx::query("UPDATE track_stages SET status='working', started_at=$2, next_attempt_at=now()+interval '15 minutes' WHERE track_id=$1")
        .bind(t).bind(old.started_at).execute(&pool).await.unwrap();
    assert!(
        !ml::complete_embedding(&pool, &old, &model("v1"), &vector())
            .await
            .unwrap()
    );
    let current = ml::embedding_claim(&pool, t).await.unwrap().unwrap();
    assert!(current.file_generation > old.file_generation);
    assert!(
        ml::complete_embedding(&pool, &current, &model("v1"), &vector())
            .await
            .unwrap()
    );
    sqlx::query("UPDATE tracks SET title='edited', album_id=album_id WHERE id=$1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        ml::embedding(&pool, t, &model("v1")).await.unwrap(),
        Some(vector())
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn storage_preserves_clap_unit_norm_contract_without_normalizing(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let mut v = vector();
    v[0] = 2.;
    assert!(
        ml::complete_embedding(&pool, &claim, &model("v1"), &v)
            .await
            .is_err()
    );
    v[0] = 1.0005;
    assert!(
        ml::complete_embedding(&pool, &claim, &model("v1"), &v)
            .await
            .unwrap()
    );
    assert_eq!(
        ml::embedding(&pool, t, &model("v1")).await.unwrap(),
        Some(v)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lease_expiring_while_publication_waits_for_stage_lock_is_rejected(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()+interval '1 second' WHERE track_id=$1")
        .bind(t).execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT track_id FROM track_stages WHERE track_id=$1 FOR UPDATE")
        .bind(t)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let p = pool.clone();
    let publisher = tokio::spawn(async move {
        ml::complete_embedding(&p, &claim, &model("v1"), &vector())
            .await
            .unwrap()
    });
    wait_for_lock(&pool, "SELECT track_id FROM track_stages WHERE track_id=").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let expired: bool = sqlx::query_scalar(
                "SELECT clock_timestamp() >= next_attempt_at FROM track_stages WHERE track_id=$1",
            )
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
            if expired {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(!publisher.await.unwrap());
}

#[sqlx::test(migrations = false)]
async fn upgrade_from_m3_preserves_existing_track_and_ledger(pool: PgPool) {
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version < 24)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await.unwrap();
    let t = track(&pool).await;
    sqlx::query("INSERT INTO track_stages(track_id,stage,status) VALUES ($1,'fingerprint','done')")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let generation: i64 = sqlx::query_scalar("SELECT ml_generation FROM tracks WHERE id=$1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(generation, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM track_stages WHERE track_id=$1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalid_vectors_and_provenance_roll_back_success(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let mut bad_vectors = vec![vec![1.; 511], vec![1.; 513], vec![0.; 512]];
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut v = vector();
        v[7] = value;
        bad_vectors.push(v);
    }
    for v in bad_vectors {
        assert!(
            ml::complete_embedding(&pool, &claim, &model("v1"), &v)
                .await
                .is_err()
        );
        let status: String =
            sqlx::query_scalar("SELECT status FROM track_stages WHERE track_id=$1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "working");
    }
    for p in [
        model(" "),
        ml::Provenance {
            model_id: " ".into(),
            ..model("v1")
        },
    ] {
        assert!(
            ml::complete_embedding(&pool, &claim, &p, &vector())
                .await
                .is_err()
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        ml::complete_embedding(&pool, &claim, &model("v1"), &vector())
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn superseded_expired_and_concurrent_reports_do_not_win(pool: PgPool) {
    let t = track(&pool).await;
    let old = working(&pool, t).await;
    let new = working(&pool, t).await;
    assert_ne!(old.started_at, new.started_at);
    assert!(
        !ml::complete_embedding(&pool, &old, &model("old"), &vector())
            .await
            .unwrap()
    );
    let provenance = model("v1");
    let v = vector();
    let (a, b) = tokio::join!(
        ml::complete_embedding(&pool, &new, &provenance, &v),
        ml::complete_embedding(&pool, &new, &provenance, &v)
    );
    assert_ne!(a.unwrap(), b.unwrap());
    let expired = working(&pool, t).await;
    sqlx::query("UPDATE track_stages SET next_attempt_at=clock_timestamp()-interval '1 second' WHERE track_id=$1")
        .bind(t).execute(&pool).await.unwrap();
    assert!(ml::embedding_claim(&pool, t).await.unwrap().is_none());
    assert!(
        !ml::complete_embedding(&pool, &expired, &model("old"), &vector())
            .await
            .unwrap()
    );
    let replacement = working(&pool, t).await;
    assert!(
        ml::complete_embedding(&pool, &replacement, &model("v2"), &vector())
            .await
            .unwrap()
    );
    assert!(
        ml::embedding(&pool, t, &model("v1"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        ml::embedding(&pool, t, &model("v2")).await.unwrap(),
        Some(vector())
    );
}

async fn wait_for_lock(pool: &PgPool, query_prefix: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $1)")
                .bind(format!("{query_prefix}%")).fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await.expect("database did not observe expected lock waiter");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn queued_publishers_have_one_winner_and_cannot_overwrite_it(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT track_id FROM track_stages WHERE track_id=$1 FOR UPDATE")
        .bind(t)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let first_claim = claim.clone();
    let p = pool.clone();
    let first = tokio::spawn(async move {
        ml::complete_embedding(&p, &first_claim, &model("winner"), &vector())
            .await
            .unwrap()
    });
    wait_for_lock(&pool, "SELECT track_id FROM track_stages WHERE track_id=").await;
    let p = pool.clone();
    let second = tokio::spawn(async move {
        let mut other = vector();
        other[0] = 0.;
        other[1] = 1.;
        ml::complete_embedding(&p, &claim, &model("loser"), &other)
            .await
            .unwrap()
    });
    wait_for_lock(&pool, "SELECT id FROM tracks WHERE id=").await;
    blocker.commit().await.unwrap();
    assert!(first.await.unwrap());
    assert!(!second.await.unwrap());
    assert_eq!(
        ml::embedding(&pool, t, &model("winner")).await.unwrap(),
        Some(vector())
    );
    assert!(
        ml::embedding(&pool, t, &model("loser"))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalidation_committing_before_waiting_publication_rejects_old_generation(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE tracks SET size=size+1 WHERE id=$1")
        .bind(t)
        .execute(&mut *tx)
        .await
        .unwrap();
    let p = pool.clone();
    let publisher = tokio::spawn(async move {
        ml::complete_embedding(&p, &claim, &model("v1"), &vector())
            .await
            .unwrap()
    });
    wait_for_lock(&pool, "SELECT id FROM tracks WHERE id=").await;
    tx.commit().await.unwrap();
    assert!(!publisher.await.unwrap());
    assert!(
        ml::embedding(&pool, t, &model("v1"))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn invalidation_waiting_behind_publication_removes_just_committed_result(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT track_id FROM track_stages WHERE track_id=$1 FOR UPDATE")
        .bind(t)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let p = pool.clone();
    let publisher = tokio::spawn(async move {
        ml::complete_embedding(&p, &claim, &model("v1"), &vector())
            .await
            .unwrap()
    });
    wait_for_lock(&pool, "SELECT track_id FROM track_stages WHERE track_id=").await;
    let p = pool.clone();
    let invalidator = tokio::spawn(async move {
        sqlx::query("UPDATE tracks SET size=size+1 WHERE id=$1")
            .bind(t)
            .execute(&p)
            .await
            .unwrap();
    });
    wait_for_lock(&pool, "UPDATE tracks SET size=size+1").await;
    tx.commit().await.unwrap();
    assert!(publisher.await.unwrap());
    invalidator.await.unwrap();
    assert!(
        ml::embedding(&pool, t, &model("v1"))
            .await
            .unwrap()
            .is_none()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn explicit_scanner_reopen_fences_same_metadata_and_rolls_back(pool: PgPool) {
    let t = track(&pool).await;
    let claim = working(&pool, t).await;
    assert!(
        ml::complete_embedding(&pool, &claim, &model("v1"), &vector())
            .await
            .unwrap()
    );
    let mut tx = pool.begin().await.unwrap();
    kima_db::stages::reopen_for_changed_file(&mut tx, t)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        ml::embedding(&pool, t, &model("v1")).await.unwrap(),
        Some(vector())
    );
    let inflight = working(&pool, t).await;
    let mut tx = pool.begin().await.unwrap();
    kima_db::stages::reopen_for_changed_file(&mut tx, t)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_embeddings WHERE track_id=$1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        !ml::complete_embedding(&pool, &inflight, &model("v1"), &vector())
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_identity_changes_invalidate_results_and_inflight_claims(pool: PgPool) {
    for change in [
        "size=size+1",
        "mtime=mtime+interval '1 second'",
        "fingerprint='\\x01'",
        "path=path||'.moved'",
        "missing_since=now()",
    ] {
        let t = track(&pool).await;
        let claim = working(&pool, t).await;
        assert!(
            ml::complete_embedding(&pool, &claim, &model("v1"), &vector())
                .await
                .unwrap()
        );
        let inflight = working(&pool, t).await;
        sqlx::query(&format!("UPDATE tracks SET {change} WHERE id=$1"))
            .bind(t)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !ml::complete_embedding(&pool, &inflight, &model("v1"), &vector())
                .await
                .unwrap(),
            "{change}"
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM track_embeddings WHERE track_id=$1")
                .bind(t)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "{change}");
        let generation: i64 = sqlx::query_scalar("SELECT ml_generation FROM tracks WHERE id=$1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(generation, inflight.file_generation + 1);
        if change == "missing_since=now()" {
            sqlx::query("UPDATE tracks SET missing_since=NULL WHERE id=$1")
                .bind(t)
                .execute(&pool)
                .await
                .unwrap();
            assert!(
                !ml::complete_embedding(&pool, &inflight, &model("v1"), &vector())
                    .await
                    .unwrap()
            );
        }
    }
}

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

fn model(version: &str) -> ml::Provenance {
    ml::Provenance {
        model_id: "synthetic-clap-test".into(),
        model_sha256: [42; 32],
        pipeline_version: version.into(),
    }
}

async fn working(pool: &PgPool, track: Uuid) -> ml::EmbeddingClaim {
    sqlx::query("INSERT INTO track_stages (track_id, stage, status, started_at, next_attempt_at)
                 VALUES ($1, 'embedding', 'working', clock_timestamp(), now() + interval '15 minutes')
                 ON CONFLICT (track_id, stage) DO UPDATE SET status = 'working',
                     started_at = clock_timestamp(), next_attempt_at = now() + interval '15 minutes'")
        .bind(track).execute(pool).await.unwrap();
    ml::embedding_claim(pool, track).await.unwrap().unwrap()
}

fn vector() -> Vec<f32> {
    let mut v = vec![0.; 512];
    v[0] = 1.;
    v
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn current_claim_atomically_persists_vector_provenance_and_done(pool: PgPool) {
    let t = track(&pool).await;
    assert!(ml::embedding_claim(&pool, t).await.unwrap().is_none());
    let claim = working(&pool, t).await;
    assert!(
        ml::complete_embedding(&pool, &claim, &model("v1"), &vector())
            .await
            .unwrap()
    );
    let source: Option<String> = sqlx::query_scalar(
        "SELECT source FROM track_stages WHERE track_id=$1 AND stage='embedding'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(source.as_deref(), Some("clap"));
    assert_eq!(
        ml::embedding(&pool, t, &model("v1")).await.unwrap(),
        Some(vector())
    );
    assert!(
        ml::embedding(&pool, t, &model("v2"))
            .await
            .unwrap()
            .is_none()
    );
    let mut different_model = model("v1");
    different_model.model_sha256 = [43; 32];
    assert!(
        ml::embedding(&pool, t, &different_model)
            .await
            .unwrap()
            .is_none()
    );
    let status: String = sqlx::query_scalar(
        "SELECT status FROM track_stages WHERE track_id=$1 AND stage='embedding'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "done");
    assert!(
        !ml::complete_embedding(&pool, &claim, &model("v2"), &vector())
            .await
            .unwrap()
    );
    sqlx::query("DELETE FROM tracks WHERE id=$1")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM track_embeddings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_database_has_clap_storage_and_cosine_hnsw(pool: PgPool) {
    let definition: Option<String> = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes WHERE indexname = 'track_embeddings_cosine_idx'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(definition.is_some(), "missing CLAP cosine HNSW index");
    let definition = definition.unwrap();
    assert!(definition.contains("USING hnsw (embedding vector_cosine_ops)"));
    let ty: String = sqlx::query_scalar(
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute
         WHERE attrelid = 'track_embeddings'::regclass AND attname = 'embedding'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ty, "vector(512)");
}
