use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_USER: &str = "user";

/// Advisory lock key that serialises registrations ("kima" in ASCII, then 1).
const REGISTRATION_LOCK: i64 = 0x6b69_6d61_0000_0001;

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub role: String,
    pub onboarding_complete: bool,
    pub token_version: i32,
    pub created_at: OffsetDateTime,
}

#[derive(Debug)]
pub enum Registration {
    Created(User),
    Closed,
    UsernameTaken,
}

/// Creates a user. The first user becomes admin; later ones need registration to be open.
pub async fn register(
    pool: &PgPool,
    id: Uuid,
    username: &str,
    password_hash: &str,
) -> sqlx::Result<Registration> {
    let mut tx = pool.begin().await?;
    // Two simultaneous first sign-ups would otherwise both see zero users and both become admin.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK)
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM users"#)
        .fetch_one(&mut *tx)
        .await?;
    if existing > 0 {
        let open = sqlx::query_scalar!("SELECT registration_open FROM system_settings")
            .fetch_one(&mut *tx)
            .await?;
        if !open {
            return Ok(Registration::Closed);
        }
        let taken = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM users WHERE lower(username) = lower($1)) AS "taken!""#,
            username
        )
        .fetch_one(&mut *tx)
        .await?;
        if taken {
            return Ok(Registration::UsernameTaken);
        }
    }

    let role = if existing == 0 { ROLE_ADMIN } else { ROLE_USER };
    let user = sqlx::query_as!(
        User,
        r#"INSERT INTO users (id, username, password_hash, role)
           VALUES ($1, $2, $3, $4)
           RETURNING id, username, password_hash, role, onboarding_complete, token_version, created_at"#,
        id,
        username,
        password_hash,
        role
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Registration::Created(user))
}

pub async fn find_by_username(pool: &PgPool, username: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"SELECT id, username, password_hash, role, onboarding_complete, token_version, created_at
           FROM users WHERE lower(username) = lower($1)"#,
        username
    )
    .fetch_optional(pool)
    .await
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> sqlx::Result<Option<User>> {
    sqlx::query_as!(
        User,
        r#"SELECT id, username, password_hash, role, onboarding_complete, token_version, created_at
           FROM users WHERE id = $1"#,
        id
    )
    .fetch_optional(pool)
    .await
}

pub async fn count(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM users"#)
        .fetch_one(pool)
        .await
}

pub async fn registration_open(pool: &PgPool) -> sqlx::Result<bool> {
    sqlx::query_scalar!("SELECT registration_open FROM system_settings")
        .fetch_one(pool)
        .await
}

pub async fn set_onboarding_complete(pool: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE users SET onboarding_complete = true WHERE id = $1",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// The username is already in use (compared ignoring case).
#[derive(Debug, PartialEq, Eq)]
pub struct Taken;

#[derive(Debug, PartialEq, Eq)]
pub enum Deletion {
    Deleted,
    NotFound,
    LastAdmin,
}

/// All users, oldest first.
pub async fn list(pool: &PgPool) -> sqlx::Result<Vec<User>> {
    sqlx::query_as!(
        User,
        r#"SELECT id, username, password_hash, role, onboarding_complete, token_version, created_at
           FROM users ORDER BY created_at ASC, id ASC"#
    )
    .fetch_all(pool)
    .await
}

/// Creates a user on an admin's behalf. Onboarding is skipped for them, as upstream does.
pub async fn create_by_admin(
    pool: &PgPool,
    id: Uuid,
    username: &str,
    password_hash: &str,
    role: &str,
) -> sqlx::Result<Result<User, Taken>> {
    // The unique index on lower(username) arbitrates races; no conflict target means any counts.
    let user = sqlx::query_as!(
        User,
        r#"INSERT INTO users (id, username, password_hash, role, onboarding_complete)
           VALUES ($1, $2, $3, $4, true)
           ON CONFLICT DO NOTHING
           RETURNING id, username, password_hash, role, onboarding_complete, token_version, created_at"#,
        id,
        username,
        password_hash,
        role
    )
    .fetch_optional(pool)
    .await?;
    Ok(user.ok_or(Taken))
}

/// Deletes a user unconditionally. Returns whether a row existed.
pub async fn delete(pool: &PgPool, id: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Deletes a user unless they are the last admin. Serialised with registrations and other
/// deletions, so two admins deleting each other cannot leave the server without one.
pub async fn delete_unless_last_admin(pool: &PgPool, id: Uuid) -> sqlx::Result<Deletion> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK)
        .execute(&mut *tx)
        .await?;
    let Some(role) = sqlx::query_scalar!("SELECT role FROM users WHERE id = $1", id)
        .fetch_optional(&mut *tx)
        .await?
    else {
        return Ok(Deletion::NotFound);
    };
    if role == ROLE_ADMIN {
        let admins = sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!" FROM users WHERE role = $1"#,
            ROLE_ADMIN
        )
        .fetch_one(&mut *tx)
        .await?;
        if admins <= 1 {
            return Ok(Deletion::LastAdmin);
        }
    }
    sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Deletion::Deleted)
}

/// Sets a new password hash and bumps the token version, but only if the version is still
/// `seen_version` (what the caller verified the old password against). Returns the new version,
/// or `None` if the user changed meanwhile or is gone.
pub async fn set_password(
    pool: &PgPool,
    id: Uuid,
    password_hash: &str,
    seen_version: i32,
) -> sqlx::Result<Option<i32>> {
    sqlx::query_scalar!(
        "UPDATE users SET password_hash = $2, token_version = token_version + 1
         WHERE id = $1 AND token_version = $3 RETURNING token_version",
        id,
        password_hash,
        seen_version
    )
    .fetch_optional(pool)
    .await
}

pub async fn bump_token_version(pool: &PgPool, id: Uuid) -> sqlx::Result<i32> {
    sqlx::query_scalar!(
        "UPDATE users SET token_version = token_version + 1 WHERE id = $1 RETURNING token_version",
        id
    )
    .fetch_one(pool)
    .await
}
