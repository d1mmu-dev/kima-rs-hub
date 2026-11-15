//! M-PROF Task 7 (D-244): a Subsonic sign-in lands in a profile. An app token signs into its
//! own profile (`apiKey`, `t`+`s`, the token sent as `p=` plain or `enc:`), the account password
//! into the main profile; `u` is always the account username. Everything a session writes or
//! reads lands in that profile; admin rights need the main profile; playlist `owner` and
//! now-playing `username` follow the D-244 display rule.

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::playback::{self, Patch};
use kima_db::podcasts::{self, NewShow};
use kima_db::profiles::{self, NewProfile};
use serde_json::Value;
use uuid::Uuid;

const WRONG: &str = "Wrong username or password";
const HINT: &str = "Wrong username or password. Token authentication needs an app token from \
                    Settings > Native Apps; your account password cannot be used with it.";

struct Env {
    server: TestServer,
    state: kima_api::AppState,
    pool: PgPool,
}

fn env(pool: PgPool) -> Env {
    let state = fixture_state(pool.clone());
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(state.clone()).layer(MockConnectInfo(peer));
    Env {
        server: TestServer::new(router).unwrap(),
        state,
        pool,
    }
}

macro_rules! fixture_test {
    ($name:ident, |$env:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $name(pool: PgPool) {
            let $env = env(pool);
            $body
        }
    };
}

/// A profile of `account` named `name`, and an app token of that profile, both created directly
/// in the database (the REST path, `/api/api-keys` from the profile's session, is
/// `kids_token_from_api_keys_signs_into_kids_until_deleted`).
async fn profile_with_token(
    env: &Env,
    account: Uuid,
    name: &str,
    kind: ProfileKind,
) -> (ProfileId, String) {
    let owner = kima_db::users::find_by_id(&env.pool, account)
        .await
        .unwrap()
        .unwrap();
    let profile = profiles::create(
        &env.pool,
        AccountId::new(account),
        NewProfile {
            name: name.into(),
            kind,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let id = Uuid::now_v7();
    let token = kima_core::app_token::generate();
    kima_db::api_keys::create(
        &env.pool,
        id,
        AccountId::new(account),
        profile.id,
        owner.token_version,
        "tablet",
        &kima_core::app_token::hash(&token),
        &kima_core::app_token::seal(&env.state.cipher, id, &token),
    )
    .await
    .unwrap()
    .unwrap();
    (profile.id, token)
}

async fn kids_of_alice(env: &Env) -> (ProfileId, String) {
    profile_with_token(env, ALICE_ID, "Kids", ProfileKind::Kids).await
}

async fn call(env: &Env, endpoint: &str, auth: &[(&str, &str)], params: &[(&str, &str)]) -> Value {
    let mut all = auth.to_vec();
    all.extend_from_slice(params);
    rest(&env.server, endpoint, &all).await
}

fn ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

fn failed(res: &Value, code: u16, message: &str) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
    assert_eq!(res["error"]["message"], message, "{res}");
}

/// The profiles that starred the track.
async fn starred_by(pool: &PgPool, track: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT profile_id FROM stars WHERE track_id = $1 ORDER BY profile_id")
        .bind(track)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Stars a fresh track with `auth` and answers which profile the star landed in.
async fn star_lands_in(env: &Env, auth: &[(&str, &str)], track: Uuid) -> Vec<Uuid> {
    ok(&call(env, "star", auth, &[("id", &track.to_string())]).await);
    starred_by(&env.pool, track).await
}

fn alice_key() -> [(&'static str, &'static str); 1] {
    [("apiKey", FIXTURE_TOKEN)]
}

fn ts_auth(u: &str, secret: &str) -> Vec<(String, String)> {
    let mut v = vec![("u".to_owned(), u.to_owned())];
    v.extend(ts_params(secret));
    v
}

fn refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
    v.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fixture_test!(api_key_of_kids_profile_signs_into_kids, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let landed = star_lands_in(&env, &[("apiKey", &token)], track_id(3)).await;
    assert_eq!(landed, vec![kids.into_uuid()]);
    // With `u` as the account username too.
    let landed = star_lands_in(&env, &[("apiKey", &token), ("u", "alice")], track_id(4)).await;
    assert_eq!(landed, vec![kids.into_uuid()]);
    // The main profile's token still lands in main.
    let landed = star_lands_in(&env, &alice_key(), track_id(5)).await;
    assert_eq!(landed, vec![ALICE_ID]);
    // tokenInfo answers the account username (the `u` a client sends with it).
    let info = call(&env, "tokenInfo", &[("apiKey", &token)], &[]).await;
    ok(&info);
    assert_eq!(info["tokenInfo"]["username"], "alice", "{info}");
});

fixture_test!(ts_salt_token_of_kids_profile_signs_into_kids, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let auth = ts_auth("alice", &token);
    assert_eq!(
        star_lands_in(&env, &refs(&auth), track_id(3)).await,
        vec![kids.into_uuid()]
    );
    // `u` folds case, as for main.
    let auth = ts_auth("ALICE", &token);
    assert_eq!(
        star_lands_in(&env, &refs(&auth), track_id(4)).await,
        vec![kids.into_uuid()]
    );
    let auth = ts_auth("alice", FIXTURE_TOKEN);
    assert_eq!(
        star_lands_in(&env, &refs(&auth), track_id(5)).await,
        vec![ALICE_ID]
    );
});

