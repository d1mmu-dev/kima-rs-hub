//! Synthetic additive-key regression plus real captures (pending parent Docker capture).
use kima_api_types::auth::{LoginResponse, MeResponse};
use kima_api_types::playlists::PlaylistDetail;
use kima_api_types::profiles::{Profile, ProfilesResponse, SwitchResponse};
use kima_api_types::settings::UserSettings;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn round_trip<T: DeserializeOwned + Serialize>(wire: &Value) {
    let typed: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), *wire);
}

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}_response.json"));
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn additive_profile_fields_survive_dto_round_trip() {
    // Explicitly synthetic DTO inputs, NOT captured responses. Original legacy fixtures remain.
    let id = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    let mut settings = fixture("settings");
    settings["profileId"] = json!(id);
    round_trip::<UserSettings>(&settings);
    let mut playlist = fixture("playlist_detail");
    playlist["profileId"] = json!(id);
    playlist["profileName"] = json!("Synthetic main");
    round_trip::<PlaylistDetail>(&playlist);
    playlist["profileName"] = Value::Null;
    round_trip::<PlaylistDetail>(&playlist);
}

#[test]
fn additive_playback_profile_id_survives_dto_round_trip() {
    // Synthetic additive extension of the pre-M-PROF capture, not a new capture.
    let mut state = fixture("playback_state");
    state["profileId"] = state["userId"].clone();
    round_trip::<kima_api_types::playback_state::PlaybackState>(&state);
}

#[test]
#[ignore = "parent must run KIMA_CAPTURE_SET=profiles tools/capture-api-fixtures.sh first"]
fn captured_profiles_round_trip_and_legacy_additive_contract() {
    let login = fixture("profiles_login");
    let me = fixture("profiles_me");
    let list = fixture("profiles_list");
    let create = fixture("profiles_create");
    let update = fixture("profiles_update");
    let switch = fixture("profiles_switch");
    let me_switched = fixture("profiles_me_switched");
    let settings = fixture("profiles_settings");
    let settings_switched = fixture("profiles_settings_switched");
    let playlist = fixture("profiles_playlist");
    let playlist_switched = fixture("profiles_playlist_switched");
    let after = fixture("profiles_list_after_delete");
    round_trip::<LoginResponse>(&login);
    round_trip::<MeResponse>(&me);
    round_trip::<MeResponse>(&me_switched);
    for value in [&list, &after] {
        round_trip::<ProfilesResponse>(value);
    }
    for value in [&create, &update] {
        round_trip::<Profile>(value);
    }
    round_trip::<SwitchResponse>(&switch);
    for value in [&settings, &settings_switched] {
        round_trip::<UserSettings>(value);
    }
    for value in [&playlist, &playlist_switched] {
        round_trip::<PlaylistDetail>(value);
    }
    for (new, old, added) in [
        (&login, "login", vec!["profile"]),
        (&me, "me", vec!["profile"]),
        (&settings, "settings", vec!["profileId"]),
        (
            &playlist,
            "playlist_detail",
            vec!["profileId", "profileName"],
        ),
    ] {
        let old = fixture(old);
        let old_keys = keys(&old);
        let new_keys = keys(new);
        assert!(old_keys.is_subset(&new_keys));
        assert_eq!(
            new_keys
                .difference(&old_keys)
                .copied()
                .collect::<BTreeSet<_>>(),
            added.into_iter().collect()
        );
    }
    let account = &me["id"];
    let main = &me["profile"]["id"];
    let sibling = &create["id"];
    assert_eq!(account, main);
    assert_ne!(main, sibling);
    assert_eq!(login["user"]["id"], *account);
    assert_eq!(login["profile"], me["profile"]);
    assert_eq!(list["current"], *main);
    assert_eq!(list["profiles"].as_array().unwrap().len(), 2);
    assert_eq!(list["profiles"][0], me["profile"]);
    assert_eq!(list["profiles"][1], update);
    assert_eq!(list["max"], 5);
    assert_eq!(create["kind"], "kids");
    assert_eq!(update["kind"], "standard");
    assert_eq!(update["id"], *sibling);
    assert_eq!(switch["profile"], update);
    assert_eq!(me_switched["id"], *account);
    assert_eq!(me_switched["profile"], update);
    assert_eq!(settings["userId"], *account);
    assert_eq!(settings_switched["userId"], *account);
    assert_eq!(settings["profileId"], *main);
    assert_eq!(settings_switched["profileId"], *sibling);
    assert_eq!(settings["wifiOnly"], false);
    assert_eq!(settings_switched["wifiOnly"], true);
    for (value, profile) in [(&playlist, &me["profile"]), (&playlist_switched, &update)] {
        assert_eq!(value["userId"], *account);
        assert_eq!(value["profileId"], profile["id"]);
        assert_eq!(value["profileName"], profile["name"]);
    }
    assert_eq!(
        fixture("profiles_delete"),
        json!({"message":"Profile deleted"})
    );
    assert_eq!(after["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(after["profiles"][0], me["profile"]);
    assert_eq!(after["current"], *main);
    assert_eq!(after["max"], 5);
    for value in [&login, &switch] {
        assert_eq!(value["token"], "[REDACTED]");
        assert_eq!(value["refreshToken"], "[REDACTED]");
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/profiles_capture_manifest.json");
    let manifest: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(manifest["captureSet"], "profiles");
    let routes = manifest["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 13);
    let expected_files: BTreeSet<_> = [
        "login",
        "me",
        "list",
        "create",
        "update",
        "switch",
        "me_switched",
        "settings",
        "settings_switched",
        "playlist",
        "playlist_switched",
        "delete",
        "list_after_delete",
    ]
    .into_iter()
    .map(|name| format!("profiles_{name}_response.json"))
    .collect();
    let observed_files: BTreeSet<_> = routes
        .iter()
        .map(|route| route["file"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(observed_files, expected_files);
    for route in routes {
        assert!((200..300).contains(&route["status"].as_u64().unwrap()));
    }
    assert_eq!(manifest["sourceCommit"].as_str().unwrap().len(), 40);
}
