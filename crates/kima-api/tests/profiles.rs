//! M-PROF Task 6: `/api/profiles` (D-243) — list, create, update, delete and switch. Management
//! is the main profile's (owner answer Q3); listing and switching are open to every profile of
//! the account; another account's profile is 404 on every route, even for an admin.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum_test::TestServer;
use common::library::{Seed, Seeded, seed};
use http_body_util::BodyExt;
use kima_api::AppState;
use kima_core::events::Event;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_db::profiles::{self, NewProfile, Profile};
use kima_db::users::User;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

/// An account with its main profile, a kids and a standard profile, and a token for each.
struct Family {
    user: User,
    kids: Profile,
    teen: Profile,
    main_token: String,
    kids_token: String,
    teen_token: String,
}

impl Family {
    fn main(&self) -> ProfileId {
        self.user.main_profile()
    }
}

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

fn token_for(state: &AppState, user: &User, profile: ProfileId) -> String {
    state.jwt.issue_access(
        user.account(),
        profile,
        &user.username,
        &user.role,
        user.token_version,
    )
}

async fn family(state: &AppState, name: &str) -> Family {
    let (user, main_token) = common::user_with_token(state, name).await;
    let kids = add_profile(state, &user, "Kids", ProfileKind::Kids).await;
    let teen = add_profile(state, &user, "Teen", ProfileKind::Standard).await;
    let kids_token = token_for(state, &user, kids.id);
    let teen_token = token_for(state, &user, teen.id);
    Family {
        user,
        kids,
        teen,
        main_token,
        kids_token,
        teen_token,
    }
}

async fn profile_count(pool: &PgPool, user: &User) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM profiles WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn stored(pool: &PgPool, id: ProfileId) -> Option<(String, String, Option<String>)> {
    sqlx::query_as("SELECT name, kind, avatar FROM profiles WHERE id = $1")
        .bind(id.into_uuid())
        .fetch_optional(pool)
        .await
        .unwrap()
}

fn error_of(res: &axum_test::TestResponse) -> String {
    res.json::<Value>()["error"].as_str().unwrap().to_owned()
}

// ---- list ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn list_shows_account_profiles_and_current(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let other = family(&state, "ben").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = server
        .get("/api/profiles")
        .authorization_bearer(&f.kids_token)
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["current"], json!(f.kids.id));
    assert_eq!(body["max"], 5);
    let listed: Vec<(Value, Value, Value)> = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].clone(), p["kind"].clone(), p["isMain"].clone()))
        .collect();
    assert_eq!(
        listed,
        vec![
            (json!(f.main()), json!("main"), json!(true)),
            (json!(f.kids.id), json!("kids"), json!(false)),
            (json!(f.teen.id), json!("standard"), json!(false)),
        ],
        "main first, then by creation; no other account's profile"
    );
    let first = &body["profiles"][0];
    assert_eq!(first["name"], "ana");
    assert_eq!(first["avatar"], Value::Null);
    assert!(first["createdAt"].as_str().unwrap().contains('T'));

    // Every profile of the account sees the same list, with itself as current.
    let main: Value = server
        .get("/api/profiles")
        .authorization_bearer(&f.main_token)
        .await
        .json();
    assert_eq!(main["profiles"], body["profiles"]);
    assert_eq!(main["current"], json!(f.main()));
    let ben: Value = server
        .get("/api/profiles")
        .authorization_bearer(&other.kids_token)
        .await
        .json();
    assert!(
        ben["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"] != json!(f.kids.id) && p["id"] != json!(f.main())),
        "{ben}"
    );

    server
        .get("/api/profiles")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