fixture_test!(token_as_p_of_kids_profile_signs_into_kids, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    assert_eq!(
        star_lands_in(&env, &[("u", "alice"), ("p", &token)], track_id(3)).await,
        vec![kids.into_uuid()]
    );
    let enc = format!("enc:{}", hex::encode(&token));
    assert_eq!(
        star_lands_in(&env, &[("u", "alice"), ("p", &enc)], track_id(4)).await,
        vec![kids.into_uuid()]
    );
});

fixture_test!(password_signs_into_main, |env| {
    kids_of_alice(&env).await;
    assert_eq!(
        star_lands_in(&env, &[("u", "alice"), ("p", ALICE_PASSWORD)], track_id(3)).await,
        vec![ALICE_ID]
    );
    let enc = format!("enc:{}", hex::encode(ALICE_PASSWORD));
    assert_eq!(
        star_lands_in(&env, &[("u", "alice"), ("p", &enc)], track_id(4)).await,
        vec![ALICE_ID]
    );
});

fixture_test!(u_must_be_account_username, |env| {
    let (_, token) = kids_of_alice(&env).await;
    failed(
        &call(&env, "ping", &[("apiKey", &token), ("u", "Kids")], &[]).await,
        40,
        WRONG,
    );
    failed(
        &call(&env, "ping", &refs(&ts_auth("Kids", &token)), &[]).await,
        40,
        HINT,
    );
    failed(
        &call(&env, "ping", &[("u", "Kids"), ("p", &token)], &[]).await,
        40,
        WRONG,
    );
    // Another account's username with this account's kids token: refused as well.
    failed(
        &call(&env, "ping", &refs(&ts_auth("admin", &token)), &[]).await,
        40,
        HINT,
    );
});

fixture_test!(deleted_profiles_token_signs_in_nowhere, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    ok(&call(&env, "ping", &[("apiKey", &token)], &[]).await);
    profiles::delete(&env.pool, AccountId::new(ALICE_ID), kids)
        .await
        .unwrap()
        .unwrap();
    failed(
        &call(&env, "ping", &[("apiKey", &token)], &[]).await,
        44,
        "Invalid API key",
    );
    failed(
        &call(&env, "ping", &refs(&ts_auth("alice", &token)), &[]).await,
        40,
        HINT,
    );
    failed(
        &call(&env, "ping", &[("u", "alice"), ("p", &token)], &[]).await,
        40,
        WRONG,
    );
    failed(
        &call(&env, "tokenInfo", &[("apiKey", &token)], &[]).await,
        44,
        "Invalid API key",
    );
});

/// The REST side of the same server (`/api/profiles`, `/api/api-keys`) and an access token of
/// Alice's main profile.
async fn rest_api(env: &Env) -> (TestServer, String) {
    let alice = kima_db::users::find_by_id(&env.pool, ALICE_ID)
        .await
        .unwrap()
        .unwrap();
    let token = env.state.jwt.issue_access(
        alice.account(),
        alice.main_profile(),
        &alice.username,
        &alice.role,
        alice.token_version,
    );
    let server = TestServer::new(kima_api::router(env.state.clone())).unwrap();
    (server, token)
}

