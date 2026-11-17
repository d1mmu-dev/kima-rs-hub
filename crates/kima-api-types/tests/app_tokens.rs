use kima_api_types::app_tokens::{AppTokensResponse, CreateAppToken, CreatedAppToken};

#[test]
fn app_token_wire_contract_has_secret_only_in_create_response() {
    let list: AppTokensResponse = serde_json::from_value(serde_json::json!({"apiKeys":[{
        "id":"00000000-0000-0000-0000-000000000002", "name":"Phone",
        "createdAt":"2026-11-15T00:00:00Z", "lastUsed":null
    }]}))
    .unwrap();
    assert_eq!(list.api_keys[0].name, "Phone");
    assert_eq!(list.api_keys[0].last_used, None);
    assert!(
        serde_json::to_value(&list).unwrap()["apiKeys"][0]
            .get("apiKey")
            .is_none()
    );
    let created: CreatedAppToken = serde_json::from_value(serde_json::json!({
        "apiKey":"one-time-test-value", "name":"Phone", "createdAt":"2026-11-15T00:00:00Z", "message":"Saved"
    })).unwrap();
    assert_eq!(created.api_key, "one-time-test-value");
    assert_eq!(
        serde_json::to_value(CreateAppToken {
            device_name: "Phone".into()
        })
        .unwrap(),
        serde_json::json!({"deviceName":"Phone"})
    );
}
