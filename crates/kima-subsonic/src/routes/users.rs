//! Users (spec §4.8 #52-#57). Reading works with any credentials; every write needs the account
//! password, never an app token, even an admin's (D-098), and every password change revokes the
//! target's app tokens with it (D-097).

use axum::Router;
use axum::extract::State;
use kima_api::{AppState, revoke_sessions};
use kima_db::users::{self, Deletion, ROLE_ADMIN, ROLE_USER, RoleChange, User};
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::map;
use crate::params::{Params, decode_password};
use crate::response::{Answer, Node};

/// As `POST /api/auth/create-user` and `change-password`.
const MIN_PASSWORD_CHARS: usize = 6;

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getUser", get_user);
    let router = add(router, "getUsers", get_users);
    let router = add(router, "createUser", create_user);
    let router = add(router, "updateUser", update_user);
    let router = add(router, "deleteUser", delete_user);
    add(router, "changePassword", change_password)
}

fn user_not_found() -> SubsonicError {
    SubsonicError::not_found("User not found")
}

/// Whether `name` is the caller's own username (usernames are unique ignoring case).
fn is_self(caller: &SubsonicUser, name: &str) -> bool {
    caller.user.username.to_lowercase() == name.to_lowercase()
}

async fn find(app: &AppState, name: &str) -> Result<User, SubsonicError> {
    users::find_by_username(&app.pool, name)
        .await?
        .ok_or_else(user_not_found)
}

/// The decoded `password` parameter, checked as the web forms check it.
fn new_password(params: &Params) -> Result<Option<String>, SubsonicError> {
    match params.get("password") {
        None | Some("") => Ok(None),
        Some(raw) => {
            let password = decode_password(raw);
            if password.chars().count() < MIN_PASSWORD_CHARS {
                return Err(SubsonicError::new(
                    10,
                    "Password must be at least 6 characters",
                ));
            }
            Ok(Some(password))
        }
    }
}

fn required_password(params: &Params) -> Result<String, SubsonicError> {
    new_password(params)?.ok_or_else(|| SubsonicError::missing("password"))
}

async fn hash(app: &AppState, password: String) -> Result<String, SubsonicError> {
    let permit = app
        .password_permit()
        .await
        .map_err(SubsonicError::internal)?;
    tokio::task::spawn_blocking(move || {
        // Released when the work ends, even if the client hung up.
        let _permit = permit;
        kima_core::auth::hash_password(&password)
    })
    .await
    .map_err(SubsonicError::internal)
}

/// Sets the password, bumps the token version and deletes the user's app tokens, all in one
/// transaction; then ends their web sessions.
async fn set_password(app: &AppState, id: Uuid, password_hash: &str) -> Result<(), SubsonicError> {
    let mut tx = app.pool.begin().await?;
    users::set_password(&mut tx, id, password_hash, None)
        .await?
        .ok_or_else(user_not_found)?;
    tx.commit().await?;
    revoke_sessions(app, id);
    Ok(())
}

async fn get_user(
    caller: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let name = match params.get("username") {
        Some(name) if !name.is_empty() => name,
        _ => &caller.user.username,
    };
    if !is_self(&caller, name) {
        caller.require_admin()?;
    }
    let target = find(&app, name).await?;
    Ok(Answer::node(map::user(&target)))
}

async fn get_users(
    caller: SubsonicUser,
    State(app): State<AppState>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    caller.require_admin()?;
    let mut all = users::list(&app.pool).await?;
    all.sort_by(|a, b| {
        a.username
            .to_lowercase()
            .cmp(&b.username.to_lowercase())
            .then_with(|| a.username.cmp(&b.username))
    });
    Ok(Answer::node(
        Node::new("users").list("user", all.iter().map(map::user).collect()),
    ))
}

async fn create_user(
    caller: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    caller.require_password_auth()?;
    caller.require_admin()?;
    let username = params.required("username")?.trim();
    if username.is_empty() {
        return Err(SubsonicError::missing("username"));
    }
    let password = required_password(&params)?;
    let role = if params.bool("adminRole")?.unwrap_or(false) {
        ROLE_ADMIN
    } else {
        ROLE_USER
    };
    let password_hash = hash(&app, password).await?;
    users::create_by_admin(&app.pool, Uuid::now_v7(), username, &password_hash, role)
        .await?
        .map_err(|_| SubsonicError::generic("Username already exists"))?;
    Ok(Answer::ok())
}

async fn update_user(
    caller: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    caller.require_password_auth()?;
    caller.require_admin()?;
    let target = find(&app, params.required("username")?).await?;
    let role = params
        .bool("adminRole")?
        .map(|admin| if admin { ROLE_ADMIN } else { ROLE_USER });
    let password = new_password(&params)?;
    if target.id == caller.user.id && role == Some(ROLE_USER) {
        return Err(SubsonicError::not_authorized(
            "Cannot remove your own admin role",
        ));
    }
    let password_hash = match password {
        Some(password) => Some(hash(&app, password).await?),
        None => None,
    };
    // The role first: it is the change that can be refused.
    if let Some(role) = role {
        match users::set_role_unless_last_admin(&app.pool, target.id, role).await? {
            RoleChange::Changed => {}
            RoleChange::NotFound => return Err(user_not_found()),
            RoleChange::LastAdmin => {
                return Err(SubsonicError::not_authorized(
                    "Cannot remove the last admin",
                ));
            }
        }
    }
    if let Some(password_hash) = password_hash {
        set_password(&app, target.id, &password_hash).await?;
    }
    Ok(Answer::ok())
}

async fn delete_user(
    caller: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    caller.require_password_auth()?;
    caller.require_admin()?;
    let target = find(&app, params.required("username")?).await?;
    if target.id == caller.user.id {
        return Err(SubsonicError::not_authorized(
            "Cannot delete your own account",
        ));
    }
    match users::delete_unless_last_admin(&app.pool, target.id).await? {
        Deletion::Deleted => {
            revoke_sessions(&app, target.id);
            Ok(Answer::ok())
        }
        Deletion::NotFound => Err(user_not_found()),
        Deletion::LastAdmin => Err(SubsonicError::not_authorized(
            "Cannot delete the last admin",
        )),
    }
}

async fn change_password(
    caller: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    // For yourself and for others alike, even as an admin (D-098; upstream refuses only the
    // self-change).
    caller.require_password_auth().map_err(|_| {
        SubsonicError::not_authorized("Change your password on the web, not with an app key.")
    })?;
    let name = params.required("username")?;
    let password = required_password(&params)?;
    if !is_self(&caller, name) {
        caller.require_admin()?;
    }
    let target = find(&app, name).await?;
    let password_hash = hash(&app, password).await?;
    set_password(&app, target.id, &password_hash).await?;
    Ok(Answer::ok())
}
