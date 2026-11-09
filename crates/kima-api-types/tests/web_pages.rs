//! Round-trips the search / playlists / settings / change-password fixtures captured from a running
//! kima-server (isolated pgvector container, generated sine-wave library).

use kima_api_types::auth::{ChangePassword, ChangePasswordResponse};
use kima_api_types::error::ApiErrorBody;
use kima_api_types::playlists::{
    AddItems, AddItemsResponse, CreatePlaylist, MessageResponse, Playlist, PlaylistDetail,
    PlaylistListEntry, RemoveItemResponse, UpdatePlaylist,
};
use kima_api_types::search::SearchResponse;
use kima_api_types::settings::UserSettings;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn round_trip<T: DeserializeOwned + Serialize>(name: &str, fixture: &str) -> T {
    let original: Value = serde_json::from_str(fixture).expect("fixture is JSON");
    let typed: T = serde_json::from_str(fixture)
        .unwrap_or_else(|e| panic!("{name}: fixture does not parse as the DTO: {e}"));
    let again: Value = serde_json::from_str(&serde_json::to_string(&typed).unwrap()).unwrap();
    assert_eq!(again, original, "{name}: DTO changed the wire shape");
    typed
}

#[test]
fn search_round_trips() {
    let all: SearchResponse = round_trip("search", include_str!("fixtures/search_response.json"));
    assert_eq!(all.artists[0].name, "Disc Band");
    assert_eq!(all.albums[0].title, "Double Album");
    let tracks: SearchResponse = round_trip(
        "search_tracks",
        include_str!("fixtures/search_tracks_response.json"),
    );
    assert!(tracks.tracks.iter().all(|t| t.title.starts_with("Song")));
    let empty: SearchResponse = round_trip(
        "search_empty",
        include_str!("fixtures/search_empty_response.json"),
    );
    assert!(empty.artists.is_empty() && empty.albums.is_empty() && empty.tracks.is_empty());
}

#[test]
fn playlist_shapes_round_trip() {
    let created: Playlist = round_trip(
        "create",
        include_str!("fixtures/playlist_create_response.json"),
    );
    assert_eq!(created.name, "Road trip");
    round_trip::<Playlist>(
        "update",
        include_str!("fixtures/playlist_update_response.json"),
    );
    let list: Vec<PlaylistListEntry> = round_trip(
        "list",
        include_str!("fixtures/playlists_list_response.json"),
    );
    assert_eq!(list[0].summary.track_count, 2);
    assert!(list[0].summary.is_owner);
    let detail: PlaylistDetail = round_trip(
        "detail",
        include_str!("fixtures/playlist_detail_response.json"),
    );
    assert_eq!(detail.items.len(), 2);
    assert!(detail.summary.playlist.reverse_order);
    assert_eq!(detail.items[0].sort, 1);
    assert_eq!(detail.items[0].track.id, detail.items[0].track_id);
    // Captured in one run: dynamic UUIDs vary across runs, but must join up.
    assert_eq!(created.id, detail.summary.playlist.id);
    assert_eq!(created.user_id, detail.summary.playlist.user_id);
    assert!(!created.reverse_order);
    assert_eq!(detail.summary.playlist.created_at, created.created_at);
    let updated: Playlist =
        serde_json::from_str(include_str!("fixtures/playlist_update_response.json")).unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.user_id, created.user_id);
    assert!(updated.reverse_order);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].summary.playlist.id, created.id);
    assert_eq!(list[0].summary.playlist.user_id, created.user_id);
    assert_eq!(
        list[0].summary.playlist.reverse_order,
        updated.reverse_order
    );
    assert_eq!(list[0].summary.playlist.updated_at, updated.updated_at);
    assert_eq!(detail.summary.playlist.updated_at, updated.updated_at);
    assert_eq!(detail.items, detail.merged_items);
    for item in &detail.items {
        assert_eq!(item.playlist_id, created.id);
        assert_eq!(item.track_id, item.track.id);
        assert!(
            list[0]
                .items
                .iter()
                .any(|preview| preview.id == item.id && preview.track.id == item.track_id)
        );
    }
    let added: AddItemsResponse = round_trip(
        "add",
        include_str!("fixtures/playlist_add_items_response.json"),
    );
    assert_eq!(added.added, 1);
    let dup: AddItemsResponse = round_trip(
        "add_dup",
        include_str!("fixtures/playlist_add_items_duplicate_response.json"),
    );
    assert!(dup.duplicated && dup.added == 0);
    round_trip::<RemoveItemResponse>(
        "remove",
        include_str!("fixtures/playlist_remove_item_response.json"),
    );
    round_trip::<MessageResponse>(
        "delete",
        include_str!("fixtures/playlist_delete_response.json"),
    );
    let err: ApiErrorBody = round_trip(
        "not_found",
        include_str!("fixtures/playlist_not_found_response.json"),
    );
    assert_eq!(err.error, "Playlist not found");
}