// ---- create ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn main_creates_kids_and_standard(pool: PgPool) {
    let state = common::state(pool);
    let (user, main_token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = server
        .post("/api/profiles")
        .authorization_bearer(&main_token)
        .json(&json!({ "name": "  Little Ola  ", "kind": "kids", "avatar": "fox" }))
        .await;
    res.assert_status(StatusCode::CREATED);
    let kids: Value = res.json();
    assert_eq!(kids["name"], "Little Ola", "the name is trimmed");
    assert_eq!(kids["kind"], "kids");
    assert_eq!(kids["avatar"], "fox");
    assert_eq!(kids["isMain"], false);
    let kids_id: ProfileId = kids["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        stored(&state.pool, kids_id).await,
        Some(("Little Ola".into(), "kids".into(), Some("fox".into())))
    );

    // More than one kids profile is fine (owner answer Q2); the avatar is optional.
    for (name, kind) in [("Second kid", "kids"), ("Guest", "standard")] {
        let res = server
            .post("/api/profiles")
            .authorization_bearer(&main_token)
            .json(&json!({ "name": name, "kind": kind }))
            .await;
        res.assert_status(StatusCode::CREATED);
        assert_eq!(res.json::<Value>()["avatar"], Value::Null);
    }
    assert_eq!(profile_count(&state.pool, &user).await, 4);

    // A new profile starts with default settings (Q9), not a copy of main's.
    let res = server
        .post("/api/settings")
        .authorization_bearer(&main_token)
        .json(&json!({ "playbackQuality": "low" }))
        .await;
    res.assert_status_ok();
    let switched: Value = server
        .post(&format!("/api/profiles/{kids_id}/switch"))
        .authorization_bearer(&main_token)
        .json(&json!({}))
        .await
        .json();
    let settings: Value = server
        .get("/api/settings")
        .authorization_bearer(switched["token"].as_str().unwrap())
        .await
        .json();
    assert_eq!(settings["profileId"], json!(kids_id));
    assert_eq!(settings["playbackQuality"], "original");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn create_main_kind_is_400(pool: PgPool) {
    let state = common::state(pool);
    let (user, main_token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let long = "x".repeat(51);
    for body in [
        json!({ "name": "Second main", "kind": "main" }),
        json!({ "name": "Guest", "kind": "guest" }),
        json!({ "name": "Guest" }),
        json!({ "kind": "kids" }),
        json!({ "name": "", "kind": "kids" }),
        json!({ "name": "   ", "kind": "kids" }),
        json!({ "name": long, "kind": "kids" }),
        json!({ "name": "Kids", "kind": "kids", "avatar": "unicorn-xl" }),
        json!({ "name": "Kids", "kind": "kids", "avatar": "Fox" }),
        json!({ "name": "Kids", "kind": "kids", "pin": "1234" }),
    ] {
        let res = server
            .post("/api/profiles")
            .authorization_bearer(&main_token)
            .json(&body)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert!(res.json::<Value>()["error"].is_string(), "{body}");
    }
    // Fifty characters is the limit, not past it.
    server
        .post("/api/profiles")
        .authorization_bearer(&main_token)
        .json(&json!({ "name": "y".repeat(50), "kind": "standard" }))
        .await
        .assert_status(StatusCode::CREATED);
    assert_eq!(profile_count(&state.pool, &user).await, 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sixth_profile_is_409(pool: PgPool) {
    let state = common::state(pool);
    let (user, main_token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    for n in 0..4 {
        server
            .post("/api/profiles")
            .authorization_bearer(&main_token)
            .json(&json!({ "name": format!("P{n}"), "kind": "standard" }))
            .await
            .assert_status(StatusCode::CREATED);
    }
    let res = server
        .post("/api/profiles")
        .authorization_bearer(&main_token)
        .json(&json!({ "name": "P5", "kind": "kids" }))
        .await;
    res.assert_status(StatusCode::CONFLICT);
    assert_eq!(error_of(&res), "Profile limit reached");
    assert_eq!(profile_count(&state.pool, &user).await, 5);
}

/// The limit holds for requests racing each other: the database decides under a lock on the
/// account row, so however the requests interleave, the account ends with five profiles.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_creates_respect_the_limit(pool: PgPool) {
    let state = common::state(pool);
    let (user, main_token) = common::user_with_token(&state, "ana").await;
    add_profile(&state, &user, "Kids", ProfileKind::Kids).await;
    add_profile(&state, &user, "Teen", ProfileKind::Standard).await;
    let app = kima_api::router(state.clone());

    let requests = (0..6).map(|n| {
        let app = app.clone();
        let token = main_token.clone();
        tokio::spawn(async move {
            app.oneshot(
                Request::post("/api/profiles")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": format!("Racer {n}"), "kind": "standard" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        })
    });
    let mut statuses = Vec::new();
    for request in requests.collect::<Vec<_>>() {
        statuses.push(request.await.unwrap());
    }
    statuses.sort();
    assert_eq!(
        statuses,
        vec![
            StatusCode::CREATED,
            StatusCode::CREATED,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT,
        ]
    );
    assert_eq!(profile_count(&state.pool, &user).await, 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn duplicate_name_any_case_is_400(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let other = family(&state, "ben").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = server
        .post("/api/profiles")
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "kIDS", "kind": "standard" }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&res), "Profile name already taken");
    // The main profile's name (the username at first) is taken too.
    server
        .post("/api/profiles")
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "ANA", "kind": "standard" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    // Renaming onto a sibling's name, any case.
    let res = server
        .put(&format!("/api/profiles/{}", f.teen.id))
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "KiDs" }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&res), "Profile name already taken");
    assert_eq!(stored(&state.pool, f.teen.id).await.unwrap().0, "Teen");
    // Another account may use the same names; a profile may change the case of its own.
    server
        .post("/api/profiles")
        .authorization_bearer(&other.main_token)
        .json(&json!({ "name": "ana", "kind": "kids" }))
        .await
        .assert_status(StatusCode::CREATED);
    server
        .put(&format!("/api/profiles/{}", f.kids.id))
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "KIDS" }))
        .await
        .assert_status_ok();
}

