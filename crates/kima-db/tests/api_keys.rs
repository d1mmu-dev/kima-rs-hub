use kima_db::PgPool;
use kima_db::api_keys::{self, MAX_PER_USER};
use kima_db::users::{self, Registration, User};
use uuid::Uuid;

async fn user(pool: &PgPool, name: &str) -> User {
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
    user
}

fn hash_of(n: u32) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&n.to_be_bytes());
    h
}

async fn add(pool: &PgPool, user: Uuid, name: &str, n: u32) -> api_keys::ApiKey {
    api_keys::create(pool, Uuid::now_v7(), user, 0, name, &hash_of(n), b"enc")
        .await
        .unwrap()
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_list_delete_owned(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let first = add(&pool, ana.id, "phone", 1).await;
    let second = add(&pool, ana.id, "laptop", 2).await;
    assert_eq!(first.name, "phone");
    assert_eq!(first.last_used_at, None);

    let listed = api_keys::list_for_user(&pool, ana.id).await.unwrap();
    let ids: Vec<_> = listed.iter().map(|k| k.id).collect();
    assert_eq!(ids, vec![second.id, first.id], "newest first");

    assert!(
        api_keys::delete_owned(&pool, first.id, ana.id)
            .await
            .unwrap()
    );
    assert!(
        !api_keys::delete_owned(&pool, first.id, ana.id)
            .await
            .unwrap()
    );
    assert_eq!(
        api_keys::list_for_user(&pool, ana.id).await.unwrap().len(),
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fifty_first_is_too_many(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bob = user(&pool, "bob").await;
    for n in 0..MAX_PER_USER as u32 {
        add(&pool, ana.id, "k", n).await;
    }
    let refused = api_keys::create(&pool, Uuid::now_v7(), ana.id, 0, "k", &hash_of(999), b"e")
        .await
        .unwrap();
    assert!(refused.is_err());
    // The cap is per user.
    add(&pool, bob.id, "k", 1000).await;
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn find_by_hash_returns_owner(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let key = add(&pool, ana.id, "phone", 7).await;
    let found = api_keys::find_by_hash(&pool, &hash_of(7))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.key_id, key.id);
    assert_eq!(found.user, ana);
    assert!(
        api_keys::find_by_hash(&pool, &hash_of(8))
            .await
            .unwrap()
            .is_none()
    );

    let secrets = api_keys::secrets_for_user(&pool, ana.id).await.unwrap();
    assert_eq!(secrets, vec![(key.id, b"enc".to_vec())]);

    api_keys::touch(&pool, key.id).await.unwrap();
    let listed = api_keys::list_for_user(&pool, ana.id).await.unwrap();
    assert!(listed[0].last_used_at.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_owned_ignores_other_users(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bob = user(&pool, "bob").await;
    let key = add(&pool, ana.id, "phone", 1).await;
    assert!(!api_keys::delete_owned(&pool, key.id, bob.id).await.unwrap());
    assert_eq!(
        api_keys::list_for_user(&pool, ana.id).await.unwrap().len(),
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn set_password_deletes_tokens_in_same_tx(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bob = user(&pool, "bob").await;
    add(&pool, ana.id, "a1", 1).await;
    add(&pool, ana.id, "a2", 2).await;
    add(&pool, bob.id, "b1", 3).await;

    // Rolled back: nothing changes.
    let mut tx = pool.begin().await.unwrap();
    let v = users::set_password(&mut tx, ana.id, "new", Some(0))
        .await
        .unwrap();
    assert_eq!(v, Some(1));
    drop(tx);
    let after = users::find_by_id(&pool, ana.id).await.unwrap().unwrap();
    assert_eq!(after.password_hash, "hash");
    assert_eq!(after.token_version, 0);
    assert_eq!(
        api_keys::list_for_user(&pool, ana.id).await.unwrap().len(),
        2
    );

    // Committed: both change, bob's keys stay.
    let mut tx = pool.begin().await.unwrap();
    users::set_password(&mut tx, ana.id, "new", Some(0))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = users::find_by_id(&pool, ana.id).await.unwrap().unwrap();
    assert_eq!(after.password_hash, "new");
    assert_eq!(after.token_version, 1);
    assert!(
        api_keys::list_for_user(&pool, ana.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        api_keys::list_for_user(&pool, bob.id).await.unwrap().len(),
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_version_keeps_tokens(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    add(&pool, ana.id, "a1", 1).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        users::set_password(&mut conn, ana.id, "x", Some(5))
            .await
            .unwrap(),
        None
    );
    drop(conn);
    assert_eq!(
        api_keys::list_for_user(&pool, ana.id).await.unwrap().len(),
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_session_version_is_refused(pool: PgPool) {
    use api_keys::Refused;
    let ana = user(&pool, "ana").await;
    let mut conn = pool.acquire().await.unwrap();
    users::set_password(&mut conn, ana.id, "new", None)
        .await
        .unwrap();
    drop(conn);

    // A session that saw version 0 must not mint a token after the change to version 1.
    let stale = api_keys::create(&pool, Uuid::now_v7(), ana.id, 0, "k", &hash_of(1), b"e")
        .await
        .unwrap();
    assert_eq!(stale.unwrap_err(), Refused::StaleSession);
    assert!(
        api_keys::list_for_user(&pool, ana.id)
            .await
            .unwrap()
            .is_empty()
    );
    // The current version works.
    api_keys::create(&pool, Uuid::now_v7(), ana.id, 1, "k", &hash_of(2), b"e")
        .await
        .unwrap()
        .unwrap();
    // A user who is gone is a stale session too, not a foreign-key error.
    let gone = api_keys::create(
        &pool,
        Uuid::now_v7(),
        Uuid::now_v7(),
        0,
        "k",
        &hash_of(3),
        b"e",
    )
    .await
    .unwrap();
    assert_eq!(gone.unwrap_err(), Refused::StaleSession);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_creates_respect_the_cap(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    for n in 0..(MAX_PER_USER as u32 - 1) {
        add(&pool, ana.id, "k", n).await;
    }
    let handles: Vec<_> = (0..5u32)
        .map(|i| {
            let pool = pool.clone();
            tokio::spawn(async move {
                api_keys::create(
                    &pool,
                    Uuid::now_v7(),
                    ana.id,
                    0,
                    "k",
                    &hash_of(500 + i),
                    b"e",
                )
                .await
                .unwrap()
            })
        })
        .collect();
    let mut created = 0;
    for handle in handles {
        if handle.await.unwrap().is_ok() {
            created += 1;
        }
    }
    assert_eq!(created, 1);
    assert_eq!(
        api_keys::list_for_user(&pool, ana.id).await.unwrap().len() as i64,
        MAX_PER_USER
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn set_password_none_is_unconditional(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    add(&pool, ana.id, "a1", 1).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        users::set_password(&mut conn, ana.id, "x", None)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        users::set_password(&mut conn, ana.id, "y", None)
            .await
            .unwrap(),
        Some(2)
    );
    drop(conn);
    assert!(
        api_keys::list_for_user(&pool, ana.id)
            .await
            .unwrap()
            .is_empty()
    );
}
