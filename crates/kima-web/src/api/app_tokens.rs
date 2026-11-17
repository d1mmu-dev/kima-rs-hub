use super::client::{ApiClient, ApiError};
use kima_api_types::app_tokens::{AppTokensResponse, CreateAppToken, CreatedAppToken};

pub const LIST_PATH: &str = "/api/api-keys";

pub fn token_path(id: uuid::Uuid) -> String {
    format!("{LIST_PATH}/{id}")
}

pub fn create_body(name: &str) -> Result<CreateAppToken, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Device name is required".into());
    }
    if name.chars().count() > 100 {
        return Err("Device name must be at most 100 characters".into());
    }
    Ok(CreateAppToken {
        device_name: name.into(),
    })
}

pub async fn list(client: &ApiClient) -> Result<AppTokensResponse, ApiError> {
    client.get(LIST_PATH).await
}

pub async fn create(
    client: &ApiClient,
    body: &CreateAppToken,
) -> Result<CreatedAppToken, ApiError> {
    client.post(LIST_PATH, body).await
}

pub async fn revoke(client: &ApiClient, id: uuid::Uuid) -> Result<serde_json::Value, ApiError> {
    client.delete(&token_path(id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_paths_are_current_profile_not_profile_parameters() {
        assert_eq!(LIST_PATH, "/api/api-keys");
        let id = uuid::Uuid::nil();
        assert_eq!(
            token_path(id),
            "/api/api-keys/00000000-0000-0000-0000-000000000000"
        );
    }
    #[test]
    fn device_name_is_trimmed_and_bounded_without_client_account_cap() {
        assert_eq!(create_body(" Phone ").unwrap().device_name, "Phone");
        assert!(create_body(" ").is_err());
        assert!(create_body(&"ą".repeat(100)).is_ok());
        assert!(create_body(&"ą".repeat(101)).is_err());
    }
}
