//! Per-app tokens for the Subsonic API (spec §5, §7.1). The table holds a lookup hash and an
//! encrypted copy of each token, never the token itself. A token belongs to a profile (D-244);
//! the cap and the token+salt scan span the whole account.

use kima_core::profile::{AccountId, ProfileId};
use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::users::User;

/// Most tokens one account can hold, over all its profiles (token+salt auth tries each one,
/// so this bounds that scan).
pub const MAX_PER_USER: i64 = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct ApiKey {
    pub id: Uuid,
    pub profile_id: ProfileId,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

/// Why [`create`] stored nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The account already holds [`MAX_PER_USER`] tokens over all its profiles.
    TooMany,
    /// The account or the profile is gone, the profile is not the account's, or the password
    /// changed after the requesting session was issued.
    StaleSession,
}

/// Stores a token of `profile` for a session of `account` issued at `session_version`. Under a
/// lock on the ACCOUNT row it checks that version is still current (a password change bumps it
/// and revokes all tokens, so a revoked session must not mint one afterwards) and that the
/// profile still belongs to the account (holding the profile row `FOR SHARE`, so a concurrent
/// profile delete waits and cascades the new token), then the account-wide cap, then inserts.
/// Concurrent creates from any of the account's profiles are serialised by that one lock and
/// cannot overshoot the cap. Lock order: account row, then profile row.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    account: AccountId,
    profile: ProfileId,
    session_version: i32,
    name: &str,
    key_hash: &[u8; 32],
    secret_enc: &[u8],
) -> sqlx::Result<Result<ApiKey, Refused>> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar!(
        "SELECT token_version FROM users WHERE id = $1 FOR UPDATE",
        account.into_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    if current != Some(session_version) {
        return Ok(Err(Refused::StaleSession));
    }
    // FOR SHARE holds the profile until commit: a concurrent profile delete waits and then
    // takes the new token with it, instead of the insert failing on the dropped foreign key.
    let owned = sqlx::query_scalar!(
        "SELECT 1 AS one FROM profiles pr WHERE pr.id = $1 AND pr.user_id = $2 FOR SHARE",
        profile.into_uuid(),
        account.into_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();
    if !owned {
        return Ok(Err(Refused::StaleSession));
    }
    let held = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM api_keys k
           JOIN profiles pr ON pr.id = k.profile_id WHERE pr.user_id = $1"#,
        account.into_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    if held >= MAX_PER_USER {
        return Ok(Err(Refused::TooMany));
    }
    let key = sqlx::query_as!(
        ApiKey,
        r#"INSERT INTO api_keys (id, profile_id, name, key_hash, secret_enc)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING id, profile_id AS "profile_id: ProfileId", name, created_at, last_used_at"#,
        id,
        profile.into_uuid(),
        name,
        key_hash.as_slice(),
        secret_enc
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(key))
}

/// The profile's tokens, newest first.
pub async fn list_for_profile(pool: &PgPool, profile: ProfileId) -> sqlx::Result<Vec<ApiKey>> {
    sqlx::query_as!(
        ApiKey,
        r#"SELECT id, profile_id AS "profile_id: ProfileId", name, created_at, last_used_at
           FROM api_keys WHERE profile_id = $1 ORDER BY created_at DESC, id DESC"#,
        profile.into_uuid()
    )
    .fetch_all(pool)
    .await
}

/// Revokes one of the profile's tokens. False if it does not exist or belongs to another
/// profile (a sibling's included).
pub async fn delete_owned(pool: &PgPool, id: Uuid, profile: ProfileId) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "DELETE FROM api_keys WHERE id = $1 AND profile_id = $2",
        id,
        profile.into_uuid()
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// A token's id, its profile and that profile's account.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyOwner {
    pub key_id: Uuid,
    pub profile: ProfileId,
    pub user: User,
}

