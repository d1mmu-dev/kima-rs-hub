//! Task 14 synthetic search vectors; actual PostgreSQL and pgvector, never a mocked DB.
mod vibe_common;
use kima_db::{
    PgPool,
    vibe::search::{ef_for_limit, search_tracks_by_embedding},
};
use vibe_common::*;

#[test]
fn ef_for_limit_is_twice_limit_clamped_100_1000() {
    for (limit, ef) in [
        (0, 100),
        (20, 100),
        (50, 100),
        (100, 200),
        (500, 1000),
        (usize::MAX, 1000),
    ] {
        assert_eq!(ef_for_limit(limit), ef);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn exact_path_below_threshold_returns_nearest_by_distance_then_id(pool: PgPool) {
    let s = track(&pool).await;
    let a = s.track;
    let b = track_in(&pool, s.album).await;
    let c = track_in(&pool, s.album).await;
    embed(&pool, a, &axis(0)).await;
    embed(&pool, b, &axis(0)).await;
    embed(&pool, c, &axis(1)).await;
    let rows = search_tracks_by_embedding(&pool, &axis(0), 3, 3)
        .await
        .unwrap();
    let mut tied = [a, b];
    tied.sort();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        [tied[0], tied[1], c]
    );
    assert_eq!(rows[0].distance, 0.0);
    assert_eq!(rows[2].distance, 1.0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn filtered_missing_and_other_model_rows_never_returned(pool: PgPool) {
    let s = track(&pool).await;
    let good = track_in(&pool, s.album).await;
    embed(&pool, s.track, &axis(0)).await;
    embed(&pool, good, &axis(1)).await;
    sqlx::query("UPDATE tracks SET missing_since=now() WHERE id=$1")
        .bind(s.track)
        .execute(&pool)
        .await
        .unwrap();
    for (model, sha, pipeline) in [
        ("other", identity().1, identity().2),
        (identity().0, vec![0; 32], identity().2),
        (identity().0, identity().1, "other"),
    ] {
        let id = track_in(&pool, s.album).await;
        embed_as(&pool, id, model, &sha, pipeline, &axis(0))
            .await
            .unwrap();
    }
    for threshold in [0, u64::MAX] {
        let rows = search_tracks_by_embedding(&pool, &axis(0), 20, threshold)
            .await
            .unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [good]);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn index_path_above_threshold_returns_same_top_limit_on_clustered_set(pool: PgPool) {
    // At only 5k rows Postgres estimates the dimensions/norm filters poorly and can
    // prefer a sorted sequential read. Force the planner in this test only so the
    // ANN assertions cannot pass by silently exercising exact search instead.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET enable_seqscan=off")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("SET enable_sort=off")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let s = track(&pool).await;
    sqlx::query("INSERT INTO tracks (id,album_id,artist_name,title,codec,path,size,mtime,fingerprint) SELECT md5('search-'||n)::uuid,$1,'synthetic','Track '||n,'flac','search/'||n,1,now(),'\\x00' FROM generate_series(1,5000) n")
        .bind(s.album).execute(&pool).await.unwrap();
    // Twenty clusters with independent deterministic nonzero jitter in every dimension.
    sqlx::query("INSERT INTO track_embeddings (track_id,file_generation,model_id,model_sha256,pipeline_version,embedding) SELECT md5('search-'||n)::uuid,0,$1,$2,$3,l2_normalize(ARRAY(SELECT (CASE WHEN k=n%20 THEN 1.0 ELSE 0.0 END + 0.025*sin(n::float8*(k+1)))::real FROM generate_series(0,511) k)::vector) FROM generate_series(1,5000) n")
        .bind(identity().0).bind(identity().1).bind(identity().2).execute(&pool).await.unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();
    let mut differences = Vec::new();
    for n in 0..20 {
        let vector:Vec<f32>=sqlx::query_scalar("SELECT embedding::real[] FROM track_embeddings WHERE track_id=md5('search-'||$1::text)::uuid").bind(n*211+1).fetch_one(&pool).await.unwrap();
        let indexed = search_tracks_by_embedding(&pool, &vector, 20, 0)
            .await
            .unwrap();
        let exact = search_tracks_by_embedding(&pool, &vector, 20, 5000)
            .await
            .unwrap();
        let indexed_ids = indexed.iter().map(|r| r.id).collect::<Vec<_>>();
        let exact_ids = exact.iter().map(|r| r.id).collect::<Vec<_>>();
        if indexed_ids != exact_ids {
            differences.push((n, indexed_ids, exact_ids));
        }
    }
    // Force an actually short ANN result: fewer eligible tracks than the requested limit.
    // 40% of vectors remain in the index but are ineligible before LIMIT, not filtered afterwards.
    sqlx::query("UPDATE tracks SET missing_since=now() WHERE path LIKE 'search/%' AND substring(path from 8)::int % 5 < 2").execute(&pool).await.unwrap();
    let indexed = search_tracks_by_embedding(&pool, &axis(0), 4000, 0)
        .await
        .unwrap();
    let exact = search_tracks_by_embedding(&pool, &axis(0), 4000, u64::MAX)
        .await
        .unwrap();
    assert_eq!(indexed.len(), 3000);
    assert_eq!(
        indexed.iter().map(|r| r.id).collect::<Vec<_>>(),
        exact.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    // Closing the actual service connections flushes their index usage statistics.
    let options = pool.connect_options();
    pool.close().await;
    let inspect = PgPool::connect_with((*options).clone()).await.unwrap();
    let scans: i64 = sqlx::query_scalar(
        "SELECT idx_scan FROM pg_stat_all_indexes WHERE indexrelname='track_embeddings_cosine_idx'",
    )
    .fetch_one(&inspect)
    .await
    .unwrap();
    assert!(
        scans > 0,
        "the policy must really exercise HNSW, not a sequential substitute"
    );
    println!(
        "20 clustered queries: {} unequal lists; {scans} actual index scans; 40%-filtered fallback returned all 3000 eligible rows",
        differences.len()
    );
    assert!(
        differences.is_empty(),
        "index/exact ID lists differ: {differences:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn settings_are_local_to_the_transaction(pool: PgPool) {
    let s = track(&pool).await;
    embed(&pool, s.track, &axis(0)).await;
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    // Load pgvector's GUC definitions on this backend before observing defaults.
    sqlx::query("SELECT vector_dims('[1,0]'::vector)")
        .execute(&single)
        .await
        .unwrap();
    let before:(String,String,String)=sqlx::query_as("SELECT current_setting('hnsw.ef_search'),current_setting('hnsw.iterative_scan'),current_setting('hnsw.max_scan_tuples')").fetch_one(&single).await.unwrap();
    search_tracks_by_embedding(&single, &axis(0), 100, 0)
        .await
        .unwrap();
    let after:(String,String,String)=sqlx::query_as("SELECT current_setting('hnsw.ef_search'),current_setting('hnsw.iterative_scan'),current_setting('hnsw.max_scan_tuples')").fetch_one(&single).await.unwrap();
    assert_eq!(after, before);
}
