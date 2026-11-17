//! `/api/profiles` (M-PROF, D-243): the account's profiles. Every profile of the account may list
//! them and switch to one; only the main profile manages them (create, rename, kind, avatar,
//! delete; owner answer Q3). A profile of another account, or an id that is not one, is 404
//! "Profile not found" on every route, for admins too (Q4): nothing tells whether it exists.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{post, put};
use kima_api_types::profiles::{
    CreateProfile, Profile, ProfileKind, ProfilesResponse, SwitchProfile, SwitchResponse,
    UpdateProfile, is_avatar_preset,
};
use kima_core::profile::{self as core, MAX_PROFILE_NAME_CHARS, MAX_PROFILES, ProfileId};
use kima_db::profiles::{self, NewProfile, ProfileError, ProfileUpdate};
use serde_json::{Value, json};

use super::auth::profile_body;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/profiles", post(create).get(list))
        .route("/profiles/{id}", put(update).delete(remove))
        .route("/profiles/{id}/switch", post(switch))
}

const NOT_FOUND: ApiError = ApiError::NotFound("Profile not found");

/// Management is the main profile's (Q3): a kids or standard profile changes nothing, not even
/// itself.
fn manager(user: &AuthUser) -> Result<(), ApiError> {
    if user.is_main_profile() {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "Only the main profile can manage profiles",
        ))
    }
}

/// A profile id from the path; anything else names no profile.
fn profile_id(raw: &str) -> Result<ProfileId, ApiError> {
    raw.parse().map_err(|_| NOT_FOUND)
}

/// The name as stored: trimmed, 1 to 50 characters.
fn name(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    let chars = trimmed.chars().count();
    if chars == 0 || chars > MAX_PROFILE_NAME_CHARS {
        return Err(ApiError::BadRequest(format!(
            "Profile name must be 1 to {MAX_PROFILE_NAME_CHARS} characters"
        )));
    }
    Ok(trimmed.to_owned())
}

/// A kind a client may ask for: kids or standard. Main exists only with its account.
fn kind(kind: ProfileKind) -> Result<core::ProfileKind, ApiError> {
    match kind {
        ProfileKind::Kids => Ok(core::ProfileKind::Kids),
        ProfileKind::Standard => Ok(core::ProfileKind::Standard),
        ProfileKind::Main | ProfileKind::Unknown => {
            Err(ApiError::BadRequest("Invalid profile kind".into()))
        }
    }
}

/// One of the preset keys (Q1), checked here; the database only checks the key's form.
fn avatar(key: Option<String>) -> Result<Option<String>, ApiError> {
    match key {
        Some(key) if !is_avatar_preset(&key) => Err(ApiError::BadRequest("Unknown avatar".into())),
        key => Ok(key),
    }
}

fn refused(error: ProfileError) -> ApiError {
    match error {
        ProfileError::Limit => ApiError::Conflict("Profile limit reached"),
        ProfileError::NameTaken => ApiError::BadRequest("Profile name already taken".into()),
        ProfileError::Main => ApiError::BadRequest("Invalid profile kind".into()),
        ProfileError::NotFound => NOT_FOUND,
        ProfileError::Invalid => ApiError::BadRequest("Invalid profile name or avatar".into()),
    }
}

async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<ProfilesResponse>, ApiError> {
    let profiles = profiles::list(&state.pool, user.account)
        .await?
        .iter()
        .map(profile_body)
        .collect::<Result<_, _>>()?;
    Ok(Json(ProfilesResponse {
        profiles,
        current: user.profile.into_uuid(),
        max: MAX_PROFILES as u32,
    }))
}

async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<CreateProfile>,
) -> Result<(StatusCode, Json<Profile>), ApiError> {
    manager(&user)?;
    let new = NewProfile {
        name: name(&body.name)?,
        kind: kind(body.kind)?,
        avatar: avatar(body.avatar)?,
    };
    // A new profile has no settings row: it reads the defaults (Q9).
    let created = profiles::create(&state.pool, user.account, new)
        .await?
        .map_err(refused)?;
    Ok((StatusCode::CREATED, Json(profile_body(&created)?)))
}

