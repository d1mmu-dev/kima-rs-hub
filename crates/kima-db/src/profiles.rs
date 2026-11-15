//! Profiles of an account (M-PROF, D-240). The rules live in the database (migration 0060):
//! at most five per account under a lock on the account row, names unique per account
//! ignoring case, exactly one main profile whose id is the account's id, and the main profile
//! only goes with its account. This module maps those refusals to [`ProfileError`].

use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use sqlx::PgPool;
use sqlx::postgres::PgDatabaseError;
use time::OffsetDateTime;
use uuid::Uuid;

/// `HINT` of the limit trigger's exception.
const HINT_LIMIT: &str = "kima:profile_limit";
/// `HINT` of the keep-main trigger's exception.
const HINT_MAIN: &str = "kima:profile_main";
const NAME_KEY: &str = "profiles_user_name_key";
const ACCOUNT_FKEY: &str = "profiles_user_id_fkey";
/// The CHECKs on what a caller writes (the kind and main-is-account checks cannot fire
/// through this module: it never writes `main` or an id).
const NAME_CHECK: &str = "profiles_name_check";
const AVATAR_CHECK: &str = "profiles_avatar_check";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: ProfileId,
    pub account: AccountId,
    pub name: String,
    pub kind: ProfileKind,
    /// A preset key; `None` = initial on a colour derived from the id.
    pub avatar: Option<String>,
    pub created_at: OffsetDateTime,
}

impl Profile {
    pub fn is_main(&self) -> bool {
        self.kind == ProfileKind::Main
    }
}

/// Why a write changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// The account already holds [`kima_core::profile::MAX_PROFILES`] profiles.
    Limit,
    /// Another profile of the account has this name (ignoring case).
    NameTaken,
    /// The write would create, remove or change the kind of a main profile.
    Main,
    /// No such profile in this account (or no such account).
    NotFound,
    /// The name (not trimmed, empty or longer than 50 characters; the database trims only
    /// spaces, so callers trim the text first) or the avatar key breaks the table's checks.
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProfile {
    pub name: String,
    pub kind: ProfileKind,
    pub avatar: Option<String>,
}

/// Fields to change; `None` keeps the stored value. `avatar: Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileUpdate {
    pub name: Option<String>,
    pub kind: Option<ProfileKind>,
    pub avatar: Option<Option<String>>,
}

struct Row {
    id: ProfileId,
    account: AccountId,
    name: String,
    kind: String,
    avatar: Option<String>,
    created_at: OffsetDateTime,
}

impl Row {
    fn into_profile(self) -> sqlx::Result<Profile> {
        let kind = self
            .kind
            .parse()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        Ok(Profile {
            id: self.id,
            account: self.account,
            name: self.name,
            kind,
            avatar: self.avatar,
            created_at: self.created_at,
        })
    }
}

/// The refusal a database error stands for, if it is one of the profile rules.
fn refusal(error: &sqlx::Error) -> Option<ProfileError> {
    let pg = error
        .as_database_error()?
        .try_downcast_ref::<PgDatabaseError>()?;
    match (pg.code(), pg.hint(), pg.constraint()) {
        ("P0001", Some(HINT_LIMIT), _) => Some(ProfileError::Limit),
        ("P0001", Some(HINT_MAIN), _) => Some(ProfileError::Main),
        ("23505", _, Some(NAME_KEY)) => Some(ProfileError::NameTaken),
        ("23503", _, Some(ACCOUNT_FKEY)) => Some(ProfileError::NotFound),
        ("23514", _, Some(NAME_CHECK | AVATAR_CHECK)) => Some(ProfileError::Invalid),
        _ => None,
    }
}

fn classify<T>(result: sqlx::Result<T>) -> sqlx::Result<Result<T, ProfileError>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(error) => match refusal(&error) {
            Some(refused) => Ok(Err(refused)),
            None => Err(error),
        },
    }
}

/// The account's profiles: main first, then by creation.
pub async fn list(pool: &PgPool, account: AccountId) -> sqlx::Result<Vec<Profile>> {
    sqlx::query_as!(
        Row,
        r#"SELECT id AS "id: ProfileId", user_id AS "account: AccountId", name, kind, avatar,
                  created_at
           FROM profiles WHERE user_id = $1
           ORDER BY kind <> 'main', created_at, id"#,
        account.into_uuid()
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Row::into_profile)
    .collect()
}

/// `profile` if it belongs to `account`; another account's profile is `None`, like a missing
/// one.
pub async fn find(
    pool: &PgPool,
    account: AccountId,
    profile: ProfileId,
) -> sqlx::Result<Option<Profile>> {
    sqlx::query_as!(
        Row,
        r#"SELECT id AS "id: ProfileId", user_id AS "account: AccountId", name, kind, avatar,
                  created_at
           FROM profiles WHERE id = $1 AND user_id = $2"#,
        profile.into_uuid(),
        account.into_uuid()
    )
    .fetch_optional(pool)
    .await?
    .map(Row::into_profile)
    .transpose()
}

