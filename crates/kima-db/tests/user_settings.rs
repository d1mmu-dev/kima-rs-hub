use kima_db::PgPool;
use kima_db::user_settings::{self, Patch, UserSettings};
use kima_db::users::{self, Registration};
use uuid::Uuid;

async fn user(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(pool, Uuid::now_v7(), name, "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    user.id
}

async fn rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM user_settings")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_defaults_without_writing(pool: PgPool) {
    let id = user(&pool, "ana").await;
    assert_eq!(
        user_settings::get(&pool, id).await.unwrap(),
        UserSettings::defaults(id)
    );
    assert_eq!(rows(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn upsert_creates_then_merges(pool: PgPool) {
    let id = user(&pool, "ana").await;
    let a = user_settings::upsert(
        &pool,
        id,
        Patch {
            wifi_only: Some(true),
            max_cache_size_mb: Some(5),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert!(a.wifi_only);
    assert_eq!(a.max_cache_size_mb, 5);
    assert_eq!(a.playback_quality, "original");
    let b = user_settings::upsert(
        &pool,
        id,
        Patch {
            playback_quality: Some("low".into()),
            ..Patch::default()
        },
    )
    .await
    .unwrap();
    assert!(b.wifi_only);
    assert_eq!(b.max_cache_size_mb, 5);
    assert_eq!(b.playback_quality, "low");
    assert_eq!(rows(&pool).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn checks_reject_bad_values(pool: PgPool) {
    let id = user(&pool, "ana").await;
    let bad_quality = user_settings::upsert(
        &pool,
        id,
        Patch {
            playback_quality: Some("ultra".into()),
            ..Patch::default()
        },
    )
    .await;
    assert!(bad_quality.is_err());
    let bad_size = user_settings::upsert(
        &pool,
        id,
        Patch {
            max_cache_size_mb: Some(-1),
            ..Patch::default()
        },
    )
    .await;
    assert!(bad_size.is_err());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn row_cascades_with_user(pool: PgPool) {
    let id = user(&pool, "ana").await;
    user_settings::upsert(&pool, id, Patch::default())
        .await
        .unwrap();
    assert_eq!(rows(&pool).await, 1);
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(rows(&pool).await, 0);
}
