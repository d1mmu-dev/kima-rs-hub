mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

struct Env {
    server: TestServer,
    ana: String,
    ben: String,
}

async fn env(pool: PgPool) -> Env {
    let state = common::state(pool);
    let (_, ana) = common::user_with_token(&state, "ana").await;
    let (_, ben) = common::user_with_token(&state, "ben").await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        ana,
        ben,
    }
}

async fn tracks(pool: &PgPool, n: usize) -> Vec<Uuid> {
    let mut out = Vec::new();
    for i in 0..n {
        let title = format!("Song {i}");
        out.push(
            seed(
                pool,
                Seed::new("Alpha", "A1", Some(2000), &title).at(1, i as i32 + 1),
            )
            .await
            .track,
        );
    }
    out
}

async fn create(e: &Env, token: &str, name: &str, public: bool) -> String {
    let res = e
        .server
        .post("/api/playlists")
        .authorization_bearer(token)
        .json(&json!({ "name": name, "isPublic": public }))
        .await;
    res.assert_status_ok();
    res.json::<Value>()["id"].as_str().unwrap().to_owned()
}

async fn detail(e: &Env, token: &str, id: &str) -> Value {
    let res = e
        .server
        .get(&format!("/api/playlists/{id}"))
        .authorization_bearer(token)
        .await;
    res.assert_status_ok();
    res.json()
}

fn titles(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["track"]["title"].as_str().unwrap().to_owned())
        .collect()
}