async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateProfile>,
) -> Result<Json<Profile>, ApiError> {
    manager(&user)?;
    let id = profile_id(&id)?;
    let change = ProfileUpdate {
        name: body.name.as_deref().map(name).transpose()?,
        // `main` is allowed only as a no-op on the main profile; the database layer refuses
        // every change of a main profile's kind and every promotion to main.
        kind: body
            .kind
            .map(|k| match k {
                ProfileKind::Main => Ok(core::ProfileKind::Main),
                other => kind(other),
            })
            .transpose()?,
        avatar: body.avatar.map(avatar).transpose()?,
    };
    let updated = profiles::update(&state.pool, user.account, id, change)
        .await?
        .map_err(refused)?;
    Ok(Json(profile_body(&updated)?))
}

/// Deletes the profile and, by cascade, everything it owns (D-247), then ends its sessions at
/// once: its open event streams and unredeemed tickets. Its access and refresh tokens fail
/// their next lookup (the profile is gone) and its app tokens went with the cascade.
async fn remove(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    manager(&user)?;
    let id = profile_id(&id)?;
    profiles::delete(&state.pool, user.account, id)
        .await?
        .map_err(|error| match error {
            ProfileError::Main => ApiError::BadRequest("Cannot delete the main profile".into()),
            other => refused(other),
        })?;
    crate::revoke_profile_sessions(&state, id);
    Ok(Json(json!({ "message": "Profile deleted" })))
}

/// The switch body: none, `{}` or `{"pin": ...}`; anything else is 400.
fn switch_body(bytes: &[u8]) -> Result<SwitchProfile, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(SwitchProfile::default());
    }
    // An object only: serde would also read a struct from an array (`[]` = no fields).
    match serde_json::from_slice::<Value>(bytes) {
        Ok(object @ Value::Object(_)) => {
            serde_json::from_value(object).map_err(|e| ApiError::Invalid(e.to_string()))
        }
        Ok(_) => Err(ApiError::Invalid("expected a JSON object".into())),
        Err(e) => Err(ApiError::Invalid(e.to_string())),
    }
}

/// The optional main-profile PIN (D-249) is checked here and only here: the switch is the one
/// route that issues a session for a non-main profile. No profile has a PIN yet.
fn check_pin(_target: &profiles::Profile, _pin: Option<&str>) -> Result<(), ApiError> {
    Ok(())
}

/// A token pair for another profile of the caller's account. Any profile may switch, to main as
/// well: profiles are not a security boundary until the PIN (D-240).
async fn switch(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<SwitchResponse>, ApiError> {
    let body = switch_body(&body)?;
    let id = profile_id(&id)?;
    let target = profiles::find(&state.pool, user.account, id)
        .await?
        .ok_or(NOT_FOUND)?;
    check_pin(&target, body.pin.as_deref())?;
    Ok(Json(SwitchResponse {
        token: state.jwt.issue_access(
            user.account,
            target.id,
            &user.username,
            &user.role,
            user.token_version,
        ),
        refresh_token: state
            .jwt
            .issue_refresh(user.account, target.id, user.token_version),
        profile: profile_body(&target)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_body_is_none_empty_or_pin() {
        assert_eq!(switch_body(b"").unwrap(), SwitchProfile::default());
        assert_eq!(switch_body(b" \n").unwrap(), SwitchProfile::default());
        assert_eq!(switch_body(b"{}").unwrap(), SwitchProfile::default());
        assert_eq!(
            switch_body(br#"{"pin":"1"}"#).unwrap().pin.as_deref(),
            Some("1")
        );
        for bad in [
            &br#"{"code":"1"}"#[..],
            br#"{"pin":1}"#,
            b"[]",
            b"{not json",
        ] {
            assert!(
                switch_body(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
