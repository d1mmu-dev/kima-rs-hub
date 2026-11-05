//! Round-trips each fixture captured from the running backend through the shared types:
//! parse, re-serialise, and compare the JSON values field by field (key order is not stable).

use kima_api_types::auth::{Credentials, LoginResponse, MeResponse, RegisterResponse};
use kima_api_types::error::ApiErrorBody;
use kima_api_types::onboarding::OnboardingStatus;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn round_trip<T: DeserializeOwned + Serialize>(name: &str, fixture: &str) {
    let original: Value = serde_json::from_str(fixture).expect("fixture is JSON");
    let typed: T = serde_json::from_str(fixture)
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"));
    let again: Value = serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
    assert_eq!(again, original, "{name}: DTO changed the wire shape");
}

#[test]
fn auth_types_round_trip() {
    round_trip::<Credentials>("login_request", include_str!("fixtures/login_request.json"));
    round_trip::<Credentials>(
        "onboarding_credentials",
        include_str!("fixtures/onboarding_credentials.json"),
    );
    round_trip::<LoginResponse>(
        "login_response",
        include_str!("fixtures/login_response.json"),
    );
    round_trip::<RegisterResponse>(
        "register_response",
        include_str!("fixtures/register_response.json"),
    );
    round_trip::<MeResponse>("me_response", include_str!("fixtures/me_response.json"));
}

#[test]
fn onboarding_status_round_trips() {
    round_trip::<OnboardingStatus>(
        "onboarding_status",
        include_str!("fixtures/onboarding_status.json"),
    );
}

#[test]
fn error_body_round_trips() {
    round_trip::<ApiErrorBody>("error", include_str!("fixtures/error_response.json"));
    round_trip::<ApiErrorBody>(
        "error_with_details",
        include_str!("fixtures/error_with_details_response.json"),
    );
}

#[test]
fn credentials_missing_fields_default_to_empty() {
    // The server treats absent username/password as empty strings (and rejects them itself).
    let c: Credentials = serde_json::from_str("{}").unwrap();
    assert_eq!((c.username.as_str(), c.password.as_str()), ("", ""));
}