// ---- management is the main profile's ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn kids_cannot_create_rename_or_delete_403(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    for token in [&f.kids_token, &f.teen_token] {
        let res = server
            .post("/api/profiles")
            .authorization_bearer(token)
            .json(&json!({ "name": "Sneaky", "kind": "standard" }))
            .await;
        res.assert_status(StatusCode::FORBIDDEN);
        assert_eq!(error_of(&res), "Only the main profile can manage profiles");
        for target in [f.kids.id, f.teen.id, f.main()] {
            for body in [
                json!({ "kind": "standard" }),
                json!({ "kind": "main" }),
                json!({ "name": "Renamed" }),
                json!({ "avatar": "fox" }),
            ] {
                server
                    .put(&format!("/api/profiles/{target}"))
                    .authorization_bearer(token)
                    .json(&body)
                    .await
                    .assert_status(StatusCode::FORBIDDEN);
            }
            server
                .delete(&format!("/api/profiles/{target}"))
                .authorization_bearer(token)
                .await
                .assert_status(StatusCode::FORBIDDEN);
        }
    }
    assert_eq!(profile_count(&state.pool, &f.user).await, 3);
    assert_eq!(
        stored(&state.pool, f.kids.id).await,
        Some(("Kids".into(), "kids".into(), None))
    );
    assert_eq!(
        stored(&state.pool, f.main()).await,
        Some(("ana".into(), "main".into(), None))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn main_updates_name_kind_and_avatar(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    let path = format!("/api/profiles/{}", f.kids.id);

    let res = server
        .put(&path)
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": " Ola ", "kind": "standard", "avatar": "owl" }))
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(
        (&body["id"], &body["name"], &body["kind"], &body["avatar"]),
        (
            &json!(f.kids.id),
            &json!("Ola"),
            &json!("standard"),
            &json!("owl")
        )
    );
    // Absent fields stay; `null` clears the avatar.
    let body: Value = server
        .put(&path)
        .authorization_bearer(&f.main_token)
        .json(&json!({ "avatar": null }))
        .await
        .json();
    assert_eq!(
        (&body["name"], &body["avatar"]),
        (&json!("Ola"), &Value::Null)
    );
    assert_eq!(
        stored(&state.pool, f.kids.id).await,
        Some(("Ola".into(), "standard".into(), None))
    );
    // Invalid values are 400 and change nothing.
    for bad in [
        json!({ "name": "" }),
        json!({ "avatar": "dragon-king-of-everything" }),
        json!({ "kind": "main" }),
        json!({ "kind": "guest" }),
        json!({ "nickname": "x" }),
    ] {
        server
            .put(&path)
            .authorization_bearer(&f.main_token)
            .json(&bad)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        stored(&state.pool, f.kids.id).await,
        Some(("Ola".into(), "standard".into(), None))
    );

    // Main renames itself and picks an avatar, but its kind never changes.
    let main_path = format!("/api/profiles/{}", f.main());
    let body: Value = server
        .put(&main_path)
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "Mum", "avatar": "cat", "kind": "main" }))
        .await
        .json();
    assert_eq!(
        (&body["name"], &body["isMain"]),
        (&json!("Mum"), &json!(true))
    );
    let res = server
        .put(&main_path)
        .authorization_bearer(&f.main_token)
        .json(&json!({ "kind": "kids" }))
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        stored(&state.pool, f.main()).await,
        Some(("Mum".into(), "main".into(), Some("cat".into())))
    );
    // The account itself is not renamed (D-240).
    let me: Value = server
        .get("/api/auth/me")
        .authorization_bearer(&f.main_token)
        .await
        .json();
    assert_eq!(me["username"], "ana");
    assert_eq!(me["profile"]["name"], "Mum");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_main_is_400(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let res = server
        .delete(&format!("/api/profiles/{}", f.main()))
        .authorization_bearer(&f.main_token)
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(error_of(&res), "Cannot delete the main profile");
    assert_eq!(profile_count(&state.pool, &f.user).await, 3);
    server
        .get("/api/profiles")
        .authorization_bearer(&f.main_token)
        .await
        .assert_status_ok();
}

