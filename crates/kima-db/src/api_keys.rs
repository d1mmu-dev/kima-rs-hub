//! Per-app tokens for the Subsonic API (spec §5, §7.1). The table holds a lookup hash and an
//! encrypted copy of each token, never the token itself.

use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::users::User;

/// Most tokens one user can hold.
pub const MAX_PER_USER: i64 = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

/// Why [`create`] stored nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The user already holds [`MAX_PER_USER`] tokens.
    TooMany,
    /// The user is gone, or their password changed after the requesting session was issued.
    StaleSession,
}

/// Stores a token for a session that was issued at `session_version`. Under a lock on the
/// user's row it checks that version is still current (a password change bumps it and revokes
/// all tokens, so a revoked session must not mint one afterwards), then the cap, then inserts.
/// Concurrent creates are serialised by the lock and cannot overshoot the cap.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    user: Uuid,
    session_version: i32,
    name: &str,
    key_hash: &[u8; 32],
    secret_enc: &[u8],
) -> sqlx::Result<Result<ApiKey, Refused>> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar!(
        "SELECT token_version FROM users WHERE id = $1 FOR UPDATE",
        user
    )
    .fetch_optional(&mut *tx)
    .await?;
    if current != Some(session_version) {
        return Ok(Err(Refused::StaleSession));
    }
    let held = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM api_keys WHERE user_id = $1"#,
        user
    )
    .fetch_one(&mut *tx)
    .await?;
    if held >= MAX_PER_USER {
        return Ok(Err(Refused::TooMany));
    }
    let key = sqlx::query_as!(
        ApiKey,
        r#"INSERT INTO api_keys (id, user_id, name, key_hash, secret_enc)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING id, user_id, name, created_at, last_used_at"#,
        id,
        user,
        name,
        key_hash.as_slice(),
        secret_enc
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(key))
}

/// The user's tokens, newest first.
pub async fn list_for_user(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<ApiKey>> {
    sqlx::query_as!(
        ApiKey,
        r#"SELECT id, user_id, name, created_at, last_used_at
           FROM api_keys WHERE user_id = $1 ORDER BY created_at DESC, id DESC"#,
        user
    )
    .fetch_all(pool)
    .await
}

/// Revokes one of the user's tokens. False if it does not exist or belongs to someone else.
pub async fn delete_owned(pool: &PgPool, id: Uuid, user: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "DELETE FROM api_keys WHERE id = $1 AND user_id = $2",
        id,
        user
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// A token's id and its owner.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyOwner {
    pub key_id: Uuid,
    pub user: User,
}

/// Finds the token with this SHA-256 and the user it belongs to.
pub async fn find_by_hash(pool: &PgPool, hash: &[u8; 32]) -> sqlx::Result<Option<KeyOwner>> {
    let row = sqlx::query!(
        r#"SELECT k.id AS key_id,
                  u.id, u.username, u.password_hash, u.role, u.onboarding_complete,
                  u.token_version, u.created_at
           FROM api_keys k JOIN users u ON u.id = k.user_id
           WHERE k.key_hash = $1"#,
        hash.as_slice()
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| KeyOwner {
        key_id: r.key_id,
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

/// The encrypted secrets of the user's tokens, oldest first (at most [`MAX_PER_USER`]).
pub async fn secrets_for_user(pool: &PgPool, user: Uuid) -> sqlx::Result<Vec<(Uuid, Vec<u8>)>> {
    let rows = sqlx::query!(
        "SELECT id, secret_enc FROM api_keys WHERE user_id = $1
         ORDER BY created_at ASC, id ASC LIMIT $2",
        user,
        MAX_PER_USER
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.secret_enc)).collect())
}

/// Records that the token was just used.
pub async fn touch(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query!("UPDATE api_keys SET last_used_at = now() WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Revokes every token of the user. `users::set_password` calls this in its own transaction;
/// call it directly only alongside another change that must revoke them.
pub async fn delete_all_for_user(conn: &mut PgConnection, user: Uuid) -> sqlx::Result<u64> {
    let done = sqlx::query!("DELETE FROM api_keys WHERE user_id = $1", user)
        .execute(conn)
        .await?;
    Ok(done.rows_affected())
}