// Task 8 (D-244), end to end: a token minted by a kids session over `/api/api-keys` signs into kids, and
// deleting the profile over `DELETE /api/profiles/{id}` leaves it signing in nowhere (44).
fixture_test!(
    kids_token_from_api_keys_signs_into_kids_until_deleted,
    |env| {
        let (api, main) = rest_api(&env).await;
        let res = api
            .post("/api/profiles")
            .authorization_bearer(&main)
            .json(&serde_json::json!({ "name": "Kids", "kind": "kids" }))
            .await;
        res.assert_status(axum::http::StatusCode::CREATED);
        let kids: Uuid = res.json::<Value>()["id"].as_str().unwrap().parse().unwrap();
        let res = api
            .post(&format!("/api/profiles/{kids}/switch"))
            .authorization_bearer(&main)
            .await;
        res.assert_status_ok();
        let kids_jwt = res.json::<Value>()["token"].as_str().unwrap().to_owned();
        let res = api
            .post("/api/api-keys")
            .authorization_bearer(&kids_jwt)
            .json(&serde_json::json!({ "deviceName": "tablet" }))
            .await;
        res.assert_status(axum::http::StatusCode::CREATED);
        let token = res.json::<Value>()["apiKey"].as_str().unwrap().to_owned();

        assert_eq!(
            star_lands_in(&env, &[("apiKey", &token)], track_id(3)).await,
            vec![kids]
        );
        assert_eq!(
            star_lands_in(&env, &refs(&ts_auth("alice", &token)), track_id(4)).await,
            vec![kids]
        );

        api.delete(&format!("/api/profiles/{kids}"))
            .authorization_bearer(&main)
            .await
            .assert_status_ok();
        for endpoint in ["ping", "tokenInfo"] {
            failed(
                &call(&env, endpoint, &[("apiKey", &token)], &[]).await,
                44,
                "Invalid API key",
            );
        }
        failed(
            &call(&env, "ping", &refs(&ts_auth("alice", &token)), &[]).await,
            40,
            HINT,
        );
        // The main profile's own token is untouched.
        ok(&call(&env, "ping", &alice_key(), &[]).await);
    }
);

/// Alice's SUBSONIC plays, per profile.
async fn plays_of(pool: &PgPool, profile: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM plays WHERE profile_id = $1 AND source = 'SUBSONIC'")
        .bind(profile)
        .fetch_one(pool)
        .await
        .unwrap()
}

fixture_test!(kids_scrobble_counts_for_kids_only, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let before = plays_of(&env.pool, ALICE_ID).await;
    ok(&call(
        &env,
        "scrobble",
        &[("apiKey", &token)],
        &[("id", &track_id(3).to_string())],
    )
    .await);
    assert_eq!(plays_of(&env.pool, kids.into_uuid()).await, 1);
    assert_eq!(plays_of(&env.pool, ALICE_ID).await, before);
});

