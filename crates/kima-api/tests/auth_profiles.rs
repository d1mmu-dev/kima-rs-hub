//! M-PROF Task 4: tokens and sessions carry the profile (D-241). Every test that matters for
//! profiles acts as a non-main profile: on the main profile the account id and the profile id
//! are the same, so a mix-up between them would pass unseen.

mod common;

use std::path::Path;
use std::time::Duration;

use axum::http::StatusCode;
use axum::http::header::ETAG;
use axum_test::TestServer;
use base64::Engine;
use common::library::{Seed, seed};
use kima_api::AppState;
use kima_core::auth::Claims;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile, Profile};
use kima_db::users::User;
use kima_library::transcode::Transcoder;
use serde_json::{Value, json};
use uuid::Uuid;

async fn add_profile(state: &AppState, user: &User, name: &str, kind: ProfileKind) -> Profile {
    profiles::create(
        &state.pool,
        user.account(),
        NewProfile {
            name: name.into(),
            kind,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
}

fn access(state: &AppState, user: &User, profile: ProfileId) -> String {
    state.jwt.issue_access(
        user.account(),
        profile,
        &user.username,
        &user.role,
        user.token_version,
    )
}

/// The claims inside a token, read without checking the signature.
fn claims(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

/// A token signed the way every token was before M-PROF: upstream's claims, no `profileId`.
fn legacy_token(user: &User) -> String {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let claims = json!({
        "userId": user.id,
        "username": user.username,
        "role": user.role,
        "tokenVersion": user.token_version,
        "iat": now,
        "exp": now + 3600,
    });
    // Same secret and algorithm as the server; the shape is checked against `Claims`.
    let _: Claims = serde_json::from_value(claims.clone()).unwrap();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(common::SECRET.as_bytes()),
    )
    .unwrap()
}

async fn me(server: &TestServer, token: &str) -> axum_test::TestResponse {
    server.get("/api/auth/me").authorization_bearer(token).await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn legacy_token_without_profile_acts_as_main(pool: PgPool) {
    let state = common::state(pool);
    let (admin, _) = common::user_with_token(&state, "ana").await;
    let token = legacy_token(&admin);
    let server = TestServer::new(kima_api::router(state)).unwrap();

    let res = me(&server, &token).await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["id"], json!(admin.id));
    assert_eq!(body["profile"]["id"], json!(admin.id));
    assert_eq!(body["profile"]["kind"], "main");
    assert_eq!(body["profile"]["isMain"], true);
    // Main is the admin's profile, so the admin routes stay open to an old token.
    server
        .get("/api/auth/users")
        .authorization_bearer(&token)
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn token_for_deleted_profile_is_401(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let token = access(&state, &user, kids.id);
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = me(&server, &token).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["profile"]["id"], json!(kids.id));

    profiles::delete(&state.pool, user.account(), kids.id)
        .await
        .unwrap()
        .unwrap();
    let res = me(&server, &token).await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(res.json::<Value>(), json!({ "error": "Not authenticated" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn token_naming_foreign_profile_is_401(pool: PgPool) {
    let state = common::state(pool);
    let (ana, _) = common::user_with_token(&state, "ana").await;
    let (bob, _) = common::user_with_token(&state, "bob").await;
    let bobs_kids = add_profile(&state, &bob, "Kids", ProfileKind::Kids).await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    // Correctly signed, ana's account and token version, but a profile of bob's account:
    // never "that other profile", whether bob's kids or bob's main.
    for foreign in [bobs_kids.id, bob.main_profile()] {
        let token = access(&state, &ana, foreign);
        me(&server, &token)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
        server
            .get("/api/settings")
            .authorization_bearer(&token)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    // A profile id that names nothing at all.
    let token = access(&state, &ana, ProfileId::new(Uuid::now_v7()));
    me(&server, &token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// Every `/api` route behind `AdminUser`, with ids that parse (so the admin check, not the
/// path, decides). Kept in step with the router by `admin_route_list_is_complete`.
const ADMIN_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/auth/users"),
    ("POST", "/api/auth/create-user"),
    ("DELETE", "/api/auth/users/{id}"),
    ("PUT", "/api/enrichment/albums/{id}/metadata"),
    ("POST", "/api/enrichment/albums/{id}/reset"),
    ("PUT", "/api/enrichment/artists/{id}/metadata"),
    ("POST", "/api/enrichment/artists/{id}/reset"),
    ("PUT", "/api/enrichment/concurrency"),
    ("POST", "/api/enrichment/pause"),
    ("POST", "/api/enrichment/re-decide-everything"),
    ("POST", "/api/enrichment/rerun"),
    ("POST", "/api/enrichment/resume"),
    ("POST", "/api/enrichment/retry-failed-and-blocked"),
    ("POST", "/api/enrichment/stop"),
    ("POST", "/api/enrichment/sync"),
    ("PUT", "/api/enrichment/tracks/{id}/metadata"),
    ("POST", "/api/enrichment/tracks/{id}/reset"),
    ("POST", "/api/enrichment/wipe-and-rerun"),
    ("GET", "/api/library/attention"),
    ("GET", "/api/library/autotag/albums"),
    ("GET", "/api/library/autotag/albums/{id}"),
    ("POST", "/api/library/autotag/albums/{id}/apply"),
    ("POST", "/api/library/autotag/albums/{id}/skip"),
    ("GET", "/api/library/autotag/capabilities"),
    ("GET", "/api/library/autotag/runs"),
    ("GET", "/api/library/autotag/runs/{id}"),
    ("POST", "/api/library/autotag/runs/{id}/undo"),
    ("POST", "/api/library/autotag/search"),
    ("GET", "/api/library/organizer/capabilities"),
    ("POST", "/api/library/organizer/preview"),
    ("GET", "/api/library/organizer/runs"),
    ("GET", "/api/library/organizer/runs/{id}"),
    ("GET", "/api/library/organizer/runs/{id}/albums"),
    ("POST", "/api/library/organizer/runs/{id}/apply"),
    ("POST", "/api/library/organizer/runs/{id}/switch-root"),
    ("POST", "/api/library/organizer/runs/{id}/undo"),
    ("POST", "/api/library/refile"),
    ("GET", "/api/library/removals"),
    ("POST", "/api/library/scan"),
    ("GET", "/api/library/watch-status"),
    ("POST", "/api/onboarding/audiobookshelf"),
    ("POST", "/api/onboarding/lidarr"),
    ("POST", "/api/onboarding/soulseek"),
    ("PUT", "/api/podcasts/{id}/ai-label"),
    ("PUT", "/api/podcasts/{id}/episodes/{id}/ai-label"),
    ("GET", "/api/system-settings"),
    ("POST", "/api/system-settings"),
    ("POST", "/api/system-settings/test-fanart"),
];

async fn call(
    server: &TestServer,
    method: &str,
    path: &str,
    token: &str,
) -> axum_test::TestResponse {
    let path = path.replace("{id}", &Uuid::now_v7().to_string());
    let req = match method {
        "GET" => server.get(&path),
        "POST" => server.post(&path),
        "PUT" => server.put(&path),
        "DELETE" => server.delete(&path),
        other => panic!("method {other}"),
    };
    req.authorization_bearer(token).json(&json!({})).await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn admin_on_kids_profile_is_403_on_every_admin_route(pool: PgPool) {
    let state = common::state(pool);
    let (admin, main_token) = common::user_with_token(&state, "ana").await;
    assert_eq!(admin.role, "admin");
    let kids = add_profile(&state, &admin, "Kids", ProfileKind::Kids).await;
    let teen = add_profile(&state, &admin, "Teen", ProfileKind::Standard).await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    for profile in [kids.id, teen.id] {
        let token = access(&state, &admin, profile);
        for (method, path) in ADMIN_ROUTES {
            let res = call(&server, method, path, &token).await;
            assert_eq!(
                res.status_code(),
                StatusCode::FORBIDDEN,
                "{method} {path} as {profile}: {}",
                res.text()
            );
            assert_eq!(
                res.json::<Value>(),
                json!({ "error": "Admin access required" }),
                "{method} {path}"
            );
        }
    }
    // The same account's main profile passes the admin check on every read.
    for (method, path) in ADMIN_ROUTES.iter().filter(|(m, _)| *m == "GET") {
        let res = call(&server, method, path, &main_token).await;
        assert!(
            !matches!(
                res.status_code(),
                StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
            ),
            "{method} {path} as main: {}",
            res.status_code()
        );
    }
}

/// `ADMIN_ROUTES` keeps up with the router: the number of route handlers taking `AdminUser`
/// in the source equals the list's length (each admin handler serves exactly one route).
#[test]
fn admin_route_list_is_complete() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes");
    let mut handlers = 0;
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for item in text.split("async fn ").skip(1) {
                let params = &item[..item.find('{').unwrap_or(item.len())];
                if params.contains("AdminUser") {
                    handlers += 1;
                }
            }
        }
    }
    assert_eq!(handlers, ADMIN_ROUTES.len());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_keeps_profile(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let refresh = state
        .jwt
        .issue_refresh(user.account(), kids.id, user.token_version);
    let server = TestServer::new(kima_api::router(state)).unwrap();

    let res = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": refresh }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    for key in ["token", "refreshToken"] {
        let token = body[key].as_str().unwrap();
        assert_eq!(claims(token)["profileId"], json!(kids.id), "{key}");
        assert_eq!(claims(token)["userId"], json!(user.id), "{key}");
    }
    let res = me(&server, body["token"].as_str().unwrap()).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["profile"]["id"], json!(kids.id));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refresh_for_deleted_profile_is_401(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let refresh = state
        .jwt
        .issue_refresh(user.account(), kids.id, user.token_version);
    profiles::delete(&state.pool, user.account(), kids.id)
        .await
        .unwrap()
        .unwrap();
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": refresh }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(res.json::<Value>(), json!({ "error": "Profile not found" }));

    // A refresh token naming another account's profile is refused the same way.
    let (bob, _) = common::user_with_token(&state, "bob").await;
    let bobs = add_profile(&state, &bob, "Kids", ProfileKind::Kids).await;
    let forged = state
        .jwt
        .issue_refresh(user.account(), bobs.id, user.token_version);
    let res = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": forged }))
        .await;
    res.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(res.json::<Value>(), json!({ "error": "Profile not found" }));
}

fn make_mp3(dest: &Path) {
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=3")
        .args(["-c:a", "libmp3lame", "-b:a", "320k"])
        .arg(dest)
        .status()
        .unwrap();
    assert!(status.success());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_token_keeps_profile_and_quality(pool: PgPool) {
    if !common::require_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let s = seed(&pool, Seed::new("Artist", "Album", Some(2001), "Song")).await;
    let file = root.path().join("Artist/Album/1-1 Song.mp3");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    make_mp3(&file);
    let original = std::fs::read(&file).unwrap();
    let state = common::state(pool)
        .with_library_root(root.path().into())
        .with_transcoder(Transcoder::new(cache.path(), 100 * 1024 * 1024));
    let (user, main) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let kids_token = access(&state, &user, kids.id);
    let server = TestServer::new(kima_api::router(state)).unwrap();

    for (token, quality) in [(&kids_token, "low"), (&main, "original")] {
        server
            .post("/api/settings")
            .authorization_bearer(token)
            .json(&json!({ "playbackQuality": quality }))
            .await
            .assert_status_ok();
    }
    let ticket = |token: String| {
        let server = &server;
        async move {
            let res = server
                .post("/api/auth/stream-ticket")
                .authorization_bearer(&token)
                .await;
            res.assert_status_ok();
            res.json::<Value>()["token"].as_str().unwrap().to_string()
        }
    };
    let kids_ticket = ticket(kids_token.clone()).await;
    let main_ticket = ticket(main.clone()).await;
    assert_eq!(claims(&kids_ticket)["profileId"], json!(kids.id));
    assert!(claims(&main_ticket).get("profileId").is_none());

    // The main ticket follows main's "original": the file as stored.
    let url = |t: &str| format!("/api/library/tracks/{}/stream?token={t}", s.track);
    let res = server.get(&url(&main_ticket)).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &original[..]);

    // The kids ticket follows kids' "low": once cached, the ETag names the quality.
    server.get(&url(&kids_ticket)).await.assert_status_ok();
    for _ in 0..50 {
        let r = server.get(&url(&kids_ticket)).await;
        if let Some(e) = r.maybe_header(ETAG) {
            assert!(e.to_str().unwrap().ends_with("-low\""), "{e:?}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("transcode never became cached");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn password_change_from_kids_is_403(pool: PgPool) {
    let state = common::state(pool);
    let (user, main) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let kids_token = access(&state, &user, kids.id);
    let server = TestServer::new(kima_api::router(state)).unwrap();

    let body = json!({ "currentPassword": "password1", "newPassword": "password2" });
    let res = server
        .post("/api/auth/change-password")
        .authorization_bearer(&kids_token)
        .json(&body)
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "Only the main profile can change the password" })
    );
    // Nothing changed: both sessions still work, the old password still signs in.
    me(&server, &kids_token).await.assert_status_ok();
    me(&server, &main).await.assert_status_ok();
    server
        .post("/api/auth/login")
        .json(&json!({ "username": "ana", "password": "password1" }))
        .await
        .assert_status_ok();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn password_change_revokes_every_profile(pool: PgPool) {
    let state = common::state(pool);
    let (user, main) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let kids_token = access(&state, &user, kids.id);
    let kids_refresh = state
        .jwt
        .issue_refresh(user.account(), kids.id, user.token_version);
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    // An app token of the kids profile.
    kima_db::api_keys::create(
        &state.pool,
        Uuid::now_v7(),
        user.account(),
        kids.id,
        user.token_version,
        "kids phone",
        &[7; 32],
        b"sealed",
    )
    .await
    .unwrap()
    .unwrap();
    let keys = |pool: PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM api_keys k JOIN profiles pr ON pr.id = k.profile_id \
             WHERE pr.user_id = $1",
        )
        .bind(user.id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(keys(state.pool.clone()).await, 1);

    let res = server
        .post("/api/auth/change-password")
        .authorization_bearer(&main)
        .json(&json!({ "currentPassword": "password1", "newPassword": "password2" }))
        .await;
    res.assert_status_ok();
    let fresh = res.json::<Value>()["token"].as_str().unwrap().to_string();
    assert!(claims(&fresh).get("profileId").is_none());

    me(&server, &main)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    me(&server, &kids_token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": kids_refresh }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    me(&server, &fresh).await.assert_status_ok();
    assert_eq!(keys(state.pool.clone()).await, 0);
}

/// The key structure of a JSON value: objects keep their keys (recursively), every other
/// value becomes `null`.
fn shape(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), shape(v))).collect())
        }
        _ => Value::Null,
    }
}

fn without_profile(mut value: Value) -> Value {
    value.as_object_mut().unwrap().remove("profile");
    value
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn login_and_me_add_profile_key(pool: PgPool) {
    let state = common::state(pool);
    let (user, _) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = server
        .post("/api/auth/login")
        .json(&json!({ "username": "ana", "password": "password1" }))
        .await;
    res.assert_status_ok();
    let login: Value = res.json();
    // Every old key is still there, unchanged in kind; `profile` is the only addition.
    let fixture: Value = serde_json::from_str(include_str!(
        "../../kima-api-types/tests/fixtures/login_response.json"
    ))
    .unwrap();
    assert_eq!(shape(&without_profile(login.clone())), shape(&fixture));
    assert_eq!(login["user"]["id"], json!(user.id));
    let main = &login["profile"];
    assert_eq!(main["id"], json!(user.id));
    assert_eq!(main["name"], "ana");
    assert_eq!(main["kind"], "main");
    assert_eq!(main["isMain"], true);
    assert_eq!(main["avatar"], Value::Null);
    assert!(main["createdAt"].is_string());
    // A login lands in the main profile: its tokens carry exactly upstream's claims.
    assert!(
        claims(login["token"].as_str().unwrap())
            .get("profileId")
            .is_none()
    );

    let me_fixture: Value = serde_json::from_str(include_str!(
        "../../kima-api-types/tests/fixtures/me_response.json"
    ))
    .unwrap();
    let res = me(&server, login["token"].as_str().unwrap()).await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(shape(&without_profile(body.clone())), shape(&me_fixture));
    assert_eq!(body["profile"], *main);

    // As kids: `id`/`username` stay the account's, `profile` is the current one.
    let res = me(&server, &access(&state, &user, kids.id)).await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["id"], json!(user.id));
    assert_eq!(body["username"], "ana");
    assert_eq!(body["profile"]["id"], json!(kids.id));
    assert_eq!(body["profile"]["name"], "Kids");
    assert_eq!(body["profile"]["kind"], "kids");
    assert_eq!(body["profile"]["isMain"], false);

    // Registration adds the new account's main profile the same way.
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(&state.pool)
        .await
        .unwrap();
    let res = server
        .post("/api/onboarding/register")
        .json(&json!({ "username": "carol", "password": "password1" }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    let register_fixture: Value = serde_json::from_str(include_str!(
        "../../kima-api-types/tests/fixtures/register_response.json"
    ))
    .unwrap();
    assert_eq!(
        shape(&without_profile(body.clone())),
        shape(&register_fixture)
    );
    assert_eq!(body["profile"]["id"], body["user"]["id"]);
    assert_eq!(body["profile"]["kind"], "main");
}

/// The podcast AI switch is a setting, so it is the session's profile's: a kids profile that
/// shows AI content sees it shown while main keeps the default (hide), and the other way round
/// (D-246). `X-Kima-Ai-Hidden` is sent exactly when the resolved filter hides.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn podcast_ai_switch_is_the_profiles_setting(pool: PgPool) {
    let state = common::state(pool);
    let (user, main) = common::user_with_token(&state, "ana").await;
    let kids = add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    let kids_token = access(&state, &user, kids.id);
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let set = |token: String, value: &'static str| {
        let server = &server;
        async move {
            server
                .post("/api/settings")
                .authorization_bearer(token)
                .json(&json!({ "podcastAiContent": value }))
                .await
                .assert_status_ok();
        }
    };
    let hides = |token: String| {
        let server = &server;
        async move {
            let mut seen = Vec::new();
            for path in [
                "/api/podcasts",
                "/api/podcasts/listening",
                "/api/library/recently-listened",
            ] {
                let res = server.get(path).authorization_bearer(&token).await;
                res.assert_status_ok();
                seen.push((path, res.headers().contains_key("x-kima-ai-hidden")));
            }
            seen
        }
    };
    let all = |hide: bool| {
        [
            "/api/podcasts",
            "/api/podcasts/listening",
            "/api/library/recently-listened",
        ]
        .map(|path| (path, hide))
        .to_vec()
    };

    set(kids_token.clone(), "show").await;
    assert_eq!(hides(kids_token.clone()).await, all(false), "kids shows");
    assert_eq!(hides(main.clone()).await, all(true), "main keeps hide");

    set(kids_token.clone(), "hide").await;
    set(main.clone(), "show").await;
    assert_eq!(hides(kids_token).await, all(true), "kids hides");
    assert_eq!(hides(main).await, all(false), "main shows");
}
