//! Route coverage (spec §4, §10.2): all 81 endpoint names answer, the stubs say what their rows
//! say, and a broken database never leaks a detail (the port of upstream's `compatAlwaysAnswers`).

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use kima_subsonic::routes::ENDPOINTS;
use serde_json::{Value, json};
use uuid::Uuid;

/// (endpoint, params, the element it answers, or its error code and message).
type StubCase<'a> = (
    &'a str,
    Vec<(&'a str, &'a str)>,
    Result<String, (u64, &'a str)>,
);

fn app(pool: PgPool) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool)).layer(MockConnectInfo(peer));
    TestServer::new(router).unwrap()
}

/// The paths an endpoint answers on: with and without `.view`. `hls.m3u8` has its own name.
fn paths(name: &str) -> Vec<String> {
    if name.ends_with(".m3u8") {
        vec![format!("/{name}")]
    } else {
        vec![format!("/{name}"), format!("/{name}.view")]
    }
}

macro_rules! fixture_test {
    ($name:ident, |$server:ident, $pool:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $name($pool: PgPool) {
            let $server = app($pool.clone());
            $body
        }
    };
}

/// A call as alice (her app token) on the `.view` path.
async fn call(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> Value {
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
}

fn failed(res: &Value, code: u64, message: &str) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
    assert_eq!(res["error"]["message"], message, "{res}");
}

#[test]
fn there_are_81_distinct_names() {
    assert_eq!(ENDPOINTS.len(), 81);
    let mut sorted = ENDPOINTS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 81);
}

fixture_test!(all_81_paths_answer, |server, pool| {
    assert_eq!(ENDPOINTS.len(), 81);
    for name in ENDPOINTS {
        for path in paths(name) {
            let res = server
                .get(&path)
                .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
                .await;
            res.assert_status_ok();
            let body = res.text();
            // Audio, images and playlists carry no envelope; an error always does.
            if let Ok(json) = serde_json::from_str::<Value>(&body) {
                let message = json["subsonic-response"]["error"]["message"]
                    .as_str()
                    .unwrap_or("");
                assert!(
                    !message.starts_with("Not implemented"),
                    "{path} is not routed: {body}"
                );
            }
        }
    }
    // Anything else is still "not implemented".
    let res = call(&server, "foo", &[]).await;
    failed(&res, 0, "Not implemented: /foo.view");
});

