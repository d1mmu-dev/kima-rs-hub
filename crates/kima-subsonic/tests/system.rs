mod common;

use axum::http::StatusCode;
use common::{app_token, rest, rest_xml, server, user};
use kima_db::PgPool;
use kima_db::users::{ROLE_ADMIN, ROLE_USER};
use serde_json::json;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn extensions_need_no_auth(pool: PgPool) {
    let (server, _) = server(pool);
    let res = rest(&server, "getOpenSubsonicExtensions", &[]).await;
    assert_eq!(res["status"], "ok");
    assert_eq!(res["openSubsonic"], true);
    assert_eq!(
        res["openSubsonicExtensions"],
        json!([
            {"name": "apiKeyAuthentication", "versions": [1]},
            {"name": "formPost", "versions": [1]},
            {"name": "indexBasedQueue", "versions": [1]},
            {"name": "songLyrics", "versions": [1]},
        ])
    );
    // The suffix-less path works too.
    let res = server
        .get("/getOpenSubsonicExtensions")
        .add_query_param("f", "json")
        .await;
    assert_eq!(
        res.json::<serde_json::Value>()["subsonic-response"]["status"],
        "ok"
    );
    let xml = rest_xml(&server, "getOpenSubsonicExtensions", &[]).await;
    assert!(xml.contains(
        "<openSubsonicExtensions name=\"formPost\"><versions>1</versions></openSubsonicExtensions>"
    ));
}

/// `songLyrics` v1 is advertised now that `getLyricsBySongId` is real (spec §18 row 4, C10).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn extensions_include_song_lyrics(pool: PgPool) {
    let (server, _) = server(pool);
    let res = rest(&server, "getOpenSubsonicExtensions", &[]).await;
    let list = res["openSubsonicExtensions"].as_array().unwrap();
    assert!(
        list.contains(&json!({"name": "songLyrics", "versions": [1]})),
        "{res}"
    );
    let xml = rest_xml(&server, "getOpenSubsonicExtensions", &[]).await;
    assert!(xml.contains(
        "<openSubsonicExtensions name=\"songLyrics\"><versions>1</versions></openSubsonicExtensions>"
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn token_info_ok(pool: PgPool) {
    let (server, state) = server(pool);
    let ana = user(&state, "ana", "password1", ROLE_USER).await;
    let (_, token) = app_token(&state, &ana, "phone").await;
    let res = rest(&server, "tokenInfo", &[("apiKey", &token)]).await;
    assert_eq!(res["status"], "ok");
    assert_eq!(res["tokenInfo"]["username"], "ana");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn token_info_missing_10(pool: PgPool) {
    let (server, _) = server(pool);
    let res = rest(&server, "tokenInfo", &[]).await;
    assert_eq!(res["status"], "failed");
    assert_eq!(res["error"]["code"], 10);
    assert_eq!(
        res["error"]["message"],
        "Required parameter is missing: apiKey"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn token_info_unknown_44(pool: PgPool) {
    let (server, _) = server(pool);
    let res = rest(&server, "tokenInfo", &[("apiKey", "nope")]).await;
    assert_eq!(res["error"]["code"], 44);
    assert_eq!(res["error"]["message"], "Invalid API key");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn post_form_params_work(pool: PgPool) {
    let (server, state) = server(pool);
    let admin = user(&state, "root", "password1", ROLE_ADMIN).await;
    let (_, token) = app_token(&state, &admin, "tv").await;
    let res = server
        .post("/tokenInfo")
        .add_query_param("f", "json")
        .form(&[("apiKey", token.as_str())])
        .await;
    res.assert_status_ok();
    let body = res.json::<serde_json::Value>();
    assert_eq!(body["subsonic-response"]["tokenInfo"]["username"], "root");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn head_answers(pool: PgPool) {
    let (server, _) = server(pool);
    let res = server
        .method(axum::http::Method::HEAD, "/getOpenSubsonicExtensions.view")
        .await;
    assert_eq!(res.status_code(), StatusCode::OK);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_endpoint_is_error_0(pool: PgPool) {
    let (server, state) = server(pool);
    user(&state, "root", "pw", ROLE_USER).await;
    let res = rest(&server, "nothingHere", &[("u", "root"), ("p", "pw")]).await;
    assert_eq!(res["status"], "failed");
    assert_eq!(res["error"]["code"], 0);
    assert_eq!(
        res["error"]["message"],
        "Not implemented: /nothingHere.view"
    );
    let res = server
        .get("/other")
        .add_query_params([("f", "json"), ("u", "root"), ("p", "pw")])
        .await;
    assert_eq!(
        res.json::<serde_json::Value>()["subsonic-response"]["error"]["message"],
        "Not implemented: /other.view"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn xml_is_the_default_format(pool: PgPool) {
    let (server, _) = server(pool);
    let res = server.get("/tokenInfo.view").await;
    assert_eq!(
        res.header("content-type").to_str().unwrap(),
        "text/xml; charset=utf-8"
    );
    assert!(res.text().contains("<error code=\"10\""));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn too_many_parameters_is_error_0(pool: PgPool) {
    let (server, _) = server(pool);
    let body: String = (0..10_001).map(|i| format!("k{i}=&")).collect();
    let res = server
        .post("/tokenInfo")
        .add_query_param("f", "json")
        .content_type("application/x-www-form-urlencoded")
        .bytes(body.into())
        .await;
    res.assert_status_ok();
    let v = res.json::<serde_json::Value>();
    assert_eq!(v["subsonic-response"]["error"]["code"], 0);
    assert_eq!(
        v["subsonic-response"]["error"]["message"],
        "Too many parameters"
    );
}