fn song_ids(list: &Value) -> Vec<String> {
    list.as_array()
        .map(|a| {
            a.iter()
                .map(|s| s["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fixture_test!(kids_star_and_rating_invisible_to_main, |env| {
    let (_, token) = kids_of_alice(&env).await;
    let kids = [("apiKey", token.as_str())];
    let (track, album) = (track_id(3).to_string(), album_id(2).to_string());
    ok(&call(&env, "star", &kids, &[("id", &track)]).await);
    ok(&call(&env, "setRating", &kids, &[("id", &album), ("rating", "4")]).await);

    let mine = call(&env, "getStarred2", &kids, &[]).await;
    assert_eq!(song_ids(&mine["starred2"]["song"]), vec![track.clone()]);
    let main = call(&env, "getStarred2", &alice_key(), &[]).await;
    assert!(
        !song_ids(&main["starred2"]["song"]).contains(&track),
        "{main}"
    );

    let mine = call(&env, "getAlbum", &kids, &[("id", &album)]).await;
    assert_eq!(mine["album"]["userRating"], 4, "{mine}");
    let main = call(&env, "getAlbum", &alice_key(), &[("id", &album)]).await;
    assert!(main["album"].get("userRating").is_none(), "{main}");
    // Main's own stars are not the kids profile's either.
    let mine = call(&env, "getStarred2", &kids, &[]).await;
    assert!(!song_ids(&mine["starred2"]["song"]).contains(&track_id(1).to_string()));
});

fixture_test!(kids_bookmarks_queue_and_playlists_stay_in_kids, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let k = [("apiKey", token.as_str())];
    let t = track_id(4).to_string();
    ok(&call(
        &env,
        "createBookmark",
        &k,
        &[("id", &t), ("position", "1000")],
    )
    .await);
    ok(&call(
        &env,
        "savePlayQueue",
        &k,
        &[("id", &t), ("current", &t), ("position", "5")],
    )
    .await);
    ok(&call(
        &env,
        "createPlaylist",
        &k,
        &[("name", "Kids Mix"), ("songId", &t)],
    )
    .await);

    let bookmarked = |res: &Value| -> Vec<String> {
        res["bookmarks"]["bookmark"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|b| b["entry"]["id"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mine = call(&env, "getBookmarks", &k, &[]).await;
    assert_eq!(bookmarked(&mine), vec![t.clone()], "{mine}");
    let main = call(&env, "getBookmarks", &alice_key(), &[]).await;
    assert!(!bookmarked(&main).contains(&t), "{main}");

    let mine = call(&env, "getPlayQueue", &k, &[]).await;
    assert_eq!(song_ids(&mine["playQueue"]["entry"]), vec![t.clone()]);
    let main = call(&env, "getPlayQueue", &alice_key(), &[]).await;
    assert_ne!(song_ids(&main["playQueue"]["entry"]), vec![t.clone()]);
    assert!(
        playback::get(&env.pool, kids).await.unwrap().is_some(),
        "the queue is the kids profile's playback state"
    );

    let names = |res: &Value| -> Vec<String> {
        res["playlists"]["playlist"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let mine = call(&env, "getPlaylists", &k, &[]).await;
    assert!(names(&mine).contains(&"Kids Mix".to_owned()), "{mine}");
    assert!(!names(&mine).contains(&"Private Mix".to_owned()), "{mine}");
    let main = call(&env, "getPlaylists", &alice_key(), &[]).await;
    assert!(!names(&main).contains(&"Kids Mix".to_owned()), "{main}");
    let owner: Uuid =
        sqlx::query_scalar("SELECT profile_id FROM playlists WHERE name = 'Kids Mix'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(owner, kids.into_uuid());
    // Main's private playlist is not the kids profile's to read or change.
    let private = fixture_id(5, 2).to_string();
    failed(
        &call(&env, "getPlaylist", &k, &[("id", &private)]).await,
        70,
        "Playlist not found",
    );
    failed(
        &call(&env, "deletePlaylist", &k, &[("id", &private)]).await,
        50,
        "Access denied",
    );
});

fixture_test!(kids_podcast_follows_are_the_kids_profiles, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let show = |title: &'static str, feed: &'static str| NewShow {
        feed_url: feed,
        title,
        author: None,
        description: None,
        image_url: None,
        itunes_id: None,
        category_ids: &[],
        language: None,
        explicit: false,
        episode_count: 0,
        feed_etag: None,
        feed_last_modified: None,
    };
    let a = podcasts::insert_show(
        &env.pool,
        Uuid::now_v7(),
        &show("Main Show", "https://f.example/a"),
    )
    .await
    .unwrap();
    let b = podcasts::insert_show(
        &env.pool,
        Uuid::now_v7(),
        &show("Kids Show", "https://f.example/b"),
    )
    .await
    .unwrap();
    podcasts::subscribe(&env.pool, ProfileId::new(ALICE_ID), a.id)
        .await
        .unwrap();
    podcasts::subscribe(&env.pool, kids, b.id).await.unwrap();
    let titles = |res: &Value| -> Vec<String> {
        res["podcasts"]["channel"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| c["title"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mine = call(&env, "getPodcasts", &[("apiKey", &token)], &[]).await;
    assert_eq!(titles(&mine), vec!["Kids Show".to_owned()], "{mine}");
    let main = call(&env, "getPodcasts", &alice_key(), &[]).await;
    assert_eq!(titles(&main), vec!["Main Show".to_owned()], "{main}");
});

fixture_test!(kids_app_token_of_admin_is_not_admin, |env| {
    let (_, token) = profile_with_token(&env, ADMIN_ID, "Kids", ProfileKind::Kids).await;
    let (_, standard) = profile_with_token(&env, ADMIN_ID, "Guest", ProfileKind::Standard).await;
    let admin_only = "Admin privileges required";
    for t in [&token, &standard] {
        let k = [("apiKey", t.as_str())];
        ok(&call(&env, "ping", &k, &[]).await);
        failed(&call(&env, "startScan", &k, &[]).await, 50, admin_only);
        failed(&call(&env, "getUsers", &k, &[]).await, 50, admin_only);
        let ts = ts_auth("admin", t);
        failed(
            &call(&env, "startScan", &refs(&ts), &[]).await,
            50,
            admin_only,
        );
        let res = call(
            &env,
            "createUser",
            &k,
            &[
                ("username", "eve"),
                ("password", "pw-eve-123"),
                ("email", "eve@example.com"),
            ],
        )
        .await;
        assert_eq!(res["error"]["code"], 50, "{res}");
        let res = call(&env, "deleteUser", &k, &[("username", "alice")]).await;
        assert_eq!(res["error"]["code"], 50, "{res}");
    }
    // The admin's own password still is.
    ok(&call(
        &env,
        "startScan",
        &[("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)],
        &[],
    )
    .await);
    assert!(
        kima_db::users::find_by_username(&env.pool, "alice")
            .await
            .unwrap()
            .is_some()
    );
});

/// (owner, name) of every playlist the session sees, sorted.
fn owners(res: &Value) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = res["playlists"]["playlist"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_owned(),
                p["owner"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    v.sort();
    v
}

fn pair(name: &str, owner: &str) -> (String, String) {
    (name.to_owned(), owner.to_owned())
}

fixture_test!(playlist_owner_rule, |env| {
    let (_, token) = kids_of_alice(&env).await;
    let k = [("apiKey", token.as_str())];
    let created = call(&env, "createPlaylist", &k, &[("name", "Kids Mix")]).await;
    let kids_mix = created["playlist"]["id"].as_str().unwrap().to_owned();
    // Its own playlist: the caller's `u`, so a client sees it as editable.
    assert_eq!(created["playlist"]["owner"], "alice", "{created}");
    ok(&call(
        &env,
        "updatePlaylist",
        &k,
        &[("playlistId", &kids_mix), ("public", "true")],
    )
    .await);

    // Alice's main profile: own playlists `alice`, the sibling's `alice (Kids)`.
    let main = call(&env, "getPlaylists", &alice_key(), &[]).await;
    assert_eq!(
        owners(&main),
        vec![
            pair("Kids Mix", "alice (Kids)"),
            pair("Private Mix", "alice"),
            pair("Road Trip", "alice"),
        ],
        "{main}"
    );
    // The kids profile: its own `alice`, main's public one under the main profile's name, so
    // the client does not offer to edit it.
    let kids = call(&env, "getPlaylists", &k, &[]).await;
    assert_eq!(
        owners(&kids),
        vec![
            pair("Kids Mix", "alice"),
            pair("Road Trip", "alice (alice)")
        ],
        "{kids}"
    );
    let road_trip = fixture_id(5, 1).to_string();
    let one = call(&env, "getPlaylist", &k, &[("id", &road_trip)]).await;
    assert_eq!(one["playlist"]["owner"], "alice (alice)", "{one}");
    failed(
        &call(
            &env,
            "updatePlaylist",
            &k,
            &[("playlistId", &road_trip), ("name", "Mine now")],
        )
        .await,
        50,
        "Access denied",
    );
    // Another account: alice's main profile is `alice`, her kids profile `alice (Kids)`.
    let admin = call(
        &env,
        "getPlaylists",
        &[("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)],
        &[],
    )
    .await;
    assert_eq!(
        owners(&admin),
        vec![
            pair("Admin Picks", "admin"),
            pair("Kids Mix", "alice (Kids)"),
            pair("Road Trip", "alice"),
        ],
        "{admin}"
    );
    let one = call(
        &env,
        "getPlaylist",
        &[("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)],
        &[("id", &kids_mix)],
    )
    .await;
    assert_eq!(one["playlist"]["owner"], "alice (Kids)", "{one}");
});

fixture_test!(kids_playlist_change_is_announced_to_kids, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let mut events = env.state.events.subscribe();
    ok(&call(
        &env,
        "createPlaylist",
        &[("apiKey", &token)],
        &[("name", "Kids Mix")],
    )
    .await);
    let event = events.try_recv().expect("one playlist event");
    assert!(
        event.is_for(AccountId::new(ALICE_ID), kids),
        "{:?}",
        event.audience
    );
    assert!(
        !event.is_for(AccountId::new(ALICE_ID), ProfileId::new(ALICE_ID)),
        "{:?}",
        event.audience
    );
});

/// (username, player, track) of every now-playing entry, sorted.
fn playing(res: &Value) -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = res["nowPlaying"]["entry"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    (
                        e["username"].as_str().unwrap().to_owned(),
                        e["playerName"].as_str().unwrap().to_owned(),
                        e["id"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn entry(user: &str, player: &str, track: Uuid) -> (String, String, String) {
    (user.to_owned(), player.to_owned(), track.to_string())
}

fixture_test!(now_playing_per_profile, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let k = [("apiKey", token.as_str())];
    let (t3, t5, t7) = (track_id(3), track_id(5), track_id(7));
    let report = |auth: Vec<(&'static str, String)>, track: Uuid| {
        let env = &env;
        async move {
            let mut all: Vec<(&str, &str)> = auth.iter().map(|(a, b)| (*a, b.as_str())).collect();
            let id = track.to_string();
            all.extend([
                ("id", id.as_str()),
                ("submission", "false"),
                ("c", "Symfonium"),
            ]);
            ok(&rest(&env.server, "scrobble", &all).await);
        }
    };
    // The same player name on two profiles of one account: two slots.
    report(vec![("apiKey", token.clone())], t3).await;
    report(vec![("apiKey", FIXTURE_TOKEN.to_owned())], t5).await;
    // The kids profile's web player too.
    playback::upsert(
        &env.pool,
        kids,
        Patch {
            playback_type: "track".into(),
            track_id: Some(Some(t7.to_string())),
            ..Patch::default()
        },
    )
    .await
    .unwrap();

    let admin = [("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)];
    assert_eq!(
        playing(&call(&env, "getNowPlaying", &admin, &[]).await),
        vec![
            entry("alice", "Symfonium", t5),
            entry("alice (Kids)", "Kima", t7),
            entry("alice (Kids)", "Symfonium", t3),
        ]
    );
    assert_eq!(
        playing(&call(&env, "getNowPlaying", &alice_key(), &[]).await),
        vec![
            entry("alice", "Symfonium", t5),
            entry("alice (Kids)", "Kima", t7),
            entry("alice (Kids)", "Symfonium", t3),
        ]
    );
    assert_eq!(
        playing(&call(&env, "getNowPlaying", &k, &[]).await),
        vec![
            entry("alice", "Kima", t7),
            entry("alice", "Symfonium", t3),
            entry("alice (alice)", "Symfonium", t5),
        ]
    );
    // A kids submission ends the kids slot only.
    ok(&call(
        &env,
        "scrobble",
        &k,
        &[("id", &t3.to_string()), ("c", "Symfonium")],
    )
    .await);
    assert_eq!(
        playing(&call(&env, "getNowPlaying", &admin, &[]).await),
        vec![
            entry("alice", "Symfonium", t5),
            entry("alice (Kids)", "Kima", t7),
        ]
    );
});

fixture_test!(deleted_profiles_now_playing_slots_are_dropped, |env| {
    let (kids, token) = kids_of_alice(&env).await;
    let t3 = track_id(3).to_string();
    ok(&call(
        &env,
        "scrobble",
        &[("apiKey", &token)],
        &[("id", &t3), ("submission", "false"), ("c", "Symfonium")],
    )
    .await);
    assert_eq!(
        playing(&call(&env, "getNowPlaying", &alice_key(), &[]).await).len(),
        1
    );
    profiles::delete(&env.pool, AccountId::new(ALICE_ID), kids)
        .await
        .unwrap()
        .unwrap();
    assert!(playing(&call(&env, "getNowPlaying", &alice_key(), &[]).await).is_empty());
});
