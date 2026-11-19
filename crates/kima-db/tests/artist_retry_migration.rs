//! Migration 0062: `artists.enrichment_retry_at`. A `failed` artist written before it keeps the
//! 15 min grace it was given (its last try + 15 min); nothing else gets a retry time.
use kima_db::PgPool;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use uuid::Uuid;

const VERSION: i64 = 62;

fn before_retry_at() -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version < VERSION)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

async fn artist(pool: &PgPool, status: &str, last: Option<&str>) -> Uuid {
    let id = Uuid::now_v7();
    let last = last.map_or("NULL".to_owned(), |l| format!("now() - interval '{l}'"));
    sqlx::query(&format!(
        "INSERT INTO artists (id, name, name_key, enrichment_status, enrichment_failures, last_enriched_at) \
         VALUES ($1, $2, $2, $3, 1, {last})"
    ))
    .bind(id)
    .bind(id.to_string())
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Seconds from now to the artist's retry time, `None` when it has none.
async fn due_in(pool: &PgPool, id: Uuid) -> Option<f64> {
    sqlx::query_scalar(
        "SELECT extract(epoch FROM enrichment_retry_at - now())::float8 FROM artists WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = false)]
async fn failed_artists_keep_their_grace_across_the_upgrade(pool: PgPool) {
    assert!(kima_db::MIGRATOR.iter().any(|m| m.version == VERSION));
    before_retry_at().run(&pool).await.unwrap();
    let recent = artist(&pool, "failed", Some("5 minutes")).await;
    let old = artist(&pool, "failed", Some("1 hour")).await;
    let never = artist(&pool, "failed", None).await;
    let pending = artist(&pool, "pending", Some("5 minutes")).await;
    let done = artist(&pool, "completed", Some("5 minutes")).await;

    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let applied: bool =
        sqlx::query_scalar("SELECT success FROM _sqlx_migrations WHERE version = $1")
            .bind(VERSION)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(applied, "0062 applied");

    let recent = due_in(&pool, recent).await.expect("a retry time");
    assert!((9.0 * 60.0..=10.0 * 60.0).contains(&recent), "{recent}");
    let old = due_in(&pool, old).await.expect("a retry time");
    assert!((-46.0 * 60.0..=-44.0 * 60.0).contains(&old), "{old}");
    for id in [never, pending, done] {
        assert_eq!(due_in(&pool, id).await, None);
    }
}