// ---- delete: data and sessions ----

/// The 13 per-profile tables (D-240).
const PERSONAL_TABLES: [&str; 13] = [
    "plays",
    "playback_state",
    "playlists",
    "hidden_playlists",
    "api_keys",
    "subsonic_bookmarks",
    "stars",
    "ratings",
    "user_settings",
    "play_audit_ignore",
    "podcast_subscriptions",
    "podcast_progress",
    "podcast_downloads",
];

struct Library {
    track: Seeded,
    show: Uuid,
    episode: Uuid,
}

async fn library(pool: &PgPool) -> Library {
    let track = seed(pool, Seed::new("Artist", "Album", Some(2001), "Song")).await;
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: "https://feed.example.invalid/show",
            title: "Show",
            author: Some("Host"),
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 1,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap()
    .id;
    podcasts::insert_episodes(
        pool,
        show,
        &[NewEpisode {
            guid: "show-1".into(),
            title: "Episode".into(),
            description: None,
            audio_url: "https://a.invalid/show-1.mp3".into(),
            duration: 600,
            published_at: OffsetDateTime::now_utc(),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        }],
    )
    .await
    .unwrap();
    let episode = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE podcast_id = $1")
        .bind(show)
        .fetch_one(pool)
        .await
        .unwrap();
    Library {
        track,
        show,
        episode,
    }
}

