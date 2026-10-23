//! `GET /api/library/tracks/{id}/lyrics` (spec §14.2) against the replay server, with the
//! hand-written LRCLIB fixtures of kima-ident (`tests/fixtures/README.md` there).

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_net::testing::{Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};
use uuid::Uuid;

fn fixture() -> Recording {
    Recording::load(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../kima-ident/tests/fixtures/lrclib/lyrics.json"),
    )
}

async fn server(pool: &PgPool, route_switch: IdentifySwitch) -> (Replay, TestServer, String) {
    let replay = Replay::start(fixture()).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    let state = common::state(pool.clone())
        .with_identify(route_switch)
        .with_net(Arc::new(net));
    let (_, token) = common::user_with_token(
        &state,
        &format!("u{}", &Uuid::new_v4().simple().to_string()[..8]),
    )
    .await;
    (
        replay,
        TestServer::new(kima_api::router(state)).unwrap(),
        token,
    )
}

/// A track the fixture knows, `secs` long.
async fn track(pool: &PgPool, title: &str, artist: &str, album: &str, secs: i32) -> Uuid {
    let id = seed(pool, Seed::new(artist, album, None, title))
        .await
        .track;
    sqlx::query("UPDATE tracks SET duration_secs = $2 WHERE id = $1")
        .bind(id)
        .bind(secs)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn hit(pool: &PgPool) -> Uuid {
    track(
        pool,
        "Amazing Grace",
        "Judy Collins",
        "Whales & Nightingales",
        243,
    )
    .await
}

async fn absent(pool: &PgPool) -> Uuid {
    track(
        pool,
        "No Such Song",
        "Nobody In Particular",
        "Nothing Here",
        200,
    )
    .await
}

async fn lyrics(server: &TestServer, token: &str, id: &str) -> (StatusCode, Value) {
    let res = server
        .get(&format!("/api/library/tracks/{id}/lyrics"))
        .authorization_bearer(token)
        .await;
    (res.status_code(), res.json())
}

fn hit_body() -> Value {
    let body = &fixture().0[0].body;
    json!({
        "plainLyrics": body["plainLyrics"],
        "syncedLyrics": body["syncedLyrics"],
        "source": "lrclib",
    })
}

fn none_body() -> Value {
    json!({ "plainLyrics": null, "syncedLyrics": null, "source": "none" })
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let (replay, server, _) = server(&pool, IdentifySwitch::new(false, true)).await;
    let id = hit(&pool).await;
    let res = server
        .get(&format!("/api/library/tracks/{id}/lyrics"))
        .await;
    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_and_non_uuid_404(pool: PgPool) {
    let (replay, server, token) = server(&pool, IdentifySwitch::new(false, true)).await;
    for id in [Uuid::new_v4().to_string(), "not-a-uuid".to_string()] {
        let (status, body) = lyrics(&server, &token, &id).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body, json!({ "error": "Track not found" }));
    }
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hit_answers_camel_case_and_is_stored(pool: PgPool) {
    let (replay, server, token) = server(&pool, IdentifySwitch::new(false, true)).await;
    let id = hit(&pool).await.to_string();
    assert_eq!(
        lyrics(&server, &token, &id).await,
        (StatusCode::OK, hit_body())
    );
    assert_eq!(
        lyrics(&server, &token, &id).await,
        (StatusCode::OK, hit_body())
    );
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_answers_nulls(pool: PgPool) {
    let (replay, server, token) = server(&pool, IdentifySwitch::new(false, true)).await;
    let id = absent(&pool).await.to_string();
    assert_eq!(
        lyrics(&server, &token, &id).await,
        (StatusCode::OK, none_body())
    );
    assert_eq!(
        lyrics(&server, &token, &id).await,
        (StatusCode::OK, none_body())
    );
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn identify_off_answers_from_storage_no_request(pool: PgPool) {
    let switches = [
        IdentifySwitch::new(false, false),
        IdentifySwitch::new(true, true),
    ];
    for (n, switch) in switches.into_iter().enumerate() {
        let (replay, server, token) = server(&pool, switch).await;
        let title = format!("Amazing Grace {n}");
        let id = track(&pool, &title, "Judy Collins", "Whales & Nightingales", 243).await;
        assert_eq!(
            lyrics(&server, &token, &id.to_string()).await,
            (StatusCode::OK, none_body())
        );
        let mut conn = pool.acquire().await.unwrap();
        kima_db::lyrics::upsert_embedded(&mut conn, id, Some("tag words"), None)
            .await
            .unwrap();
        drop(conn);
        assert_eq!(
            lyrics(&server, &token, &id.to_string()).await,
            (
                StatusCode::OK,
                json!({ "plainLyrics": "tag words", "syncedLyrics": null, "source": "embedded" })
            )
        );
        assert!(replay.asked().is_empty());
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_track_answers_like_present(pool: PgPool) {
    let (_replay, server, token) = server(&pool, IdentifySwitch::new(false, true)).await;
    let id = hit(&pool).await;
    mark_missing(&pool, id).await;
    assert_eq!(
        lyrics(&server, &token, &id.to_string()).await,
        (StatusCode::OK, hit_body())
    );
}