/// A server from before the 1.12 base sends no `reverseOrder`: it reads as false.
#[test]
fn playlist_without_reverse_order_parses() {
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/playlist_create_response.json")).unwrap();
    assert_eq!(v["reverseOrder"], false);
    v.as_object_mut().unwrap().remove("reverseOrder");
    let p: Playlist = serde_json::from_value(v).unwrap();
    assert!(!p.reverse_order);
}

#[test]
fn playlist_requests_serialize_as_the_backend_reads_them() {
    let create = serde_json::to_value(CreatePlaylist {
        name: "x".into(),
        is_public: false,
    })
    .unwrap();
    assert_eq!(create, json!({ "name": "x", "isPublic": false }));
    let rename = serde_json::to_value(UpdatePlaylist {
        name: Some("y".into()),
        is_public: None,
        reverse_order: None,
    })
    .unwrap();
    assert_eq!(rename, json!({ "name": "y" }));
    let reverse = serde_json::to_value(UpdatePlaylist {
        reverse_order: Some(true),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(reverse, json!({ "reverseOrder": true }));
    let add = serde_json::to_value(AddItems::one("t1")).unwrap();
    assert_eq!(add, json!({ "trackId": "t1" }));
}

#[test]
fn settings_and_change_password_round_trip() {
    round_trip::<UserSettings>("settings", include_str!("fixtures/settings_response.json"));
    let done: ChangePasswordResponse = round_trip(
        "change_password",
        include_str!("fixtures/change_password_response.json"),
    );
    assert_eq!(done.message, "Password changed successfully");
    for (name, fixture) in [
        (
            "wrong",
            include_str!("fixtures/change_password_wrong_response.json"),
        ),
        (
            "short",
            include_str!("fixtures/change_password_short_response.json"),
        ),
    ] {
        round_trip::<ApiErrorBody>(name, fixture);
    }
    let body = serde_json::to_value(ChangePassword {
        current_password: "a".into(),
        new_password: "b".into(),
    })
    .unwrap();
    assert_eq!(body, json!({ "currentPassword": "a", "newPassword": "b" }));
}

#[test]
fn captured_sync_routes_have_the_expected_wire_contract() {
    let public: Value =
        serde_json::from_str(include_str!("fixtures/public_config_response.json")).unwrap();
    assert_eq!(public, json!({"publicUrl":"", "canAcquire":false}));
    let removals: Value =
        serde_json::from_str(include_str!("fixtures/library_removals_response.json")).unwrap();
    assert_eq!(removals, json!({"count":0, "removals":[]}));
    let rerun: Value =
        serde_json::from_str(include_str!("fixtures/enrichment_rerun_response.json")).unwrap();
    assert_eq!(
        rerun,
        json!({
            "artists": 3,
            "tracks": 0,
            "vectors": 0,
            "message": "Re-running from identification"
        }),
        "identification reset on this fresh captured library changed its wire contract"
    );
}
