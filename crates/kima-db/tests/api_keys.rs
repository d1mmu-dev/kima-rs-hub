use kima_core::profile::{AccountId, ProfileId};
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
    api_keys::create(
        pool,
        Uuid::now_v7(),
        AccountId::new(user),
        ProfileId::new(user),
        0,
        name,
        &hash_of(n),
        b"enc",
    )
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

    let listed = api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
        .await
        .unwrap();
    let ids: Vec<_> = listed.iter().map(|k| k.id).collect();
    assert_eq!(ids, vec![second.id, first.id], "newest first");

    assert!(
        api_keys::delete_owned(&pool, first.id, ProfileId::new(ana.id))
            .await
            .unwrap()
    );
    assert!(
        !api_keys::delete_owned(&pool, first.id, ProfileId::new(ana.id))
            .await
            .unwrap()
    );
    assert_eq!(
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .len(),
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
    let refused = api_keys::create(
        &pool,
        Uuid::now_v7(),
        AccountId::new(ana.id),
        ProfileId::new(ana.id),
        0,
        "k",
        &hash_of(999),
        b"e",
    )
    .await
    .unwrap();
    assert!(refused.is_err());
    // The cap is per account.
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
    assert_eq!(found.profile, ProfileId::new(ana.id));
    assert_eq!(found.user, ana);
    assert!(
        api_keys::find_by_hash(&pool, &hash_of(8))
            .await
            .unwrap()
            .is_none()
    );

    let secrets = api_keys::secrets_for_profile(&pool, ProfileId::new(ana.id))
        .await
        .unwrap();
    assert_eq!(secrets, vec![(key.id, b"enc".to_vec())]);

    api_keys::touch(&pool, key.id).await.unwrap();
    let listed = api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
        .await
        .unwrap();
    assert!(listed[0].last_used_at.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_owned_ignores_other_users(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bob = user(&pool, "bob").await;
    let key = add(&pool, ana.id, "phone", 1).await;
    assert!(
        !api_keys::delete_owned(&pool, key.id, ProfileId::new(bob.id))
            .await
            .unwrap()
    );
    assert_eq!(
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .len(),
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
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .len(),
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
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        api_keys::list_for_profile(&pool, ProfileId::new(bob.id))
            .await
            .unwrap()
            .len(),
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
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .len(),
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
    let stale = api_keys::create(
        &pool,
        Uuid::now_v7(),
        AccountId::new(ana.id),
        ProfileId::new(ana.id),
        0,
        "k",
        &hash_of(1),
        b"e",
    )
    .await
    .unwrap();
    assert_eq!(stale.unwrap_err(), Refused::StaleSession);
    assert!(
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .is_empty()
    );
    // The current version works.
    api_keys::create(
        &pool,
        Uuid::now_v7(),
        AccountId::new(ana.id),
        ProfileId::new(ana.id),
        1,
        "k",
        &hash_of(2),
        b"e",
    )
    .await
    .unwrap()
    .unwrap();
    // A user who is gone is a stale session too, not a foreign-key error.
    let gone_id = Uuid::now_v7();
    let gone = api_keys::create(
        &pool,
        Uuid::now_v7(),
        AccountId::new(gone_id),
        ProfileId::new(gone_id),
        0,
        "k",
        &hash_of(3),
        b"e",
    )
    .await
    .unwrap();
    assert_eq!(gone.unwrap_err(), Refused::StaleSession);
}

/// Parks every `api_keys` insert after `create`'s checks (BEFORE INSERT trigger, shared
/// advisory lock 4244) until the returned connection unlocks 4244.
async fn gate_key_inserts(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    sqlx::query(
        "CREATE FUNCTION test_key_gate() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN PERFORM pg_advisory_xact_lock_shared(4244); RETURN NEW; END $$",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER zz_test_key_gate BEFORE INSERT ON api_keys \
         FOR EACH ROW EXECUTE FUNCTION test_key_gate()",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut gate = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(4244)")
        .execute(&mut *gate)
        .await
        .unwrap();
    gate
}

/// Sessions of this database waiting on a lock whose query matches `like`.
async fn waiting(pool: &PgPool, like: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity \
         WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE $1",
    )
    .bind(like)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn kids_of(pool: &PgPool, account: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO profiles (id, user_id, name, kind) VALUES ($1, $2, 'Kids', 'kids') RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_creates_respect_the_cap(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    for n in 0..(MAX_PER_USER as u32 - 1) {
        add(&pool, ana.id, "k", n).await;
    }
    // Racing creates come from two profiles of the account: the cap and its lock are the
    // account's (M-PROF).
    let kids = kids_of(&pool, ana.id).await;
    let mut gate = gate_key_inserts(&pool).await;
    let spawn = |profile: Uuid, n: u32| {
        let pool = pool.clone();
        tokio::spawn(async move {
            api_keys::create(
                &pool,
                Uuid::now_v7(),
                AccountId::new(ana.id),
                ProfileId::new(profile),
                0,
                "k",
                &hash_of(500 + n),
                b"e",
            )
            .await
            .unwrap()
        })
    };
    let first = spawn(ana.id, 1);
    let second = spawn(kids, 2);
    // Both are in flight: one parked on the gate after counting 49, the other on the account
    // row lock (or, if the count took no lock, both on the gate with the same stale count).
    for _ in 0..500 {
        let parked = waiting(&pool, "%INSERT INTO api_keys%").await;
        let queued = waiting(&pool, "%FROM users WHERE id = $1 FOR UPDATE%").await;
        if parked + queued >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    sqlx::query("SELECT pg_advisory_unlock(4244)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let results = [first.await.unwrap(), second.await.unwrap()];
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let refused = results
        .iter()
        .filter(|r| r.as_ref().err() == Some(&api_keys::Refused::TooMany))
        .count();
    assert_eq!((ok, refused), (1, 1), "{results:?}");
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_keys k JOIN profiles pr ON pr.id = k.profile_id WHERE pr.user_id = $1",
    )
    .bind(ana.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held, MAX_PER_USER);
}

/// A profile deleted while a token is being created for it: the delete waits for the create
/// (which holds the profile row), then takes the new token with it by cascade. Without that
/// lock the insert would hit the dropped profile's foreign key and fail as a database error
/// (500) instead of a clean answer.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_deleted_during_create_is_no_database_error(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let kids = kids_of(&pool, ana.id).await;
    let mut gate = gate_key_inserts(&pool).await;
    let create = {
        let pool = pool.clone();
        tokio::spawn(async move {
            api_keys::create(
                &pool,
                Uuid::now_v7(),
                AccountId::new(ana.id),
                ProfileId::new(kids),
                0,
                "tablet",
                &hash_of(1),
                b"e",
            )
            .await
        })
    };
    for _ in 0..500 {
        if waiting(&pool, "%INSERT INTO api_keys%").await == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let delete = {
        let pool = pool.clone();
        tokio::spawn(async move {
            kima_db::profiles::delete(&pool, AccountId::new(ana.id), ProfileId::new(kids)).await
        })
    };
    // The delete either queues behind the create's hold on the profile row or (without it)
    // finishes at once.
    for _ in 0..500 {
        if delete.is_finished() || waiting(&pool, "%DELETE FROM profiles%").await == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !delete.is_finished(),
        "the delete must wait for the create's hold on the profile row"
    );
    sqlx::query("SELECT pg_advisory_unlock(4244)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let created = create.await.unwrap();
    assert!(
        matches!(
            created,
            Ok(Ok(_)) | Ok(Err(api_keys::Refused::StaleSession))
        ),
        "{created:?}"
    );
    assert_eq!(delete.await.unwrap().unwrap(), Ok(()));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE profile_id = $1")
        .bind(kids)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "the deleted profile's token went with it");
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
        api_keys::list_for_profile(&pool, ProfileId::new(ana.id))
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_with_secrets_one_query_known_or_not(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let (found, secrets) = api_keys::user_with_secrets(&pool, "ANA").await.unwrap();
    assert_eq!(found.unwrap().id, ana.id);
    assert!(secrets.is_empty(), "a user with no tokens still comes back");
    let first = add(&pool, ana.id, "phone", 1).await;
    let (found, secrets) = api_keys::user_with_secrets(&pool, "ana").await.unwrap();
    assert_eq!(found.unwrap().id, ana.id);
    let got: Vec<_> = secrets
        .into_iter()
        .map(|k| (k.id, k.profile, k.secret_enc))
        .collect();
    assert_eq!(got, vec![(first.id, ana.main_profile(), b"enc".to_vec())]);
    let (found, secrets) = api_keys::user_with_secrets(&pool, "nobody").await.unwrap();
    assert!(found.is_none() && secrets.is_empty());
}