/// One row in every per-profile table for `profile`.
async fn own_everything(pool: &PgPool, profile: ProfileId, lib: &Library) {
    let p = profile.into_uuid();
    let t = lib.track.track;
    let playlist = Uuid::now_v7();
    let statements: [(&str, Vec<Uuid>); 13] = [
        (
            "INSERT INTO plays (id, profile_id, track_id) VALUES ($1, $2, $3)",
            vec![Uuid::now_v7(), p, t],
        ),
        (
            "INSERT INTO playback_state (profile_id, playback_type) VALUES ($1, 'track')",
            vec![p],
        ),
        (
            "INSERT INTO playlists (id, profile_id, name) VALUES ($1, $2, 'Mine')",
            vec![playlist, p],
        ),
        (
            "INSERT INTO hidden_playlists (profile_id, playlist_id) VALUES ($1, $2)",
            vec![p, playlist],
        ),
        (
            "INSERT INTO api_keys (id, profile_id, name, key_hash, secret_enc)
             VALUES ($1, $2, 'phone', sha256(convert_to($1::text, 'UTF8')), '\\x00')",
            vec![Uuid::now_v7(), p],
        ),
        (
            "INSERT INTO subsonic_bookmarks (profile_id, track_id, position_ms) VALUES ($1, $2, 5)",
            vec![p, t],
        ),
        (
            "INSERT INTO stars (id, profile_id, album_id) VALUES ($1, $2, $3)",
            vec![Uuid::now_v7(), p, lib.track.album],
        ),
        (
            "INSERT INTO ratings (id, profile_id, artist_id, rating) VALUES ($1, $2, $3, 4)",
            vec![Uuid::now_v7(), p, lib.track.artist],
        ),
        (
            "INSERT INTO user_settings (profile_id, playback_quality) VALUES ($1, 'low')",
            vec![p],
        ),
        (
            "INSERT INTO play_audit_ignore (profile_id, track_id) VALUES ($1, $2)",
            vec![p, t],
        ),
        (
            "INSERT INTO podcast_subscriptions (profile_id, podcast_id) VALUES ($1, $2)",
            vec![p, lib.show],
        ),
        (
            "INSERT INTO podcast_progress (profile_id, episode_id, \"current_time\") VALUES ($1, $2, 9)",
            vec![p, lib.episode],
        ),
        (
            "INSERT INTO podcast_downloads (profile_id, episode_id, local_path, file_size_mb)
             VALUES ($1, $2, 'x.mp3', 1)",
            vec![p, lib.episode],
        ),
    ];
    for (sql, binds) in statements {
        let mut query = sqlx::query(sql);
        for bind in binds {
            query = query.bind(bind);
        }
        query.execute(pool).await.unwrap();
    }
}

/// Rows per personal table that `profile` owns.
async fn rows_of(pool: &PgPool, profile: ProfileId) -> Vec<i64> {
    let mut counts = Vec::new();
    for table in PERSONAL_TABLES {
        counts.push(
            sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE profile_id = $1"
            ))
            .bind(profile.into_uuid())
            .fetch_one(pool)
            .await
            .unwrap(),
        );
    }
    counts
}

async fn ticket(server: &TestServer, token: &str) -> String {
    let res = server
        .post("/api/events/ticket")
        .authorization_bearer(token)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["ticket"].as_str().unwrap().to_owned()
}

