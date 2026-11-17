//! M-PROF (plan Task 5): every personal REST route, event and SSE stream belongs to the session's
//! profile (D-240, D-245). Each test acts as a non-main profile: the main profile's id is the
//! account's id, so a route that read the account instead would pass unseen on main.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum_test::TestServer;
use common::library::{Seed, seed};
use http_body_util::BodyExt;
use kima_api::AppState;
use kima_core::events::Event;
use kima_core::profile::ProfileKind;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_db::profiles::{self, NewProfile, Profile};
use kima_db::users::User;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

/// An account `name` with its main profile and a kids profile, plus a token for each.
struct Family {
    user: User,
    kids: Profile,
    main_token: String,
    kids_token: String,
}

async fn family(state: &AppState, name: &str) -> Family {
    let (user, main_token) = common::user_with_token(state, name).await;
    let kids = profiles::create(
        &state.pool,
        user.account(),
        NewProfile {
            name: "Kids".into(),
            kind: ProfileKind::Kids,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let kids_token = state.jwt.issue_access(
        user.account(),
        kids.id,
        &user.username,
        &user.role,
        user.token_version,
    );
    Family {
        user,
        kids,
        main_token,
        kids_token,
    }
}

async fn get(server: &TestServer, token: &str, path: &str) -> Value {
    let res = server.get(path).authorization_bearer(token).await;
    res.assert_status_ok();
    res.json()
}

async fn post(server: &TestServer, token: &str, path: &str, body: Value) -> Value {
    let res = server
        .post(path)
        .authorization_bearer(token)
        .json(&body)
        .await;
    res.assert_status_ok();
    res.json()
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

/// The next `data:` payload of an SSE body, as JSON.
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

fn broadcast(state: &AppState, n: i32) {
    state
        .events
        .publish(Event::broadcast(json!({ "type": "sentinel", "n": n })));
}

// ---- events and SSE (D-245) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sse_ticket_and_events_are_per_profile(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let app = kima_api::router(state.clone());
    let server = TestServer::new(app.clone()).unwrap();

    let kids_ticket = ticket(&server, &f.kids_token).await;
    let main_ticket = ticket(&server, &f.main_token).await;
    let mut kids = open_stream(&app, &kids_ticket).await.into_body();
    let mut main = open_stream(&app, &main_ticket).await.into_body();
    assert_eq!(next_event(&mut kids).await["type"], "connected");
    assert_eq!(next_event(&mut main).await["type"], "connected");

    // A playlist change made by kids reaches kids' devices...
    let created = post(
        &server,
        &f.kids_token,
        "/api/playlists",
        json!({ "name": "Songs for the car" }),
    )
    .await;
    let event = next_event(&mut kids).await;
    assert_eq!(event["type"], "playlist:updated");
    assert_eq!(event["playlistId"], created["id"]);

    // ...and not main's: the next thing main sees is the broadcast sent after it.
    broadcast(&state, 1);
    assert_eq!(
        next_event(&mut main).await,
        json!({ "type": "sentinel", "n": 1 })
    );
    assert_eq!(
        next_event(&mut kids).await,
        json!({ "type": "sentinel", "n": 1 })
    );

    // And the other way round.
    post(
        &server,
        &f.main_token,
        "/api/playlists",
        json!({ "name": "Grown-up" }),
    )
    .await;
    assert_eq!(next_event(&mut main).await["type"], "playlist:updated");
    broadcast(&state, 2);
    assert_eq!(
        next_event(&mut kids).await,
        json!({ "type": "sentinel", "n": 2 })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn account_revoke_closes_every_profile_stream(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let other = family(&state, "ben").await;
    let app = kima_api::router(state.clone());
    let server = TestServer::new(app.clone()).unwrap();

    let mut kids = open_stream(&app, &ticket(&server, &f.kids_token).await)
        .await
        .into_body();
    let mut main = open_stream(&app, &ticket(&server, &f.main_token).await)
        .await
        .into_body();
    let mut bens_kids = open_stream(&app, &ticket(&server, &other.kids_token).await)
        .await
        .into_body();
    for body in [&mut kids, &mut main, &mut bens_kids] {
        assert_eq!(next_event(body).await["type"], "connected");
    }
    // A ticket issued but not redeemed yet dies with the sessions too.
    let pending = ticket(&server, &f.kids_token).await;

    // A password change (from main) ends the sessions of every profile of the account.
    post(
        &server,
        &f.main_token,
        "/api/auth/change-password",
        json!({ "currentPassword": "password1", "newPassword": "password2" }),
    )
    .await;
    stream_ends(&mut kids).await;
    stream_ends(&mut main).await;
    assert_eq!(
        open_stream(&app, &pending).await.status(),
        StatusCode::UNAUTHORIZED
    );
    // Another account's profile is untouched.
    broadcast(&state, 1);
    assert_eq!(
        next_event(&mut bens_kids).await,
        json!({ "type": "sentinel", "n": 1 })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stream_cap_is_per_profile(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let app = kima_api::router(state.clone());
    let server = TestServer::new(app.clone()).unwrap();

    let mut open = Vec::new();
    for _ in 0..kima_api::events::MAX_STREAMS_PER_PROFILE {
        let res = open_stream(&app, &ticket(&server, &f.main_token).await).await;
        assert_eq!(res.status(), StatusCode::OK);
        open.push(res);
    }
    let res = open_stream(&app, &ticket(&server, &f.main_token).await).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    // Main's five streams leave kids' own five free.
    let res = open_stream(&app, &ticket(&server, &f.kids_token).await).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(state.sse.open_streams(f.kids.id), 1);
    assert_eq!(state.sse.open_streams(f.user.main_profile()), 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ticket_of_a_deleted_profile_opens_no_stream(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let app = kima_api::router(state.clone());
    let server = TestServer::new(app.clone()).unwrap();

    let pending = ticket(&server, &f.kids_token).await;
    profiles::delete(&state.pool, f.user.account(), f.kids.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        open_stream(&app, &pending).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

// ---- settings and playback state (D-242) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn settings_and_playback_state_add_profile_id_keep_user_id(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    let account = json!(f.user.id);

    let settings = get(&server, &f.kids_token, "/api/settings").await;
    assert_eq!(settings["userId"], account);
    assert_eq!(settings["profileId"], json!(f.kids.id));
    let settings = post(
        &server,
        &f.kids_token,
        "/api/settings",
        json!({ "playbackQuality": "low" }),
    )
    .await;
    assert_eq!(settings["userId"], account);
    assert_eq!(settings["profileId"], json!(f.kids.id));
    let main_settings = get(&server, &f.main_token, "/api/settings").await;
    assert_eq!(main_settings["profileId"], account);
    assert_ne!(main_settings["playbackQuality"], "low");

    let saved = post(
        &server,
        &f.kids_token,
        "/api/playback-state",
        json!({ "playbackType": "track", "trackId": "t1", "queue": [], "currentIndex": 0 }),
    )
    .await;
    assert_eq!(saved["userId"], account);
    assert_eq!(saved["profileId"], json!(f.kids.id));
    let read = get(&server, &f.kids_token, "/api/playback-state").await;
    assert_eq!(read["trackId"], "t1");
    assert_eq!(read["profileId"], json!(f.kids.id));
    // Main has no playback state of its own yet; clearing it leaves kids' alone.
    assert_eq!(
        get(&server, &f.main_token, "/api/playback-state").await,
        Value::Null
    );
    let res = server
        .delete("/api/playback-state")
        .authorization_bearer(&f.main_token)
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["cleared"], 0);
    assert_eq!(
        get(&server, &f.kids_token, "/api/playback-state").await["trackId"],
        "t1"
    );
}

// ---- playlists (D-242: `isOwner` = same profile) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn playlist_is_owner_is_per_profile(pool: PgPool) {
    let state = common::state(pool);
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let private = post(
        &server,
        &f.kids_token,
        "/api/playlists",
        json!({ "name": "Secret" }),
    )
    .await;
    let public = post(
        &server,
        &f.kids_token,
        "/api/playlists",
        json!({ "name": "Shared", "isPublic": true }),
    )
    .await;
    assert_eq!(private["userId"], json!(f.user.id));
    assert_eq!(private["profileId"], json!(f.kids.id));

    let mine = get(&server, &f.kids_token, "/api/playlists").await;
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 2);
    for p in mine {
        assert_eq!(p["isOwner"], true, "{p}");
        assert_eq!(p["profileId"], json!(f.kids.id));
        assert_eq!(p["profileName"], "Kids");
        assert_eq!(p["userId"], json!(f.user.id));
    }
    let detail = get(
        &server,
        &f.kids_token,
        &format!("/api/playlists/{}", private["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(detail["isOwner"], true);

    // Main sees kids' public playlist like anyone else's, not the private one.
    let theirs = get(&server, &f.main_token, "/api/playlists").await;
    assert_eq!(theirs.as_array().unwrap().len(), 1);
    assert_eq!(theirs[0]["id"], public["id"]);
    assert_eq!(theirs[0]["isOwner"], false);
    assert_eq!(theirs[0]["profileName"], "Kids");
    server
        .get(&format!(
            "/api/playlists/{}",
            private["id"].as_str().unwrap()
        ))
        .authorization_bearer(&f.main_token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server
        .put(&format!(
            "/api/playlists/{}",
            public["id"].as_str().unwrap()
        ))
        .authorization_bearer(&f.main_token)
        .json(&json!({ "name": "Taken over" }))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // Hiding is the viewer's own: main hides kids' public playlist, kids still see it.
    post(
        &server,
        &f.main_token,
        &format!("/api/playlists/{}/hide", public["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let theirs = get(&server, &f.main_token, "/api/playlists").await;
    assert_eq!(theirs[0]["isHidden"], true);
    let mine = get(&server, &f.kids_token, "/api/playlists").await;
    assert!(
        mine.as_array()
            .unwrap()
            .iter()
            .all(|p| p["isHidden"] == false)
    );
}

// ---- plays, stats, the play audit, recently listened (design (g)) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn plays_and_stats_are_per_profile(pool: PgPool) {
    let s = seed(&pool, Seed::new("Alpha", "A1", Some(2000), "Song")).await;
    let state = common::state(pool.clone());
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();

    let played = post(
        &server,
        &f.kids_token,
        "/api/plays",
        json!({ "trackId": s.track, "secondsListened": 120.0 }),
    )
    .await;
    assert_eq!(played["recorded"], true);
    let owner: Uuid = sqlx::query_scalar("SELECT profile_id FROM plays")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner, f.kids.id.into_uuid());

    let path = format!("/api/stats/track/{}/plays", s.track);
    assert_eq!(get(&server, &f.kids_token, &path).await["plays"], 1);
    assert_eq!(get(&server, &f.main_token, &path).await["plays"], 0);

    let recent = get(&server, &f.kids_token, "/api/library/recently-listened").await;
    assert_eq!(recent["items"][0]["id"], json!(s.artist), "{recent}");
    let recent = get(&server, &f.main_token, "/api/library/recently-listened").await;
    assert_eq!(recent["items"], json!([]), "{recent}");

    let audit = get(&server, &f.kids_token, "/api/stats/false-starts").await;
    assert_eq!(audit["totalRows"], 1);
    let audit = get(&server, &f.main_token, "/api/stats/false-starts").await;
    assert_eq!(audit["totalRows"], 0);
    // Ignoring a track is the profile's own.
    let ignored = post(
        &server,
        &f.kids_token,
        "/api/stats/false-starts/ignore",
        json!({ "trackIds": [s.track] }),
    )
    .await;
    assert_eq!(ignored, json!({ "ignored": 1 }));
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM play_audit_ignore WHERE profile_id = $1")
            .bind(f.kids.id.into_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);

    // The kids' play shows only in kids' stats.
    let stats = get(&server, &f.kids_token, "/api/stats?period=all").await;
    assert_eq!(stats["listening"]["plays"], 1, "{stats}");
    assert_eq!(stats["listening"]["distinctTracks"], 1, "{stats}");
    let main_stats = get(&server, &f.main_token, "/api/stats?period=all").await;
    assert_eq!(main_stats["listening"]["plays"], 0, "{main_stats}");
    assert_eq!(main_stats["listening"]["distinctTracks"], 0, "{main_stats}");
}

// ---- podcasts: follows and progress (design (h)) ----

async fn show_with_episodes(pool: &PgPool, title: &str) -> (Uuid, Vec<Uuid>) {
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &format!("https://feed.example.invalid/{title}"),
            title,
            author: Some("Host"),
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 2,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap()
    .id;
    let now = OffsetDateTime::now_utc();
    let episodes: Vec<NewEpisode> = (0..2)
        .map(|n| NewEpisode {
            guid: format!("{title}-{n}"),
            title: format!("{title} {n}"),
            description: None,
            audio_url: format!("https://a.invalid/{title}-{n}.mp3"),
            duration: 600,
            published_at: now - time::Duration::days(n),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        })
        .collect();
    podcasts::insert_episodes(pool, show, &episodes)
        .await
        .unwrap();
    let ids = sqlx::query_scalar(
        "SELECT id FROM podcast_episodes WHERE podcast_id = $1 ORDER BY published_at",
    )
    .bind(show)
    .fetch_all(pool)
    .await
    .unwrap();
    (show, ids)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn podcast_follows_and_progress_are_per_profile(pool: PgPool) {
    let state = common::state(pool.clone());
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    let (show, episodes) = show_with_episodes(&pool, "Bedtime").await;
    // Labels are unknown, so the AI filter hides nothing on any of these reads.
    assert!(podcasts::subscribe(&pool, f.kids.id, show).await.unwrap());

    let list = get(&server, &f.kids_token, "/api/podcasts").await;
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list[0]["id"], json!(show));
    assert_eq!(
        get(&server, &f.main_token, "/api/podcasts").await,
        json!([])
    );

    // The show page and its episodes need the profile's own follow.
    let page = get(&server, &f.kids_token, &format!("/api/podcasts/{show}")).await;
    assert_eq!(page["isSubscribed"], true);
    get(
        &server,
        &f.kids_token,
        &format!("/api/podcasts/{show}/episodes"),
    )
    .await;
    for path in [
        format!("/api/podcasts/{show}"),
        format!("/api/podcasts/{show}/episodes"),
    ] {
        server
            .get(&path)
            .authorization_bearer(&f.main_token)
            .await
            .assert_status(StatusCode::NOT_FOUND);
    }

    // Progress belongs to kids.
    let progress = format!("/api/podcasts/{show}/episodes/{}/progress", episodes[1]);
    post(
        &server,
        &f.kids_token,
        &progress,
        json!({ "currentTime": 120, "duration": 600, "isFinished": false }),
    )
    .await;
    let owners: Vec<Uuid> = sqlx::query_scalar("SELECT profile_id FROM podcast_progress")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(owners, vec![f.kids.id.into_uuid()]);
    let listening = get(&server, &f.kids_token, "/api/podcasts/listening").await;
    assert_eq!(
        listening["inProgress"].as_array().map(Vec::len),
        Some(1),
        "{listening}"
    );
    let listening = get(&server, &f.main_token, "/api/podcasts/listening").await;
    assert_eq!(
        listening["inProgress"].as_array().map(Vec::len),
        Some(0),
        "{listening}"
    );
    let next = format!("/api/podcasts/{show}/episodes/{}/next", episodes[0]);
    let kids_next = get(&server, &f.kids_token, &next).await;
    assert_eq!(
        kids_next["episode"]["progress"]["currentTime"], 120,
        "{kids_next}"
    );
    let main_next = get(&server, &f.main_token, &next).await;
    assert_ne!(
        main_next["episode"]["progress"]["currentTime"], 120,
        "{main_next}"
    );
    let recent = get(&server, &f.kids_token, "/api/library/recently-listened").await;
    assert_eq!(recent["items"][0]["type"], "podcast", "{recent}");

    // The podcast part of search covers the profile's follows.
    let found = get(
        &server,
        &f.kids_token,
        "/api/search?q=Bedtime&type=podcasts",
    )
    .await;
    assert_eq!(
        found["podcasts"].as_array().map(Vec::len),
        Some(1),
        "{found}"
    );
    let found = get(
        &server,
        &f.main_token,
        "/api/search?q=Bedtime&type=podcasts",
    )
    .await;
    assert_eq!(
        found["podcasts"].as_array().map(Vec::len),
        Some(0),
        "{found}"
    );

    // Main cannot unfollow kids' show; kids can.
    server
        .delete(&format!("/api/podcasts/{show}/unsubscribe"))
        .authorization_bearer(&f.main_token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    server
        .delete(&format!("/api/podcasts/{show}/unsubscribe"))
        .authorization_bearer(&f.kids_token)
        .await
        .assert_status_ok();
    assert_eq!(
        get(&server, &f.kids_token, "/api/podcasts").await,
        json!([])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn podcast_refresh_all_reads_the_profiles_follows(pool: PgPool) {
    let state = common::state(pool.clone());
    let f = family(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state.clone())).unwrap();
    let (show, _) = show_with_episodes(&pool, "Followed").await;
    let main = f.user.main_profile();
    assert!(podcasts::subscribe(&pool, main, show).await.unwrap());

    // Kids follows nothing: nothing to refresh, whatever main follows.
    let res = server
        .post("/api/podcasts/refresh-all")
        .authorization_bearer(&f.kids_token)
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["message"], "No subscribed podcasts");
}