fixture_test!(stub_answers, |server, pool| {
    let uuid = artist_id(1).to_string();
    let ar = format!("ar-{uuid}");
    let unknown = Uuid::now_v7().to_string();

    // (endpoint, params, expected element or (code, message))
    let empty = |name: &str| Ok(name.to_owned());
    let cases: Vec<StubCase> = vec![
        // Real since M3 (spec §18 rows 58-63); `tests/info.rs` covers them in full.
        (
            "getAlbumInfo",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        (
            "getAlbumInfo2",
            vec![("id", &uuid)],
            Err((70, "Album not found")),
        ),
        ("getArtistInfo", vec![("id", &uuid)], empty("artistInfo")),
        ("getArtistInfo", vec![("id", &ar)], empty("artistInfo")),
        ("getArtistInfo2", vec![("id", &uuid)], empty("artistInfo2")),
        (
            "getArtistInfo",
            vec![("id", &unknown)],
            Err((70, "Artist not found")),
        ),
        (
            "getArtistInfo2",
            vec![("id", "nope")],
            Err((70, "Artist not found")),
        ),
        (
            "getArtistInfo",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        (
            "getArtistInfo2",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        ("getLyrics", vec![], empty("lyrics")),
        (
            "getLyricsBySongId",
            vec![("id", &uuid)],
            empty("lyricsList"),
        ),
        (
            "getLyricsBySongId",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        ("getPodcasts", vec![], empty("podcasts")),
        ("getNewestPodcasts", vec![], empty("newestPodcasts")),
        (
            "getPodcastEpisode",
            vec![("id", "1")],
            Err((70, "Podcast episode not found")),
        ),
        ("refreshPodcasts", vec![], Ok(String::new())),
        ("getShares", vec![], empty("shares")),
        (
            "createShare",
            vec![("id", &uuid)],
            Err((0, "Sharing is not supported by this server yet")),
        ),
        (
            "updateShare",
            vec![("id", "1")],
            Err((0, "Sharing is not supported by this server yet")),
        ),
        (
            "deleteShare",
            vec![("id", "1")],
            Err((0, "Sharing is not supported by this server yet")),
        ),
        (
            "getInternetRadioStations",
            vec![],
            empty("internetRadioStations"),
        ),
        (
            "createInternetRadioStation",
            vec![],
            Err((0, "Internet radio is not supported by this server")),
        ),
        (
            "updateInternetRadioStation",
            vec![],
            Err((0, "Internet radio is not supported by this server")),
        ),
        (
            "deleteInternetRadioStation",
            vec![],
            Err((0, "Internet radio is not supported by this server")),
        ),
        ("getChatMessages", vec![], empty("chatMessages")),
        (
            "addChatMessage",
            vec![("message", "hi")],
            Err((0, "Chat is not supported by this server")),
        ),
        ("getVideos", vec![], empty("videos")),
        ("getVideoInfo", vec![("id", "1")], empty("videoInfo")),
        (
            "getVideoInfo",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        (
            "getCaptions",
            vec![("id", "1")],
            Err((70, "No captions available")),
        ),
        (
            "getCaptions",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        (
            "jukeboxControl",
            vec![("action", "get")],
            Err((0, "Jukebox mode is not supported by this server")),
        ),
        (
            "jukeboxControl",
            vec![],
            Err((10, "Required parameter is missing: action")),
        ),
        // Real endpoints that answer empty on a library with no genres.
        ("getGenres", vec![], empty("genres")),
        (
            "getSongsByGenre",
            vec![("genre", "Rock")],
            empty("songsByGenre"),
        ),
        (
            "getSongsByGenre",
            vec![],
            Err((10, "Required parameter is missing: genre")),
        ),
        (
            "getSimilarSongs",
            vec![("id", &uuid)],
            empty("similarSongs"),
        ),
        (
            "getSimilarSongs",
            vec![],
            Err((10, "Required parameter is missing: id")),
        ),
        (
            "getSimilarSongs2",
            vec![("id", &uuid)],
            empty("similarSongs2"),
        ),
    ];
    for (endpoint, params, expected) in cases {
        let res = call(&server, endpoint, &params).await;
        match expected {
            Ok(element) if element.is_empty() => {
                assert_eq!(res["status"], "ok", "{endpoint}: {res}");
            }
            Ok(element) => {
                assert_eq!(res["status"], "ok", "{endpoint}: {res}");
                assert!(res.get(&element).is_some(), "{endpoint}: {res}");
            }
            Err((code, message)) => {
                assert_eq!(res["error"]["code"], code, "{endpoint}: {res}");
                failed(&res, code, message);
            }
        }
    }
});

fixture_test!(create_share_not_supported, |server, pool| {
    let id = track_id(1).to_string();
    for path in ["/createShare", "/createShare.view"] {
        let res = server
            .get(path)
            .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN), ("id", &id)])
            .await;
        res.assert_status_ok();
        let body = res.json::<Value>();
        failed(
            &body["subsonic-response"],
            0,
            "Sharing is not supported by this server yet",
        );
    }
    // Over POST as well.
    let res = server
        .post("/createShare.view")
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
        .form(&json!({"id": id}))
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["subsonic-response"]["error"]["code"], 0);
});

/// Endpoints whose queries touch only `users` and `api_keys`, or no table at all, so they still
/// answer when every other table is gone. Each outcome is pinned: `None` is ok, `Some(code)` an
/// error. The four user writes answer 50 to an app token before they read anything (D-098);
/// the password-auth probe below covers them for real.
const UNAFFECTED: &[(&str, Option<u64>)] = &[
    ("ping", None),
    ("getLicense", None),
    ("getMusicFolders", None),
    ("getUser", None),
    ("getUsers", None),
    ("getOpenSubsonicExtensions", None),
    ("tokenInfo", None),
    ("createUser", Some(50)),
    ("updateUser", Some(50)),
    ("deleteUser", Some(50)),
    ("changePassword", Some(50)),
    ("getPodcasts", None),
    ("getNewestPodcasts", None),
    ("getPodcastEpisode", Some(70)),
    ("refreshPodcasts", None),
    ("getShares", None),
    ("createShare", Some(0)),
    ("updateShare", Some(0)),
    ("deleteShare", Some(0)),
    ("getInternetRadioStations", None),
    ("createInternetRadioStation", Some(0)),
    ("updateInternetRadioStation", Some(0)),
    ("deleteInternetRadioStation", Some(0)),
    ("getChatMessages", None),
    ("addChatMessage", Some(0)),
    ("getVideos", None),
    ("getVideoInfo", None),
    ("getCaptions", Some(70)),
    ("jukeboxControl", Some(0)),
    // The shared probe parameters carry no `genre`.
    ("getSongsByGenre", Some(10)),
    ("getSimilarSongs", None),
    ("getSimilarSongs2", None),
];

/// Unaffected endpoints that answer a playlist or an image, not an envelope. `hls` builds a URL
/// from its parameters and reads no table, as upstream; `getAvatar` reads only `users`.
const NO_ENVELOPE: &[&str] = &["hls", "hls.m3u8", "getAvatar"];

fixture_test!(
    every_endpoint_answers_internal_error_when_db_breaks,
    |server, pool| {
        // Auth needs `users` and `api_keys`, so break everything else instead of closing the pool.
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT tablename FROM pg_tables WHERE schemaname = current_schema()")
                .fetch_all(&pool)
                .await
                .unwrap();
        for (table,) in &tables {
            if ["users", "api_keys", "_sqlx_migrations"].contains(&table.as_str()) {
                continue;
            }
            sqlx::query(&format!(
                "ALTER TABLE \"{table}\" RENAME TO \"{table}_gone\""
            ))
            .execute(&pool)
            .await
            .unwrap();
        }

        // `startScan` needs an admin, so call everything as the admin with an app token.
        let admin = kima_db::users::find_by_username(&pool, ADMIN_NAME)
            .await
            .unwrap()
            .unwrap();
        let (_, token) = app_token(&fixture_state(pool.clone()), &admin, "probe").await;

        let id = track_id(1).to_string();
        let params = [
            ("id", id.as_str()),
            ("current", id.as_str()),
            ("currentIndex", "0"),
            ("songId", id.as_str()),
            ("albumId", id.as_str()),
            ("artistId", id.as_str()),
            ("playlistId", id.as_str()),
            ("songIdToAdd", id.as_str()),
            ("name", "x"),
            ("query", "x"),
            ("artist", "x"),
            ("username", "admin"),
            ("position", "0"),
            ("rating", "3"),
            ("type", "newest"),
            ("fromYear", "2000"),
            ("toYear", "2001"),
            ("mediaType", "song"),
            ("transcodeParams", "x"),
            ("mediaId", id.as_str()),
            ("action", "get"),
            ("size", "10"),
        ];
        for name in ENDPOINTS {
            let path = paths(name).pop().unwrap();
            let res = server
                .get(&path)
                .add_query_params([("f", "json"), ("apiKey", token.as_str())])
                .add_query_params(params)
                .await;
            res.assert_status_ok();
            let body = res.text();
            for leak in ["_gone", "relation", "sqlx", "SELECT", "pool"] {
                assert!(!body.contains(leak), "{name} leaks {leak}: {body}");
            }
            if NO_ENVELOPE.contains(name) {
                assert!(
                    serde_json::from_str::<Value>(&body).is_err(),
                    "{name}: {body}"
                );
                continue;
            }
            let json: Value = serde_json::from_str(&body)
                .unwrap_or_else(|_| panic!("{name} answered no envelope: {body}"));
            let res = &json["subsonic-response"];
            match UNAFFECTED.iter().find(|(n, _)| n == name) {
                Some((_, None)) => assert_eq!(res["status"], "ok", "{name}: {body}"),
                Some((_, Some(code))) => {
                    assert_eq!(res["status"], "failed", "{name}: {body}");
                    assert_eq!(res["error"]["code"], *code, "{name}: {body}");
                }
                None => failed(res, 0, "Internal error"),
            }
        }

        // The user writes under password auth, which is how they work for real. Password auth,
        // the user lookups and the password and token updates touch only `users` and `api_keys`
        // (session revocation is in memory), so all four still succeed with the other tables
        // gone. The outcomes are pinned, and `admin_call` checks that no body leaks a detail.
        let admin_call = |endpoint: &'static str, extra: Vec<(&'static str, String)>| {
            let server = &server;
            async move {
                let res = server
                    .get(&format!("/{endpoint}.view"))
                    .add_query_params([("f", "json"), ("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)])
                    .add_query_params(extra)
                    .await;
                res.assert_status_ok();
                let body = res.text();
                for leak in ["_gone", "relation", "sqlx", "SELECT", "pool"] {
                    assert!(!body.contains(leak), "{endpoint} leaks {leak}: {body}");
                }
                serde_json::from_str::<Value>(&body).unwrap()["subsonic-response"].clone()
            }
        };
        let res = admin_call(
            "changePassword",
            vec![
                ("username", ALICE_NAME.to_owned()),
                ("password", "newpass123".to_owned()),
            ],
        )
        .await;
        assert_eq!(res["status"], "ok", "changePassword: {res}");
        let res = admin_call(
            "updateUser",
            vec![
                ("username", ALICE_NAME.to_owned()),
                ("password", "newpass123".to_owned()),
            ],
        )
        .await;
        assert_eq!(res["status"], "ok", "updateUser: {res}");
        let res = admin_call(
            "createUser",
            vec![
                ("username", "newbie".to_owned()),
                ("password", "newpass123".to_owned()),
            ],
        )
        .await;
        assert_eq!(res["status"], "ok", "{res}");
        let res = admin_call("deleteUser", vec![("username", "newbie".to_owned())]).await;
        assert_eq!(res["status"], "ok", "{res}");

        // With the pool closed, even auth fails the same way.
        pool.close().await;
        let res = call(&server, "tokenInfo", &[]).await;
        failed(&res, 0, "Internal error");
    }
);