async fn open_stream(app: &axum::Router, ticket: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::get(format!("/api/events?ticket={ticket}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn next_event(body: &mut Body) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("an event within 2 s")
        .expect("stream still open")
        .unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    serde_json::from_str(text.strip_prefix("data: ").unwrap().trim()).unwrap()
}

async fn stream_ends(body: &mut Body) {
    let end = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("the stream ends within 2 s");
    assert!(end.is_none(), "no frame after the revocation");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_cascades_and_revokes_its_sessions(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let other = family(&state, "ben").await;
    let lib = library(&state.pool).await;
    for profile in [f.main(), f.kids.id, f.teen.id, other.kids.id] {
        own_everything(&state.pool, profile, &lib).await;
    }
    let everything = vec![1; PERSONAL_TABLES.len()];
    assert_eq!(rows_of(&state.pool, f.kids.id).await, everything);
    let app = kima_api::router(state.clone());
    let server = TestServer::new(app.clone()).unwrap();

    let mut kids = open_stream(&app, &ticket(&server, &f.kids_token).await)
        .await
        .into_body();
    let mut main = open_stream(&app, &ticket(&server, &f.main_token).await)
        .await
        .into_body();
    let mut teen = open_stream(&app, &ticket(&server, &f.teen_token).await)
        .await
        .into_body();
    for body in [&mut kids, &mut main, &mut teen] {
        assert_eq!(next_event(body).await["type"], "connected");
    }
    // A ticket issued to kids but not redeemed yet dies with the profile.
    let pending = ticket(&server, &f.kids_token).await;

    let res = server
        .delete(&format!("/api/profiles/{}", f.kids.id))
        .authorization_bearer(&f.main_token)
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>(), json!({ "message": "Profile deleted" }));

    // Its data is gone, everyone else's untouched.
    assert_eq!(rows_of(&state.pool, f.kids.id).await, vec![0; 13]);
    for profile in [f.main(), f.teen.id, other.kids.id] {
        assert_eq!(rows_of(&state.pool, profile).await, everything, "{profile}");
    }
    assert_eq!(stored(&state.pool, f.kids.id).await, None);
    assert_eq!(profile_count(&state.pool, &f.user).await, 2);

    // Its open stream ends at once, its pending ticket opens nothing, its token is dead.
    stream_ends(&mut kids).await;
    assert_eq!(
        open_stream(&app, &pending).await.status(),
        StatusCode::UNAUTHORIZED
    );
    server
        .get("/api/profiles")
        .authorization_bearer(&f.kids_token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    // Siblings keep streaming and signing in.
    state
        .events
        .publish(Event::broadcast(json!({ "type": "sentinel" })));
    assert_eq!(next_event(&mut main).await["type"], "sentinel");
    assert_eq!(next_event(&mut teen).await["type"], "sentinel");
    server
        .get("/api/profiles")
        .authorization_bearer(&f.teen_token)
        .await
        .assert_status_ok();

    // Deleting it again is 404.
    server
        .delete(&format!("/api/profiles/{}", f.kids.id))
        .authorization_bearer(&f.main_token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ---- isolation between accounts ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn foreign_profile_is_404_for_every_route(pool: PgPool) {
    let state = common::state(pool);
    // The first account is the server's admin: admin rights do not reach other accounts'
    // profiles (owner answer Q4).
    let admin = family(&state, "root").await;
    assert_eq!(admin.user.role, kima_db::users::ROLE_ADMIN);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let foreign = [f.kids.id.to_string(), f.main().to_string()];
    let unknown = [
        Uuid::now_v7().to_string(),
        "not-a-uuid".to_owned(),
        "0".to_owned(),
    ];
    for id in foreign.iter().chain(&unknown) {
        let res = server
            .put(&format!("/api/profiles/{id}"))
            .authorization_bearer(&admin.main_token)
            .json(&json!({ "name": "Hijacked" }))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(error_of(&res), "Profile not found", "{id}");
        server
            .delete(&format!("/api/profiles/{id}"))
            .authorization_bearer(&admin.main_token)
            .await
            .assert_status(StatusCode::NOT_FOUND);
        let res = server
            .post(&format!("/api/profiles/{id}/switch"))
            .authorization_bearer(&admin.main_token)
            .json(&json!({}))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(error_of(&res), "Profile not found", "{id}");
        // Also from a non-main profile (the switch is open to it).
        server
            .post(&format!("/api/profiles/{id}/switch"))
            .authorization_bearer(&admin.kids_token)
            .json(&json!({}))
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }
    assert_eq!(
        stored(&state.pool, f.kids.id).await,
        Some(("Kids".into(), "kids".into(), None))
    );
    assert_eq!(profile_count(&state.pool, &f.user).await, 3);
}

// ---- switch ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_returns_working_pair(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    // Main's private data.
    server
        .post("/api/playlists")
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "Main only" }))
        .await
        .assert_status_ok();

    let res = server
        .post(&format!("/api/profiles/{}/switch", f.kids.id))
        .authorization_bearer(&f.main_token)
        .json(&json!({}))
        .await;
    res.assert_status_ok();
    let pair: Value = res.json();
    assert_eq!(pair["profile"]["id"], json!(f.kids.id));
    assert_eq!(pair["profile"]["kind"], "kids");
    let token = pair["token"].as_str().unwrap();
    let refresh = pair["refreshToken"].as_str().unwrap();

    // The access token acts as kids...
    let me: Value = server
        .get("/api/auth/me")
        .authorization_bearer(token)
        .await
        .json();
    assert_eq!(me["id"], json!(f.user.id), "`id` stays the account's");
    assert_eq!(me["profile"]["id"], json!(f.kids.id));
    let playlists: Value = server
        .get("/api/playlists")
        .authorization_bearer(token)
        .await
        .json();
    assert_eq!(playlists, json!([]), "main's private playlist is not kids'");
    server
        .post("/api/playlists")
        .authorization_bearer(token)
        .json(&json!({ "name": "Kids only" }))
        .await
        .assert_status_ok();
    let mains: Value = server
        .get("/api/playlists")
        .authorization_bearer(&f.main_token)
        .await
        .json();
    assert_eq!(mains.as_array().unwrap().len(), 1, "{mains}");
    assert_eq!(mains[0]["name"], "Main only");
    // ...and is no admin, though the account is (the first account on the server).
    assert_eq!(f.user.role, kima_db::users::ROLE_ADMIN);
    server
        .get("/api/auth/users")
        .authorization_bearer(token)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // The refresh token keeps the profile.
    let refreshed: Value = server
        .post("/api/auth/refresh")
        .json(&json!({ "refreshToken": refresh }))
        .await
        .json();
    let me: Value = server
        .get("/api/auth/me")
        .authorization_bearer(refreshed["token"].as_str().unwrap())
        .await
        .json();
    assert_eq!(me["profile"]["id"], json!(f.kids.id));

    // Any profile may switch, back to main included (no PIN yet, D-241/D-249).
    let back: Value = server
        .post(&format!("/api/profiles/{}/switch", f.main()))
        .authorization_bearer(token)
        .json(&json!({}))
        .await
        .json();
    assert_eq!(back["profile"]["isMain"], true);
    server
        .get("/api/auth/users")
        .authorization_bearer(back["token"].as_str().unwrap())
        .await
        .assert_status_ok();

    // A password change ends the switched sessions too.
    server
        .post("/api/auth/change-password")
        .authorization_bearer(&f.main_token)
        .json(&json!({ "currentPassword": "password1", "newPassword": "password2" }))
        .await
        .assert_status_ok();
    server
        .get("/api/auth/me")
        .authorization_bearer(token)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_to_foreign_profile_is_404(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let other = family(&state, "ben").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    for id in [other.kids.id, other.main()] {
        let res = server
            .post(&format!("/api/profiles/{id}/switch"))
            .authorization_bearer(&f.kids_token)
            .json(&json!({}))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(error_of(&res), "Profile not found");
    }
    // Without a session nothing switches; a deleted profile's session neither.
    server
        .post(&format!("/api/profiles/{}/switch", f.kids.id))
        .json(&json!({}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    profiles::delete(&state.pool, f.user.account(), f.teen.id)
        .await
        .unwrap()
        .unwrap();
    server
        .post(&format!("/api/profiles/{}/switch", f.main()))
        .authorization_bearer(&f.teen_token)
        .json(&json!({}))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_ignores_pin_field(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    let path = format!("/api/profiles/{}/switch", f.kids.id);

    // No PIN exists yet: any `pin` (or none, or no body at all) switches.
    for body in [json!({ "pin": "0000" }), json!({}), json!({ "pin": null })] {
        let res = server
            .post(&path)
            .authorization_bearer(&f.main_token)
            .json(&body)
            .await;
        res.assert_status_ok();
        assert_eq!(res.json::<Value>()["profile"]["id"], json!(f.kids.id));
    }
    let res = server.post(&path).authorization_bearer(&f.main_token).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["profile"]["id"], json!(f.kids.id));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn switch_rejects_unknown_fields_400(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    let path = format!("/api/profiles/{}/switch", f.kids.id);

    for body in [
        json!({ "code": "0000" }),
        json!({ "pin": 1234 }),
        json!({ "pin": "0000", "profileId": f.main() }),
        json!([]),
    ] {
        let res = server
            .post(&path)
            .authorization_bearer(&f.main_token)
            .json(&body)
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert!(res.json::<Value>()["error"].is_string(), "{body}");
    }
    let res = server
        .post(&path)
        .authorization_bearer(&f.main_token)
        .text("{not json")
        .content_type("application/json")
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
}
