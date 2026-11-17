//! `kima_db::profiles` (M-PROF Task 2): listing, lookup, create/update/delete and the error
//! mapping of the database rules from migration 0060.

use std::time::Duration;

use kima_core::profile::{AccountId, MAX_PROFILES, ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile, ProfileError, ProfileUpdate};
use uuid::Uuid;

async fn account(pool: &PgPool, name: &str) -> AccountId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    AccountId::new(id)
}

fn new(name: &str, kind: ProfileKind) -> NewProfile {
    NewProfile {
        name: name.to_string(),
        kind,
        avatar: None,
    }
}

async fn create_ok(pool: &PgPool, account: AccountId, name: &str, kind: ProfileKind) -> ProfileId {
    profiles::create(pool, account, new(name, kind))
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn count(pool: &PgPool, account: AccountId) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM profiles WHERE user_id = $1")
        .bind(account.into_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_until_limit_then_limit_error(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    for i in 1..MAX_PROFILES {
        let kind = if i % 2 == 0 {
            ProfileKind::Standard
        } else {
            ProfileKind::Kids
        };
        let created = profiles::create(
            &pool,
            a,
            NewProfile {
                name: format!("P{i}"),
                kind,
                avatar: Some("fox".into()),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(created.account, a);
        assert_eq!(created.kind, kind);
        assert_eq!(created.avatar.as_deref(), Some("fox"));
        assert_ne!(created.id, ProfileId::main_of(a));
    }
    assert_eq!(count(&pool, a).await, MAX_PROFILES as i64);
    let refused = profiles::create(&pool, a, new("Sixth", ProfileKind::Kids))
        .await
        .unwrap();
    assert_eq!(refused, Err(ProfileError::Limit));
    assert_eq!(count(&pool, a).await, MAX_PROFILES as i64);
    // Asking for a second main profile never reaches the database.
    let b = account(&pool, "Ben").await;
    let main = profiles::create(&pool, b, new("Other main", ProfileKind::Main))
        .await
        .unwrap();
    assert_eq!(main, Err(ProfileError::Main));
}

/// Park every profile insert on a gate (advisory lock 4243) that fires after the limit
/// trigger (`zz_` sorts after `profiles_limit`): an insert that counted without the account
/// lock would wait here with a stale count while the other one counts as well.
async fn gate_inserts_after_limit_check(
    pool: &PgPool,
) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    sqlx::query(
        "CREATE FUNCTION test_profile_gate() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN PERFORM pg_advisory_xact_lock_shared(4243); RETURN NEW; END $$",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER zz_test_profile_gate BEFORE INSERT ON profiles \
         FOR EACH ROW EXECUTE FUNCTION test_profile_gate()",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut gate = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(4243)")
        .execute(&mut *gate)
        .await
        .unwrap();
    gate
}

/// Waits until `n` sessions inserting into `profiles` are blocked on a lock (the gate or the
/// account row).
async fn blocked_inserts(pool: &PgPool, n: i64) {
    for _ in 0..500 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock' \
               AND query LIKE '%INSERT INTO profiles%'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{n} profile inserts never blocked");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_creates_respect_limit(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    for i in 1..MAX_PROFILES - 1 {
        create_ok(&pool, a, &format!("P{i}"), ProfileKind::Kids).await;
    }
    assert_eq!(count(&pool, a).await, MAX_PROFILES as i64 - 1);

    let mut gate = gate_inserts_after_limit_check(&pool).await;
    let spawn = |name: &'static str| {
        let pool = pool.clone();
        tokio::spawn(async move {
            profiles::create(&pool, a, new(name, ProfileKind::Standard))
                .await
                .unwrap()
        })
    };
    let first = spawn("Left");
    let second = spawn("Right");
    // Both are in flight: one parked on the gate, the other on the account row lock (or, if
    // the limit check took no lock, both on the gate with a stale count).
    blocked_inserts(&pool, 2).await;
    sqlx::query("SELECT pg_advisory_unlock(4243)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let results = [first.await.unwrap(), second.await.unwrap()];
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let limit = results
        .iter()
        .filter(|r| **r == Err(ProfileError::Limit))
        .count();
    assert_eq!((ok, limit), (1, 1), "{results:?}");
    assert_eq!(count(&pool, a).await, MAX_PROFILES as i64);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rename_to_taken_name_is_name_taken(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    let kids = create_ok(&pool, a, "Kids", ProfileKind::Kids).await;
    let guest = create_ok(&pool, a, "Guest", ProfileKind::Standard).await;
    let rename = |name: &str| ProfileUpdate {
        name: Some(name.to_string()),
        ..ProfileUpdate::default()
    };
    assert_eq!(
        profiles::update(&pool, a, guest, rename("KIDS"))
            .await
            .unwrap(),
        Err(ProfileError::NameTaken)
    );
    // The main profile's name (the username) is taken too.
    assert_eq!(
        profiles::update(&pool, a, guest, rename("ana"))
            .await
            .unwrap(),
        Err(ProfileError::NameTaken)
    );
    assert_eq!(
        profiles::create(&pool, a, new("guest", ProfileKind::Kids))
            .await
            .unwrap(),
        Err(ProfileError::NameTaken)
    );
    // Renaming to a different case of its own name is fine.
    let renamed = profiles::update(&pool, a, kids, rename("KIDS"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed.name, "KIDS");
    let both = profiles::update(
        &pool,
        a,
        guest,
        ProfileUpdate {
            name: Some("Teen".into()),
            kind: Some(ProfileKind::Kids),
            avatar: Some(Some("owl".into())),
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        (both.name.as_str(), both.kind, both.avatar.as_deref()),
        ("Teen", ProfileKind::Kids, Some("owl"))
    );
    let cleared = profiles::update(
        &pool,
        a,
        guest,
        ProfileUpdate {
            avatar: Some(None),
            ..ProfileUpdate::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!((cleared.name.as_str(), cleared.avatar), ("Teen", None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn update_kind_main_is_refused(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    let kids = create_ok(&pool, a, "Kids", ProfileKind::Kids).await;
    let to_main = ProfileUpdate {
        kind: Some(ProfileKind::Main),
        ..ProfileUpdate::default()
    };
    assert_eq!(
        profiles::update(&pool, a, kids, to_main).await.unwrap(),
        Err(ProfileError::Main)
    );
    let main_to_kids = ProfileUpdate {
        kind: Some(ProfileKind::Kids),
        ..ProfileUpdate::default()
    };
    assert_eq!(
        profiles::update(&pool, a, ProfileId::main_of(a), main_to_kids)
            .await
            .unwrap(),
        Err(ProfileError::Main)
    );
    // Main keeping its kind and renaming itself is fine.
    let main = profiles::update(
        &pool,
        a,
        ProfileId::main_of(a),
        ProfileUpdate {
            name: Some("Mum".into()),
            kind: Some(ProfileKind::Main),
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!((main.name.as_str(), main.kind), ("Mum", ProfileKind::Main));
    let kinds: Vec<ProfileKind> = profiles::list(&pool, a)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.kind)
        .collect();
    assert_eq!(kinds, vec![ProfileKind::Main, ProfileKind::Kids]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn find_with_foreign_account_is_none(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    let b = account(&pool, "Ben").await;
    let kids = create_ok(&pool, a, "Kids", ProfileKind::Kids).await;
    let found = profiles::find(&pool, a, kids).await.unwrap().unwrap();
    assert_eq!((found.id, found.account), (kids, a));
    assert!(profiles::find(&pool, b, kids).await.unwrap().is_none());
    assert!(
        profiles::find(&pool, b, ProfileId::main_of(a))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        profiles::find(&pool, a, ProfileId::new(Uuid::now_v7()))
            .await
            .unwrap()
            .is_none()
    );
    // Writes through a foreign account are NotFound and change nothing.
    let rename = ProfileUpdate {
        name: Some("Mine".into()),
        ..ProfileUpdate::default()
    };
    assert_eq!(
        profiles::update(&pool, b, kids, rename).await.unwrap(),
        Err(ProfileError::NotFound)
    );
    assert_eq!(
        profiles::delete(&pool, b, kids).await.unwrap(),
        Err(ProfileError::NotFound)
    );
    assert_eq!(
        profiles::find(&pool, a, kids).await.unwrap().unwrap().name,
        "Kids"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_removes_one_profile_but_never_main(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    let kids = create_ok(&pool, a, "Kids", ProfileKind::Kids).await;
    assert_eq!(
        profiles::delete(&pool, a, ProfileId::main_of(a))
            .await
            .unwrap(),
        Err(ProfileError::Main)
    );
    assert_eq!(profiles::delete(&pool, a, kids).await.unwrap(), Ok(()));
    assert_eq!(
        profiles::delete(&pool, a, kids).await.unwrap(),
        Err(ProfileError::NotFound)
    );
    assert_eq!(count(&pool, a).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn list_orders_main_first(pool: PgPool) {
    let a = account(&pool, "Zed").await;
    let b = account(&pool, "Other").await;
    // Created after the account, named to sort before it, so neither name nor time puts
    // main first by accident.
    let first = create_ok(&pool, a, "Alpha", ProfileKind::Standard).await;
    let second = create_ok(&pool, a, "Beta", ProfileKind::Kids).await;
    sqlx::query("UPDATE profiles SET created_at = '2000-01-01' WHERE id = $1")
        .bind(first.into_uuid())
        .execute(&pool)
        .await
        .unwrap();
    create_ok(&pool, b, "Not mine", ProfileKind::Kids).await;
    let listed = profiles::list(&pool, a).await.unwrap();
    let ids: Vec<ProfileId> = listed.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![ProfileId::main_of(a), first, second]);
    assert_eq!(listed[0].name, "Zed");
    assert!(listed[0].is_main());
    assert!(listed.iter().skip(1).all(|p| !p.is_main()));
    assert!(listed.iter().all(|p| p.account == a));
}

/// Names and avatar keys the table's checks refuse come back as `Invalid`, not as a database
/// error (which the API would answer with 500).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bad_name_or_avatar_is_invalid(pool: PgPool) {
    let a = account(&pool, "Ana").await;
    let long = "x".repeat(51);
    for name in ["", " Kids", "Kids ", long.as_str()] {
        let refused = profiles::create(&pool, a, new(name, ProfileKind::Kids))
            .await
            .unwrap();
        assert_eq!(refused, Err(ProfileError::Invalid), "{name:?}");
    }
    let bad_avatar = NewProfile {
        avatar: Some("Not A Key!".into()),
        ..new("Kids", ProfileKind::Kids)
    };
    assert_eq!(
        profiles::create(&pool, a, bad_avatar).await.unwrap(),
        Err(ProfileError::Invalid)
    );

    let kids = create_ok(&pool, a, "Kids", ProfileKind::Kids).await;
    for change in [
        ProfileUpdate {
            name: Some(String::new()),
            ..Default::default()
        },
        ProfileUpdate {
            avatar: Some(Some("UPPER".into())),
            ..Default::default()
        },
    ] {
        assert_eq!(
            profiles::update(&pool, a, kids, change.clone())
                .await
                .unwrap(),
            Err(ProfileError::Invalid),
            "{change:?}"
        );
    }
    assert_eq!(count(&pool, a).await, 2);
}
