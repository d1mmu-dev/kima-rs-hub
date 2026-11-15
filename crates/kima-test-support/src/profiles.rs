//! Two accounts with every kind of profile, for the M-PROF isolation matrix (plan Task 9).
//!
//! Account A (`ana`, admin) and account B (`ben`, user), each with its main profile plus a kids
//! and a standard profile, and one app token per profile. Sessions (JWTs) are issued by the
//! test binary, which owns the signing keys.

use kima_core::cipher::Cipher;
use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use kima_db::users::{self, User};
use uuid::Uuid;

/// Every seeded account's password.
pub const PASSWORD: &str = "password1";

/// One profile of a seeded account and its app token (the plain token a client sends).
#[derive(Debug, Clone)]
pub struct SeededProfile {
    pub id: ProfileId,
    pub kind: ProfileKind,
    pub name: String,
    pub app_token: String,
}

#[derive(Debug, Clone)]
pub struct SeededAccount {
    pub user: User,
    pub main: SeededProfile,
    pub kids: SeededProfile,
    pub standard: SeededProfile,
}

impl SeededAccount {
    pub fn account(&self) -> AccountId {
        self.user.account()
    }
}

#[derive(Debug, Clone)]
pub struct TwoAccounts {
    /// `ana`, an admin.
    pub a: SeededAccount,
    /// `ben`, a user.
    pub b: SeededAccount,
}

/// Seeds accounts A (`ana`, admin) and B (`ben`, user); app tokens are sealed with `cipher`.
pub async fn seed_two_accounts(pool: &PgPool, cipher: &Cipher) -> TwoAccounts {
    TwoAccounts {
        a: seed_account(pool, cipher, "ana", users::ROLE_ADMIN).await,
        b: seed_account(pool, cipher, "ben", users::ROLE_USER).await,
    }
}

/// One account `name` with role `role`, its main profile and a kids (`Kids`) and a standard
/// (`Guest`) profile, each with an app token.
pub async fn seed_account(pool: &PgPool, cipher: &Cipher, name: &str, role: &str) -> SeededAccount {
    let hash = kima_core::auth::hash_password(PASSWORD);
    let user = users::create_by_admin(pool, Uuid::now_v7(), name, &hash, role)
        .await
        .unwrap()
        .unwrap();
    let main = SeededProfile {
        id: user.main_profile(),
        kind: ProfileKind::Main,
        name: user.username.clone(),
        app_token: app_token(pool, cipher, &user, user.main_profile()).await,
    };
    let kids = profile(pool, cipher, &user, "Kids", ProfileKind::Kids).await;
    let standard = profile(pool, cipher, &user, "Guest", ProfileKind::Standard).await;
    SeededAccount {
        user,
        main,
        kids,
        standard,
    }
}

async fn profile(
    pool: &PgPool,
    cipher: &Cipher,
    user: &User,
    name: &str,
    kind: ProfileKind,
) -> SeededProfile {
    let created = profiles::create(
        pool,
        user.account(),
        NewProfile {
            name: name.into(),
            kind,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    SeededProfile {
        id: created.id,
        kind,
        name: created.name,
        app_token: app_token(pool, cipher, user, created.id).await,
    }
}

/// Mints an app token of `profile`, as `POST /api/api-keys` from that profile's session does.
async fn app_token(pool: &PgPool, cipher: &Cipher, user: &User, profile: ProfileId) -> String {
    let id = Uuid::now_v7();
    let token = kima_core::app_token::generate();
    kima_db::api_keys::create(
        pool,
        id,
        user.account(),
        profile,
        user.token_version,
        "matrix",
        &kima_core::app_token::hash(&token),
        &kima_core::app_token::seal(cipher, id, &token),
    )
    .await
    .unwrap()
    .unwrap();
    token
}
