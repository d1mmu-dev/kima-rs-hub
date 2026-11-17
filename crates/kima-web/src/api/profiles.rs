//! Typed profile listing and switching (D-243).

use super::client::{ApiClient, ApiError};
use kima_api_types::profiles::{Profile, ProfilesResponse, SwitchProfile, SwitchResponse};

pub const LIST_PATH: &str = "/api/profiles";

/// Notify persistent header subscribers after a successful, session-fenced mutation.
pub fn invalidate(revision: &vertigo::Value<u32>) {
    revision.change(|n| *n = n.wrapping_add(1));
}

fn switch_path(id: &str) -> String {
    format!("{LIST_PATH}/{id}/switch")
}

pub fn profile_path(id: uuid::Uuid) -> String {
    format!("{LIST_PATH}/{id}")
}

pub async fn create(
    client: &ApiClient,
    body: &kima_api_types::profiles::CreateProfile,
) -> Result<Profile, ApiError> {
    client.post(LIST_PATH, body).await
}

pub async fn update(
    client: &ApiClient,
    id: uuid::Uuid,
    body: &kima_api_types::profiles::UpdateProfile,
) -> Result<Profile, ApiError> {
    client.put(&profile_path(id), body).await
}

pub async fn delete(client: &ApiClient, id: uuid::Uuid) -> Result<serde_json::Value, ApiError> {
    client.delete(&profile_path(id)).await
}

pub async fn list(client: &ApiClient) -> Result<ProfilesResponse, ApiError> {
    client.get(LIST_PATH).await
}

pub async fn switch(client: &ApiClient, profile: &Profile) -> Result<SwitchResponse, ApiError> {
    client
        .post(
            &switch_path(&profile.id.to_string()),
            &SwitchProfile::default(),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::build_request;
    use kima_api_types::profiles::SwitchProfile;
    use vertigo::FetchMethod;

    #[test]
    fn invalidation_reaches_both_header_subscribers() {
        let revision = vertigo::Value::new(0u32);
        let desktop = revision.to_computed();
        let mobile = revision.to_computed();
        for expected in 1..=3 {
            invalidate(&revision);
            assert_eq!(vertigo::transaction(|c| desktop.get(c)), expected);
            assert_eq!(vertigo::transaction(|c| mobile.get(c)), expected);
        }
    }

    #[test]
    fn management_paths_use_typed_ids_without_switch_suffix() {
        let id = uuid::Uuid::nil();
        assert_eq!(
            profile_path(id),
            "/api/profiles/00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn typed_switch_has_the_profile_path_and_pin_ready_body() {
        let id = "00000000-0000-0000-0000-000000000002";
        assert_eq!(switch_path(id), format!("/api/profiles/{id}/switch"));
        let request = build_request(
            FetchMethod::POST,
            &switch_path(id),
            Some(&SwitchProfile::default()),
            Some("access".into()),
        )
        .unwrap();
        assert_eq!(request.headers["Authorization"], "Bearer access");
        assert_eq!(super::LIST_PATH, "/api/profiles");
        assert_eq!(
            serde_json::to_value(SwitchProfile::default()).unwrap(),
            serde_json::json!({})
        );
    }
}
