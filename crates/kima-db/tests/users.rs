use kima_db::PgPool;
use kima_db::users::{self, ROLE_ADMIN, ROLE_USER, Registration};
use uuid::Uuid;

async fn register(pool: &PgPool, name: &str) -> Registration {
    users::register(pool, Uuid::now_v7(), name, "hash")
        .await
        .unwrap()
}

async fn open_registration(pool: &PgPool) {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn first_user_is_admin(pool: PgPool) {
    let Registration::Created(user) = register(&pool, "ana").await else {
        panic!("expected Created")
    };
    assert_eq!(user.role, ROLE_ADMIN);
    assert!(!user.onboarding_complete);
    assert_eq!(user.token_version, 0);
    assert_eq!(users::count(&pool).await.unwrap(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn registration_is_closed_after_the_first_user_by_default(pool: PgPool) {
    register(&pool, "ana").await;
    assert!(!users::registration_open(&pool).await.unwrap());
    assert!(matches!(register(&pool, "ben").await, Registration::Closed));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn open_registration_creates_plain_users(pool: PgPool) {
    register(&pool, "ana").await;
    open_registration(&pool).await;
    let Registration::Created(user) = register(&pool, "ben").await else {
        panic!("expected Created")
    };
    assert_eq!(user.role, ROLE_USER);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn usernames_are_unique_ignoring_case(pool: PgPool) {
    register(&pool, "Ana").await;
    open_registration(&pool).await;
    assert!(matches!(
        register(&pool, "ana").await,
        Registration::UsernameTaken
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn find_by_username_ignores_case(pool: PgPool) {
    register(&pool, "Ana").await;
    let found = users::find_by_username(&pool, "ANA")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.username, "Ana");
    assert!(
        users::find_by_username(&pool, "nobody")
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn find_by_id_and_complete_onboarding(pool: PgPool) {
    let Registration::Created(user) = register(&pool, "ana").await else {
        panic!()
    };
    users::set_onboarding_complete(&pool, user.id)
        .await
        .unwrap();
    let found = users::find_by_id(&pool, user.id).await.unwrap().unwrap();
    assert!(found.onboarding_complete);
    assert!(
        users::find_by_id(&pool, Uuid::now_v7())
            .await
            .unwrap()
            .is_none()
    );
}

/// Review focus 1: simultaneous first sign-ups must not produce two admins.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_first_registrations_make_exactly_one_admin(pool: PgPool) {
    let mut tasks = Vec::new();
    for i in 0..8 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            users::register(&pool, Uuid::now_v7(), &format!("user{i}"), "hash")
                .await
                .unwrap()
        }));
    }
    let mut created = 0;
    let mut closed = 0;
    for task in tasks {
        match task.await.unwrap() {
            Registration::Created(user) => {
                assert_eq!(user.role, ROLE_ADMIN);
                created += 1;
            }
            Registration::Closed => closed += 1,
            Registration::UsernameTaken => panic!("names are distinct"),
        }
    }
    assert_eq!((created, closed), (1, 7));
}