/// Finds the token with this SHA-256, its profile and the account that profile belongs to.
pub async fn find_by_hash(pool: &PgPool, hash: &[u8; 32]) -> sqlx::Result<Option<KeyOwner>> {
    let row = sqlx::query!(
        r#"SELECT k.id AS key_id, k.profile_id,
                  u.id, u.username, u.password_hash, u.role, u.onboarding_complete,
                  u.token_version, u.created_at
           FROM api_keys k
           JOIN profiles pr ON pr.id = k.profile_id
           JOIN users u ON u.id = pr.user_id
           WHERE k.key_hash = $1"#,
        hash.as_slice()
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| KeyOwner {
        key_id: r.key_id,
        profile: ProfileId::new(r.profile_id),
        user: User {
            id: r.id,
            username: r.username,
            password_hash: r.password_hash,
            role: r.role,
            onboarding_complete: r.onboarding_complete,
            token_version: r.token_version,
            created_at: r.created_at,
        },
    }))
}

/// The encrypted secrets of the profile's tokens, oldest first (at most [`MAX_PER_USER`]).
pub async fn secrets_for_profile(
    pool: &PgPool,
    profile: ProfileId,
) -> sqlx::Result<Vec<(Uuid, Vec<u8>)>> {
    let rows = sqlx::query!(
        "SELECT id, secret_enc FROM api_keys WHERE profile_id = $1
         ORDER BY created_at ASC, id ASC LIMIT $2",
        profile.into_uuid(),
        MAX_PER_USER
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.secret_enc)).collect())
}

/// One token's encrypted secret and the profile it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySecret {
    pub id: Uuid,
    pub profile: ProfileId,
    pub secret_enc: Vec<u8>,
}

/// The account named `username` (case-insensitive) and the token secrets of ALL its profiles
/// (at most [`MAX_PER_USER`], oldest first), in ONE query that runs the same way whether or not
/// the account exists: token+salt auth must not tell a client, by round trips, which usernames
/// exist. An unknown user gives `(None, [])`. Each secret names its profile, so the matched
/// key says which profile signs in.
pub async fn user_with_secrets(
    pool: &PgPool,
    username: &str,
) -> sqlx::Result<(Option<User>, Vec<KeySecret>)> {
    let rows = sqlx::query!(
        r#"SELECT u.id, u.username, u.password_hash, u.role, u.onboarding_complete,
                  u.token_version, u.created_at,
                  k.id AS "key_id?", k.profile_id AS "profile_id?", k.secret_enc AS "secret_enc?"
           FROM users u
           LEFT JOIN (api_keys k JOIN profiles pr ON pr.id = k.profile_id) ON pr.user_id = u.id
           WHERE lower(u.username) = lower($1)
           ORDER BY k.created_at ASC, k.id ASC
           LIMIT $2"#,
        username,
        MAX_PER_USER
    )
    .fetch_all(pool)
    .await?;
    let user = rows.first().map(|r| User {
        id: r.id,
        username: r.username.clone(),
        password_hash: r.password_hash.clone(),
        role: r.role.clone(),
        onboarding_complete: r.onboarding_complete,
        token_version: r.token_version,
        created_at: r.created_at,
    });
    let secrets = rows
        .into_iter()
        .filter_map(|r| {
            Some(KeySecret {
                id: r.key_id?,
                profile: ProfileId::new(r.profile_id?),
                secret_enc: r.secret_enc?,
            })
        })
        .collect();
    Ok((user, secrets))
}

/// Records that the token was just used.
pub async fn touch(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query!("UPDATE api_keys SET last_used_at = now() WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Revokes every token of every profile of the account. `users::set_password` calls this in
/// its own transaction; call it directly only alongside another change that must revoke them.
pub async fn delete_all_for_account(
    conn: &mut PgConnection,
    account: AccountId,
) -> sqlx::Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM api_keys k USING profiles pr WHERE pr.id = k.profile_id AND pr.user_id = $1",
        account.into_uuid()
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}
