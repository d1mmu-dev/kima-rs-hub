//! Migration 0064 (M5b Task 15, D-280): `system_settings.created_at`. A server upgraded from 0063 takes the time its
//! first account was made; one without accounts and a fresh install take the migration time.
use kima_db::PgPool;
use sqlx::migrate::Migrator;
use std::borrow::Cow;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const VERSION: i64 = 64;

/// Every migration up to `last` (all lower versions, so none is missing when a longer set runs).
fn migrator_upto(last: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version <= last)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

/// Every migration a server had before 0064.
fn before_release_window() -> Migrator {
    migrator_upto(VERSION - 1)
}

/// Full rows as JSON, ordered, so an upgrade can be compared value by value.
async fn rows(pool: &PgPool, sql: &str) -> serde_json::Value {
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(json_agg(r ORDER BY r::text), '[]'::json) FROM ({sql}) r"
    ))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn user(pool: &PgPool, name: &str, created_at: OffsetDateTime) {
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, created_at) VALUES ($1, $2, 'x', $3)",
    )
    .bind(Uuid::now_v7())
    .bind(name)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn installed_at(pool: &PgPool) -> OffsetDateTime {
    sqlx::query_scalar("SELECT created_at FROM system_settings")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The database clock: `now()` inside the migration is its transaction time.
async fn db_now(pool: &PgPool) -> OffsetDateTime {
    sqlx::query_scalar("SELECT now()")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn upgrade_takes_the_first_accounts_time(pool: PgPool) {
    assert!(kima_db::MIGRATOR.iter().any(|m| m.version == VERSION));
    before_release_window().run(&pool).await.unwrap();
    let has_column: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
                         WHERE table_name = 'system_settings' AND column_name = 'created_at')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!has_column, "0063 has no install time");
    let first = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() - Duration::days(400);
    // Inserted after a newer account: the earliest one counts, not the first row.
    user(&pool, "later", first + Duration::days(30)).await;
    user(&pool, "first", first).await;
    let users = rows(&pool, "SELECT * FROM users").await;
    let settings = rows(&pool, "SELECT to_jsonb(s) AS s FROM system_settings s").await;

    // Only up to 0064: a later migration may add its own columns.
    migrator_upto(VERSION).run(&pool).await.unwrap();
    assert_eq!(installed_at(&pool).await, first);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM system_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let refiled: Option<OffsetDateTime> =
        sqlx::query_scalar("SELECT albums_refiled_at FROM system_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(refiled, None, "no re-file has run since the upgrade");
    // Every other value is kept.
    assert_eq!(rows(&pool, "SELECT * FROM users").await, users);
    assert_eq!(
        rows(
            &pool,
            "SELECT (to_jsonb(s) - 'created_at' - 'albums_refiled_at') AS s FROM system_settings s"
        )
        .await,
        settings
    );
}

#[sqlx::test(migrations = false)]
async fn upgrade_without_accounts_takes_the_migration_time(pool: PgPool) {
    before_release_window().run(&pool).await.unwrap();
    let before = db_now(&pool).await;
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let at = installed_at(&pool).await;
    assert!(at >= before && at <= db_now(&pool).await, "{at}");
}

#[sqlx::test(migrations = false)]
async fn fresh_install_takes_its_setup_time(pool: PgPool) {
    let before = db_now(&pool).await;
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let at = installed_at(&pool).await;
    assert!(at >= before && at <= db_now(&pool).await, "{at}");
    // The column keeps a default for any later insert of the singleton.
    let default: Option<String> = sqlx::query_scalar(
        "SELECT column_default FROM information_schema.columns
          WHERE table_name = 'system_settings' AND column_name = 'created_at'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(default.as_deref(), Some("now()"));
}
