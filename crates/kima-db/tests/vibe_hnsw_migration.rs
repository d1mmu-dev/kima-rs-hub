//! D-184: construction200 changes only the index, including out-of-order profile upgrades.
mod vibe_common;
use kima_db::PgPool;
use serde_json::Value;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use vibe_common::*;

fn without_new_index() -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version != 41)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

async fn index(pool: &PgPool) -> (String, Vec<String>, Option<String>) {
    sqlx::query_as("SELECT pg_get_indexdef(c.oid),coalesce(c.reloptions,ARRAY[]::text[]),pg_get_expr(i.indpred,i.indrelid) FROM pg_class c JOIN pg_index i ON i.indexrelid=c.oid WHERE c.relname='track_embeddings_cosine_idx'")
        .fetch_one(pool).await.unwrap()
}

async fn snapshot(pool: &PgPool) -> Vec<(String, Value)> {
    let tables: Vec<String> = sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname='public' AND tablename <> '_sqlx_migrations' ORDER BY tablename")
        .fetch_all(pool).await.unwrap();
    let mut rows = Vec::new();
    for table in tables {
        let value: Value = sqlx::query_scalar(&format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM \"{table}\" t"))
            .fetch_one(pool).await.unwrap();
        rows.push((table, value));
    }
    rows
}

async fn assert_new_index(pool: &PgPool) {
    let (definition, options, predicate) = index(pool).await;
    assert!(
        definition.contains("USING hnsw (embedding vector_cosine_ops)"),
        "{definition}"
    );
    assert!(options.contains(&"m=16".into()), "{options:?}");
    assert!(
        options.contains(&"ef_construction=200".into()),
        "{options:?}"
    );
    assert_eq!(predicate, None, "preserve the original non-partial index");
    let applied: Vec<(i64, Vec<u8>, bool)> =
        sqlx::query_as("SELECT version,checksum,success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .unwrap();
    let expected: Vec<_> = kima_db::MIGRATOR
        .iter()
        .map(|m| (m.version, m.checksum.to_vec(), true))
        .collect();
    assert_eq!(applied, expected);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fresh_database_has_construction200(pool: PgPool) {
    assert_new_index(&pool).await;
}

#[sqlx::test(migrations = false)]
async fn existing_0040_and_0060_upgrade_preserves_every_table_and_vector(pool: PgPool) {
    // All already-applied versions must remain in the interim set, including 0060.
    let old = without_new_index();
    let base = Migrator {
        migrations: Cow::Owned(old.iter().filter(|m| m.version <= 39).cloned().collect()),
        ..Migrator::DEFAULT
    };
    base.run(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("sql/m_prof_seed_0039.sql"))
        .execute(&pool)
        .await
        .unwrap();
    old.run(&pool).await.unwrap();
    let old_index = index(&pool).await;
    assert!(
        old_index.1.is_empty(),
        "old index uses pgvector default construction64"
    );
    assert_eq!(old_index.2, None);
    let s = track(&pool).await;
    embed(&pool, s.track, &axis(0)).await;
    sqlx::query("INSERT INTO matching_feel(track_id,embedded_at,probe,valence,arousal) SELECT track_id,created_at,'upgrade',0.5,0.5 FROM track_embeddings").execute(&pool).await.unwrap();
    let before = snapshot(&pool).await;
    let old_node: i64 = sqlx::query_scalar(
        "SELECT relfilenode::bigint FROM pg_class WHERE relname='track_embeddings_cosine_idx'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let bytes: Vec<u8> =
        sqlx::query_scalar("SELECT vector_send(embedding) FROM track_embeddings WHERE track_id=$1")
            .bind(s.track)
            .fetch_one(&pool)
            .await
            .unwrap();
    let distance: f64 = sqlx::query_scalar(
        "SELECT embedding <=> $1::real[]::vector FROM track_embeddings WHERE track_id=$2",
    )
    .bind(axis(0).as_slice())
    .bind(s.track)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(distance, 0.0);
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    assert_new_index(&pool).await;
    let new_node: i64 = sqlx::query_scalar(
        "SELECT relfilenode::bigint FROM pg_class WHERE relname='track_embeddings_cosine_idx'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_ne!(
        old_node, new_node,
        "REINDEX must actually rebuild the graph"
    );
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan=off")
        .execute(&mut *connection)
        .await
        .unwrap();
    let plan: Vec<String> = sqlx::query_scalar("EXPLAIN (ANALYZE, BUFFERS) SELECT track_id FROM track_embeddings ORDER BY embedding <=> $1::real[]::vector LIMIT 20").bind(axis(0).as_slice()).fetch_all(&mut *connection).await.unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("Index Scan using track_embeddings_cosine_idx")),
        "{plan:?}"
    );
    sqlx::query("RESET enable_seqscan")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    println!("upgrade actual index plan: {plan:?}");
    assert_eq!(snapshot(&pool).await, before);
    let after_bytes: Vec<u8> =
        sqlx::query_scalar("SELECT vector_send(embedding) FROM track_embeddings WHERE track_id=$1")
            .bind(s.track)
            .fetch_one(&pool)
            .await
            .unwrap();
    let after_distance: f64 = sqlx::query_scalar(
        "SELECT embedding <=> $1::real[]::vector FROM track_embeddings WHERE track_id=$2",
    )
    .bind(axis(0).as_slice())
    .bind(s.track)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after_bytes, bytes);
    assert_eq!(after_distance, distance);
    println!(
        "0040 + 0060 default64 -> 0041 construction200: every public table, binary vector and exact distance unchanged"
    );
}