/// Adds a kids or standard profile to `account`. A main profile is created only with its
/// account (by the `users` trigger), so asking for one is [`ProfileError::Main`].
pub async fn create(
    pool: &PgPool,
    account: AccountId,
    new: NewProfile,
) -> sqlx::Result<Result<Profile, ProfileError>> {
    if new.kind == ProfileKind::Main {
        return Ok(Err(ProfileError::Main));
    }
    let inserted = sqlx::query_as!(
        Row,
        r#"INSERT INTO profiles (id, user_id, name, kind, avatar) VALUES ($1, $2, $3, $4, $5)
           RETURNING id AS "id: ProfileId", user_id AS "account: AccountId", name, kind, avatar,
                     created_at"#,
        Uuid::now_v7(),
        account.into_uuid(),
        new.name,
        new.kind.as_str(),
        new.avatar
    )
    .fetch_one(pool)
    .await;
    match classify(inserted)? {
        Ok(row) => Ok(Ok(row.into_profile()?)),
        Err(refused) => Ok(Err(refused)),
    }
}

/// Changes `profile` of `account`. The main profile keeps its kind and no other profile can
/// become main ([`ProfileError::Main`]).
pub async fn update(
    pool: &PgPool,
    account: AccountId,
    profile: ProfileId,
    change: ProfileUpdate,
) -> sqlx::Result<Result<Profile, ProfileError>> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar!(
        "SELECT kind FROM profiles WHERE id = $1 AND user_id = $2 FOR UPDATE",
        profile.into_uuid(),
        account.into_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        return Ok(Err(ProfileError::NotFound));
    };
    let is_main = current == ProfileKind::Main.as_str();
    if let Some(kind) = change.kind
        && (kind == ProfileKind::Main) != is_main
    {
        return Ok(Err(ProfileError::Main));
    }
    let (set_avatar, avatar) = match change.avatar {
        Some(avatar) => (true, avatar),
        None => (false, None),
    };
    let updated = sqlx::query_as!(
        Row,
        r#"UPDATE profiles
           SET name = COALESCE($3, name),
               kind = COALESCE($4, kind),
               avatar = CASE WHEN $5 THEN $6 ELSE avatar END
           WHERE id = $1 AND user_id = $2
           RETURNING id AS "id: ProfileId", user_id AS "account: AccountId", name, kind, avatar,
                     created_at"#,
        profile.into_uuid(),
        account.into_uuid(),
        change.name,
        change.kind.map(ProfileKind::as_str),
        set_avatar,
        avatar
    )
    .fetch_one(&mut *tx)
    .await;
    match classify(updated)? {
        Ok(row) => {
            tx.commit().await?;
            Ok(Ok(row.into_profile()?))
        }
        Err(refused) => Ok(Err(refused)),
    }
}

/// Deletes `profile` of `account` and, by cascade, every row it owns. The main profile is
/// refused ([`ProfileError::Main`]); it goes only with its account.
pub async fn delete(
    pool: &PgPool,
    account: AccountId,
    profile: ProfileId,
) -> sqlx::Result<Result<(), ProfileError>> {
    let deleted = sqlx::query!(
        "DELETE FROM profiles WHERE id = $1 AND user_id = $2",
        profile.into_uuid(),
        account.into_uuid()
    )
    .execute(pool)
    .await;
    match classify(deleted)? {
        Ok(done) if done.rows_affected() == 0 => Ok(Err(ProfileError::NotFound)),
        Ok(_) => Ok(Ok(())),
        Err(refused) => Ok(Err(refused)),
    }
}

/// A profile with its account's username, as a Subsonic display name needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileOwner {
    pub profile: ProfileId,
    pub account: AccountId,
    pub username: String,
    pub name: String,
}

/// The owners of `profiles` that still exist (a deleted profile is simply absent), in no
/// particular order.
pub async fn owners(pool: &PgPool, profiles: &[ProfileId]) -> sqlx::Result<Vec<ProfileOwner>> {
    let ids: Vec<Uuid> = profiles.iter().map(|p| p.into_uuid()).collect();
    sqlx::query_as!(
        ProfileOwner,
        r#"SELECT pr.id AS "profile: ProfileId", pr.user_id AS "account: AccountId",
                  u.username, pr.name
           FROM profiles pr JOIN users u ON u.id = pr.user_id
           WHERE pr.id = ANY($1)"#,
        &ids
    )
    .fetch_all(pool)
    .await
}
