use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::api_keys;

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_USER: &str = "user";

/// The longest username, in characters (Unicode scalar values) after trimming. Registration,
/// both admin create paths and both sign-in paths use it, so no account can hold a name that
/// cannot sign in.
pub const MAX_USERNAME_CHARS: usize = 50;

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

/// What a password sign-in looks up: the database's own case fold of the typed name (always
/// present) and the account it resolves to, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct SignInLookup {
    /// `lower(name)` under the database's locale: the fold every lookup matches on.
    pub fold: String,
    pub user: Option<User>,
}

/// [`find_by_username`] plus the database's fold of `username`, in one query whose work does
/// not depend on whether the account exists. The sign-in limiter keys on the fold, so every
/// spelling that reaches the same account shares one budget, and an unknown name is charged
/// exactly like a known one.
pub async fn sign_in_lookup(pool: &PgPool, username: &str) -> sqlx::Result<SignInLookup> {
    let row = sqlx::query!(
        r#"SELECT lower($1::text) AS "fold!", u.id AS "id?", u.username AS "username?",
                  u.password_hash AS "password_hash?", u.role AS "role?",
                  u.onboarding_complete AS "onboarding_complete?",
                  u.token_version AS "token_version?", u.created_at AS "created_at?"
           FROM (SELECT 1) AS one
           LEFT JOIN users u ON lower(u.username) = lower($1::text)"#,
        username
    )
    .fetch_one(pool)
    .await?;
    let user = match (
        row.id,
        row.username,
        row.password_hash,
        row.role,
        row.onboarding_complete,
        row.token_version,
        row.created_at,
    ) {
        (
            Some(id),
            Some(username),
            Some(password_hash),
            Some(role),
            Some(onboarding_complete),
            Some(token_version),
            Some(created_at),
        ) => Some(User {
            id,
            username,
            password_hash,
            role,
            onboarding_complete,
            token_version,
            created_at,
        }),
        _ => None,
    };
    Ok(SignInLookup {
        fold: row.fold,
        user,
    })
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

#[derive(Debug, PartialEq, Eq)]
pub enum RoleChange {
    Changed,
    NotFound,
    LastAdmin,
}

/// Sets a user's role unless that would demote the last admin. Serialised with registrations and
/// deletions under the same lock, so two admins demoting each other cannot both succeed.
pub async fn set_role_unless_last_admin(
    pool: &PgPool,
    id: Uuid,
    role: &str,
) -> sqlx::Result<RoleChange> {
    let mut tx = pool.begin().await?;
    let change = set_role_unless_last_admin_in(&mut tx, id, role).await?;
    if change == RoleChange::Changed {
        tx.commit().await?;
    }
    Ok(change)
}

/// `set_role_unless_last_admin` on the caller's transaction, which must be open: the advisory
/// lock is held until the caller commits or rolls back, so the caller can fold further changes
/// into the same transaction. Nothing is committed here.
pub async fn set_role_unless_last_admin_in(
    conn: &mut PgConnection,
    id: Uuid,
    role: &str,
) -> sqlx::Result<RoleChange> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK)
        .execute(&mut *conn)
        .await?;
    let Some(current) = sqlx::query_scalar!("SELECT role FROM users WHERE id = $1", id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(RoleChange::NotFound);
    };
    if current == ROLE_ADMIN && role != ROLE_ADMIN {
        let admins = sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!" FROM users WHERE role = $1"#,
            ROLE_ADMIN
        )
        .fetch_one(&mut *conn)
        .await?;
        if admins <= 1 {
            return Ok(RoleChange::LastAdmin);
        }
    }
    sqlx::query!("UPDATE users SET role = $2 WHERE id = $1", id, role)
        .execute(&mut *conn)
        .await?;
    Ok(RoleChange::Changed)
}

/// Sets a new password hash, bumps the token version and revokes all of the user's app tokens.
/// `seen_version` is what the caller verified the old password against: the update then applies
/// only if the version is unchanged. `None` updates unconditionally. Returns the new version,
/// or `None` if nothing matched (the user changed meanwhile or is gone).
///
/// Takes a connection so the caller owns the transaction. The token deletion runs on the same
/// connection, so tokens and password can only commit together, whoever the caller is.
pub async fn set_password(
    conn: &mut PgConnection,
    id: Uuid,
    password_hash: &str,
    seen_version: Option<i32>,
) -> sqlx::Result<Option<i32>> {
    let version = sqlx::query_scalar!(
        "UPDATE users SET password_hash = $2, token_version = token_version + 1
         WHERE id = $1 AND ($3::int IS NULL OR token_version = $3) RETURNING token_version",
        id,
        password_hash,
        seen_version
    )
    .fetch_optional(&mut *conn)
    .await?;
    if version.is_some() {
        api_keys::delete_all_for_user(conn, id).await?;
    }
    Ok(version)
}

pub async fn bump_token_version(pool: &PgPool, id: Uuid) -> sqlx::Result<i32> {
    sqlx::query_scalar!(
        "UPDATE users SET token_version = token_version + 1 WHERE id = $1 RETURNING token_version",
        id
    )
    .fetch_one(pool)
    .await
}
