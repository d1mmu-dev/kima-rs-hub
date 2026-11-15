//! Typed profile listing and switching (D-243).

use super::client::{ApiClient, ApiError};
use kima_api_types::profiles::{Profile, ProfilesResponse, SwitchProfile, SwitchResponse};

pub const LIST_PATH: &str = "/api/profiles";

fn switch_path(id: &str) -> String {
    format!("{LIST_PATH}/{id}/switch")
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