async fn add(e: &Env, token: &str, id: &str, body: Value) -> axum_test::TestResponse {
    e.server
        .post(&format!("/api/playlists/{id}/items"))
        .authorization_bearer(token)
        .json(&body)
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_add_list_remove(pool: PgPool) {
    let t = tracks(&pool, 2).await;
    let e = env(pool).await;
    let id = create(&e, &e.ana, "Mix", false).await;

    let res = add(&e, &e.ana, &id, json!({ "trackId": t[0] })).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["added"], 1);
    let res = add(&e, &e.ana, &id, json!({ "trackIds": [t[1]] })).await;
    assert_eq!(res.json::<Value>()["added"], 1);

    let d = detail(&e, &e.ana, &id).await;
    assert_eq!(titles(&d), ["Song 0", "Song 1"]);
    assert_eq!(d["trackCount"], 2);
    assert_eq!(d["isOwner"], true);
    assert_eq!(d["isHidden"], false);
    assert_eq!(d["pendingCount"], 0);
    assert_eq!(d["pendingTracks"], json!([]));
    assert_eq!(d["mergedItems"].as_array().unwrap().len(), 2);
    assert_eq!(d["items"][0]["type"], "track");
    assert_eq!(d["items"][0]["track"]["album"]["artist"]["name"], "Alpha");
    assert_eq!(d["user"]["username"], "ana");

    // The list carries counts and cover items.
    let list: Value = e
        .server
        .get("/api/playlists")
        .authorization_bearer(&e.ana)
        .await
        .json();
    assert_eq!(list[0]["trackCount"], 2);
    assert_eq!(list[0]["items"].as_array().unwrap().len(), 2);
    assert!(list[0]["items"][0]["track"]["album"]["coverUrl"].is_string());

    // Rename and publish.
    let res = e
        .server
        .put(&format!("/api/playlists/{id}"))
        .authorization_bearer(&e.ana)
        .json(&json!({ "name": "Renamed", "isPublic": true }))
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["name"], "Renamed");

    // Remove one by track id; a second removal is 404.
    let url = format!("/api/playlists/{id}/items/{}", t[0]);
    let res = e.server.delete(&url).authorization_bearer(&e.ana).await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({ "success": true, "removed": 1 })
    );
    e.server
        .delete(&url)
        .authorization_bearer(&e.ana)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert_eq!(titles(&detail(&e, &e.ana, &id).await), ["Song 1"]);

    // Bulk removal.
    let res = e
        .server
        .delete(&format!("/api/playlists/{id}/items"))
        .authorization_bearer(&e.ana)
        .json(&json!({ "trackIds": [t[1]] }))
        .await;
    assert_eq!(res.json::<Value>(), json!({ "removed": 1 }));

    // Delete.
    e.server
        .delete(&format!("/api/playlists/{id}"))
        .authorization_bearer(&e.ana)
        .await
        .assert_status_ok();
    e.server
        .get(&format!("/api/playlists/{id}"))
        .authorization_bearer(&e.ana)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn duplicate_add_is_noop(pool: PgPool) {
    let t = tracks(&pool, 2).await;
    let e = env(pool).await;
    let id = create(&e, &e.ana, "Mix", false).await;
    add(&e, &e.ana, &id, json!({ "trackId": t[0] }))
        .await
        .assert_status_ok();

    let res = add(&e, &e.ana, &id, json!({ "trackId": t[0] })).await;
    res.assert_status(StatusCode::OK);
    let v: Value = res.json();
    assert_eq!(
        (v["added"].clone(), v["duplicates"].clone()),
        (json!(0), json!(1))
    );
    assert_eq!(v["duplicated"], true);

    // Mixed: one new, one duplicate, one unknown, one not an id.
    let ghost = Uuid::now_v7();
    let res = add(
        &e,
        &e.ana,
        &id,
        json!({ "trackIds": [t[0], t[1], ghost, "nope"] }),
    )
    .await;
    let v: Value = res.json();
    assert_eq!(v["added"], 1);
    assert_eq!(v["duplicates"], 1);
    assert_eq!(v["rejected"].as_array().unwrap().len(), 2);
    assert_eq!(titles(&detail(&e, &e.ana, &id).await), ["Song 0", "Song 1"]);

    // Nothing usable at all: 404 with the rejected list.
    let res = add(&e, &e.ana, &id, json!({ "trackId": ghost })).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>()["rejected"], json!([ghost.to_string()]));

    // Validation.
    for body in [
        json!({}),
        json!({ "trackId": "a", "trackIds": ["b"] }),
        json!({ "trackIds": [] }),
    ] {
        add(&e, &e.ana, &id, body)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn batch_add_keeps_order_and_reports(pool: PgPool) {
    let t = tracks(&pool, 4).await;
    let e = env(pool).await;
    let id = create(&e, &e.ana, "Mix", false).await;
    add(&e, &e.ana, &id, json!({ "trackId": t[2] }))
        .await
        .assert_status_ok();

    let res = e
        .server
        .post(&format!("/api/playlists/{id}/items/batch"))
        .authorization_bearer(&e.ana)
        .json(&json!({ "trackIds": [t[3], t[2], t[0], "x"] }))
        .await;
    res.assert_status_ok();
    let v: Value = res.json();
    assert_eq!(v["added"], 2);
    assert_eq!(v["skippedExisting"], 1);
    assert_eq!(v["skippedInvalid"], 1);
    // Items come back in playlist order and carry the track.
    assert_eq!(titles(&v), ["Song 2", "Song 3", "Song 0"]);
    assert_eq!(
        titles(&detail(&e, &e.ana, &id).await),
        ["Song 2", "Song 3", "Song 0"]
    );

    // Someone else's playlist: 404 when private.
    e.server
        .post(&format!("/api/playlists/{id}/items/batch"))
        .authorization_bearer(&e.ben)
        .json(&json!({ "trackIds": [t[1]] }))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Over the cap.
    let many: Vec<String> = (0..501).map(|_| Uuid::now_v7().to_string()).collect();
    e.server
        .post(&format!("/api/playlists/{id}/items/batch"))
        .authorization_bearer(&e.ana)
        .json(&json!({ "trackIds": many }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

async fn item_ids(e: &Env, id: &str) -> Vec<String> {
    detail(e, &e.ana, id).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn mv(e: &Env, id: &str, item: &str, after: Option<&str>) -> axum_test::TestResponse {
    e.server
        .patch(&format!("/api/playlists/{id}/items/{item}/move"))
        .authorization_bearer(&e.ana)
        .json(&json!({ "afterItemId": after }))
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn move_reorders(pool: PgPool) {
    let t = tracks(&pool, 4).await;
    let e = env(pool).await;
    let id = create(&e, &e.ana, "Mix", false).await;
    add(&e, &e.ana, &id, json!({ "trackIds": t }))
        .await
        .assert_status_ok();
    let ids = item_ids(&e, &id).await;

    // Last to first (null anchor).
    let res = mv(&e, &id, &ids[3], None).await;
    res.assert_status_ok();
    let v: Value = res.json();
    assert_eq!(v["success"], true);
    assert!(v["rank"].is_string());
    assert_eq!(
        titles(&detail(&e, &e.ana, &id).await),
        ["Song 3", "Song 0", "Song 1", "Song 2"]
    );

    // First after the second-to-last.
    mv(&e, &id, &ids[3], Some(&ids[1])).await.assert_status_ok();
    assert_eq!(
        titles(&detail(&e, &e.ana, &id).await),
        ["Song 0", "Song 1", "Song 3", "Song 2"]
    );

    // After itself changes nothing; unknown item 404; unknown anchor 400.
    mv(&e, &id, &ids[0], Some(&ids[0])).await.assert_status_ok();
    mv(&e, &id, &Uuid::now_v7().to_string(), None)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    mv(&e, &id, &ids[0], Some(&Uuid::now_v7().to_string()))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        titles(&detail(&e, &e.ana, &id).await),
        ["Song 0", "Song 1", "Song 3", "Song 2"]
    );

    // Sorts stay dense 0..n-1 and ranks sort the same way, also after a removal.
    e.server
        .delete(&format!("/api/playlists/{id}/items/{}", t[1]))
        .authorization_bearer(&e.ana)
        .await
        .assert_status_ok();
    let d = detail(&e, &e.ana, &id).await;
    let sorts: Vec<i64> = d["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["sort"].as_i64().unwrap())
        .collect();
    assert_eq!(sorts, [0, 1, 2]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_moves_stay_dense(pool: PgPool) {
    let t = tracks(&pool, 8).await;
    let e = env(pool.clone()).await;
    let id = create(&e, &e.ana, "Mix", false).await;
    add(&e, &e.ana, &id, json!({ "trackIds": t }))
        .await
        .assert_status_ok();
    let ids = item_ids(&e, &id).await;

    let moves = (0..8).map(|i| {
        let e = &e;
        let id = &id;
        let ids = &ids;
        async move {
            let after = ids[(i + 3) % 8].clone();
            mv(e, id, &ids[i], (after != ids[i]).then_some(after.as_str()))
                .await
                .assert_status_ok();
        }
    });
    futures::future::join_all(moves).await;

    let mut sorts: Vec<i32> =
        sqlx::query_scalar("SELECT sort FROM playlist_items WHERE playlist_id = $1")
            .bind(id.parse::<Uuid>().unwrap())
            .fetch_all(&pool)
            .await
            .unwrap();
    sorts.sort_unstable();
    assert_eq!(sorts, (0..8).collect::<Vec<_>>());
    let after = item_ids(&e, &id).await;
    let mut a = after.clone();
    a.sort();
    let mut b = ids.clone();
    b.sort();
    assert_eq!(a, b, "no item lost or duplicated");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn private_playlist_404_for_others(pool: PgPool) {
    let t = tracks(&pool, 1).await;
    let e = env(pool).await;
    let id = create(&e, &e.ana, "Secret", false).await;
    let url = format!("/api/playlists/{id}");

    e.server
        .get(&url)
        .authorization_bearer(&e.ben)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    e.server
        .put(&url)
        .authorization_bearer(&e.ben)
        .json(&json!({ "name": "x" }))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    e.server
        .post(&format!("{url}/hide"))
        .authorization_bearer(&e.ben)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    add(&e, &e.ben, &id, json!({ "trackId": t[0] }))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Not listed for others, and a malformed id is 404 too.
    let list: Value = e
        .server
        .get("/api/playlists")
        .authorization_bearer(&e.ben)
        .await
        .json();
    assert_eq!(list, json!([]));
    e.server
        .get("/api/playlists/not-an-id")
        .authorization_bearer(&e.ben)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    e.server
        .get(&url)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn public_playlist_readable_not_writable(pool: PgPool) {
    let t = tracks(&pool, 2).await;
    let e = env(pool).await;
    let id = create(&e, &e.ana, "Shared", true).await;
    add(&e, &e.ana, &id, json!({ "trackId": t[0] }))
        .await
        .assert_status_ok();
    let url = format!("/api/playlists/{id}");

    let d = detail(&e, &e.ben, &id).await;
    assert_eq!(d["isOwner"], false);
    assert_eq!(d["user"]["username"], "ana");
    assert_eq!(titles(&d), ["Song 0"]);
    let list: Value = e
        .server
        .get("/api/playlists")
        .authorization_bearer(&e.ben)
        .await
        .json();
    assert_eq!(list[0]["id"], json!(id));
    assert_eq!(list[0]["isOwner"], false);

    e.server
        .put(&url)
        .authorization_bearer(&e.ben)
        .json(&json!({ "name": "mine now" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    e.server
        .delete(&url)
        .authorization_bearer(&e.ben)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    add(&e, &e.ben, &id, json!({ "trackId": t[1] }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    e.server
        .post(&format!("{url}/items/batch"))
        .authorization_bearer(&e.ben)
        .json(&json!({ "trackIds": [t[1]] }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    e.server
        .delete(&format!("{url}/items/{}", t[0]))
        .authorization_bearer(&e.ben)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let res = e
        .server
        .patch(&format!("{url}/items/{}/move", Uuid::now_v7()))
        .authorization_bearer(&e.ben)
        .json(&json!({ "afterItemId": null }))
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(titles(&detail(&e, &e.ana, &id).await), ["Song 0"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hide_unhide(pool: PgPool) {
    let e = env(pool).await;
    let own = create(&e, &e.ana, "Mine", false).await;
    let shared = create(&e, &e.ana, "Shared", true).await;

    // Ben hides Ana's public playlist; only his view changes.
    let res = e
        .server
        .post(&format!("/api/playlists/{shared}/hide"))
        .authorization_bearer(&e.ben)
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({ "message": "Playlist hidden", "isHidden": true })
    );
    // Hiding twice is fine.
    e.server
        .post(&format!("/api/playlists/{shared}/hide"))
        .authorization_bearer(&e.ben)
        .await
        .assert_status_ok();
    assert_eq!(detail(&e, &e.ben, &shared).await["isHidden"], true);
    assert_eq!(detail(&e, &e.ana, &shared).await["isHidden"], false);
    let list: Value = e
        .server
        .get("/api/playlists")
        .authorization_bearer(&e.ben)
        .await
        .json();
    assert_eq!(list[0]["isHidden"], true);

    let res = e
        .server
        .delete(&format!("/api/playlists/{shared}/hide"))
        .authorization_bearer(&e.ben)
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({ "message": "Playlist unhidden", "isHidden": false })
    );
    assert_eq!(detail(&e, &e.ben, &shared).await["isHidden"], false);

    // The owner can hide their own, and unhide of something never hidden is fine.
    e.server
        .post(&format!("/api/playlists/{own}/hide"))
        .authorization_bearer(&e.ana)
        .await
        .assert_status_ok();
    e.server
        .delete(&format!("/api/playlists/{own}/hide"))
        .authorization_bearer(&e.ben)
        .await
        .assert_status_ok();
    assert_eq!(detail(&e, &e.ana, &own).await["isHidden"], true);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleting_track_removes_from_playlists(pool: PgPool) {
    let t = tracks(&pool, 3).await;
    let e = env(pool.clone()).await;
    let id = create(&e, &e.ana, "Mix", false).await;
    add(&e, &e.ana, &id, json!({ "trackIds": t }))
        .await
        .assert_status_ok();

    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(t[1])
        .execute(&pool)
        .await
        .unwrap();
    let d = detail(&e, &e.ana, &id).await;
    assert_eq!(titles(&d), ["Song 0", "Song 2"]);
    assert_eq!(d["trackCount"], 2);

    // Deleting the playlist or its owner removes the items and hidden rows too.
    sqlx::query("DELETE FROM users WHERE username = 'ana'")
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM playlist_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_tracks_are_hidden_but_keep_their_place(pool: PgPool) {
    let t = tracks(&pool, 3).await;
    let e = env(pool.clone()).await;
    let id = create(&e, &e.ana, "Mix", false).await;
    add(&e, &e.ana, &id, json!({ "trackIds": t }))
        .await
        .assert_status_ok();

    mark_missing(&pool, t[1]).await;
    let d = detail(&e, &e.ana, &id).await;
    assert_eq!(titles(&d), ["Song 0", "Song 2"]);
    assert_eq!(d["trackCount"], 2);
    // A missing track cannot be newly added.
    let other = create(&e, &e.ana, "Other", false).await;
    add(&e, &e.ana, &other, json!({ "trackId": t[1] }))
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // The file comes back: the entry is where it was.
    sqlx::query("UPDATE tracks SET missing_since = NULL WHERE id = $1")
        .bind(t[1])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        titles(&detail(&e, &e.ana, &id).await),
        ["Song 0", "Song 1", "Song 2"]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_validates(pool: PgPool) {
    let e = env(pool).await;
    for body in [
        json!({}),
        json!({ "name": "" }),
        json!({ "name": "x".repeat(201) }),
        json!({ "name": 5 }),
    ] {
        e.server
            .post("/api/playlists")
            .authorization_bearer(&e.ana)
            .json(&body)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
    let id = create(&e, &e.ana, &"x".repeat(200), false).await;
    e.server
        .put(&format!("/api/playlists/{id}"))
        .authorization_bearer(&e.ana)
        .json(&json!({ "name": "" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}
