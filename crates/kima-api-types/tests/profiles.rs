//! `/api/profiles*` DTOs (M-PROF Task 6, D-243): wire shape, the PIN-ready switch body and the
//! avatar presets.

use kima_api_types::profiles::{
    AVATAR_PRESETS, CreateProfile, Profile, ProfileKind, ProfilesResponse, SwitchProfile,
    SwitchResponse, UpdateProfile, is_avatar_preset,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn round_trip<T: DeserializeOwned + Serialize>(name: &str, wire: Value) -> T {
    let typed: T = serde_json::from_value(wire.clone())
        .unwrap_or_else(|e| panic!("{name}: does not parse as the DTO: {e}"));
    let again = serde_json::to_value(&typed).unwrap();
    assert_eq!(again, wire, "{name}: DTO changed the wire shape");
    typed
}

fn profile_json(kind: &str, avatar: Value) -> Value {
    json!({
        "id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        "name": "Kids",
        "kind": kind,
        "avatar": avatar,
        "isMain": kind == "main",
        "createdAt": "2026-11-15T10:00:00Z",
    })
}

#[test]
fn dto_round_trip() {
    let list: ProfilesResponse = round_trip(
        "list",
        json!({
            "profiles": [profile_json("main", Value::Null), profile_json("kids", json!("fox"))],
            "current": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
            "max": 5,
        }),
    );
    assert_eq!(list.max, 5);
    assert_eq!(list.profiles[1].kind, ProfileKind::Kids);

    round_trip::<CreateProfile>(
        "create",
        json!({ "name": "Kids", "kind": "kids", "avatar": "owl" }),
    );
    let create: CreateProfile =
        serde_json::from_value(json!({ "name": "Ola", "kind": "standard" })).unwrap();
    assert_eq!(create.avatar, None);

    let switched: SwitchResponse = round_trip(
        "switch",
        json!({
            "token": "a.b.c",
            "refreshToken": "d.e.f",
            "profile": profile_json("kids", Value::Null),
        }),
    );
    assert_eq!(switched.profile.name, "Kids");
    round_trip::<Profile>("profile", profile_json("standard", json!("robot")));
}

#[test]
fn update_tells_absent_avatar_from_null() {
    // Absent: keep the stored avatar. `null`: clear it. A key: set it.
    let keep: UpdateProfile = serde_json::from_value(json!({ "name": "Ola" })).unwrap();
    assert_eq!(keep.avatar, None);
    assert_eq!(keep.name.as_deref(), Some("Ola"));
    let clear: UpdateProfile = serde_json::from_value(json!({ "avatar": null })).unwrap();
    assert_eq!(clear.avatar, Some(None));
    let set: UpdateProfile =
        serde_json::from_value(json!({ "avatar": "cat", "kind": "kids" })).unwrap();
    assert_eq!(set.avatar, Some(Some("cat".to_owned())));
    assert_eq!(set.kind, Some(ProfileKind::Kids));
    // And back: only the fields that are set go on the wire.
    assert_eq!(
        serde_json::to_value(&keep).unwrap(),
        json!({ "name": "Ola" })
    );
    assert_eq!(
        serde_json::to_value(&clear).unwrap(),
        json!({ "avatar": null })
    );
}

#[test]
fn switch_body_is_empty_or_pin_only() {
    let empty: SwitchProfile = serde_json::from_value(json!({})).unwrap();
    assert_eq!(empty.pin, None);
    let with_pin: SwitchProfile = serde_json::from_value(json!({ "pin": "1234" })).unwrap();
    assert_eq!(with_pin.pin.as_deref(), Some("1234"));
    // Any other field is refused, so a client sending the PIN under another name finds out.
    assert!(serde_json::from_value::<SwitchProfile>(json!({ "code": "1234" })).is_err());
    assert_eq!(serde_json::to_value(&empty).unwrap(), json!({}));
}

#[test]
fn bodies_refuse_unknown_fields() {
    assert!(
        serde_json::from_value::<CreateProfile>(json!({ "name": "A", "kind": "kids", "x": 1 }))
            .is_err()
    );
    assert!(serde_json::from_value::<UpdateProfile>(json!({ "avatarKey": "fox" })).is_err());
}

#[test]
fn avatar_presets_are_valid_keys() {
    assert!(!AVATAR_PRESETS.is_empty());
    let mut seen = std::collections::HashSet::new();
    for key in AVATAR_PRESETS {
        // The database's own check on `profiles.avatar`.
        assert!(
            (1..=32).contains(&key.len())
                && key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{key}"
        );
        assert!(seen.insert(key), "duplicate preset {key}");
        assert!(is_avatar_preset(key));
    }
    for bad in ["", "Fox", "fox ", "unicorn-xl", "../x"] {
        assert!(!is_avatar_preset(bad), "{bad:?}");
    }
}
