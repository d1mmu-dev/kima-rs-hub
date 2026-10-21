//! Stars and ratings on the SQL fixture (spec §4.7 #47-#51). Alice starred tracks 1 and 20
//! (missing), albums 3, 1 and 6 (no present track) and artist 1; she rated track 1 = 5, album 1 = 4,
//! album 4 = 5 and artist 2 = 3. The admin starred track 2.

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use serde_json::Value;
use uuid::Uuid;

fn app(pool: PgPool) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool)).layer(MockConnectInfo(peer));
    TestServer::new(router).unwrap()
}

/// A call as alice (her app token). `params` may repeat keys.
async fn call(server: &TestServer, endpoint: &str, params: &[(&str, String)]) -> Value {
    let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
}

/// A call as the admin, with the password.
async fn call_admin(server: &TestServer, endpoint: &str, params: &[(&str, String)]) -> Value {
    let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let res = server
        .get(&format!("/{endpoint}.view"))
        .add_query_params([("f", "json"), ("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)])
        .add_query_params(params)
        .await;
    res.assert_status_ok();
    res.json::<Value>()["subsonic-response"].clone()
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

fn failed(res: &Value, code: u64) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
}

fn ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

fn tid(n: u128) -> String {
    track_id(n).to_string()
}
fn alid(n: u128) -> String {
    album_id(n).to_string()
}
fn arid(n: u128) -> String {
    artist_id(n).to_string()
}

/// The ids of one list of `getStarred2`, in order.
fn ids(res: &Value, list: &str) -> Vec<String> {
    res["starred2"][list]
        .as_array()
        .unwrap_or_else(|| panic!("no {list} list: {res}"))
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

fn has_star(item: &Value) -> bool {
    item["starred"].is_string()
}

fixture_test!(
    star_song_album_artist_each_shows_starred_everywhere,
    |server, pool| {
        // Track 4 is on album 2; neither that album nor its artist carries a star yet.
        let before = call(&server, "getAlbum", &[("id", alid(2))]).await;
        assert!(!has_star(&before["album"]));
        let res = call(
            &server,
            "star",
            &[("id", tid(4)), ("albumId", alid(2)), ("artistId", arid(2))],
        )
        .await;
        ok(&res);

        let album = call(&server, "getAlbum", &[("id", alid(2))]).await;
        assert!(has_star(&album["album"]), "{album}");
        let album1 = call(&server, "getAlbum", &[("id", alid(1))]).await;
        let song = album1["album"]["song"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == tid(4))
            .unwrap();
        assert!(has_star(song), "{album1}");
        let artist = call(&server, "getArtist", &[("id", arid(2))]).await;
        assert!(has_star(&artist["artist"]), "{artist}");

        let found = call(
            &server,
            "search3",
            &[("query", String::new()), ("songCount", "500".into())],
        )
        .await;
        let r = &found["searchResult3"];
        let by_id = |list: &str, id: String| -> Value {
            r[list]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["id"] == id)
                .unwrap_or_else(|| panic!("{list} {id} not in {r}"))
                .clone()
        };
        assert!(has_star(&by_id("song", tid(4))));
        assert!(has_star(&by_id("album", alid(2))));
        assert!(has_star(&by_id("artist", arid(2))));

        let starred = call(&server, "getStarred2", &[]).await;
        assert!(ids(&starred, "song").contains(&tid(4)));
        assert!(ids(&starred, "album").contains(&alid(2)));
        assert!(ids(&starred, "artist").contains(&arid(2)));
        assert!(starred["starred2"]["song"][0]["starred"].is_string());
    }
);

fixture_test!(star_by_plain_id_of_album_and_artist, |server, pool| {
    // `id` may name an album or an artist; the kind is looked up.
    ok(&call(&server, "star", &[("id", alid(2)), ("id", arid(3))]).await);
    let starred = call(&server, "getStarred2", &[]).await;
    assert!(ids(&starred, "album").contains(&alid(2)));
    assert!(ids(&starred, "artist").contains(&arid(3)));
});

fixture_test!(
    star_unknown_ids_skipped_ok_when_one_valid,
    |server, pool| {
        let res = call(
            &server,
            "star",
            &[
                ("id", Uuid::now_v7().to_string()),
                ("id", "not-an-id".into()),
                ("id", tid(5)),
                // An `albumId` that is really a track id: skipped too.
                ("albumId", tid(6)),
            ],
        )
        .await;
        ok(&res);
        let starred = call(&server, "getStarred2", &[]).await;
        assert!(ids(&starred, "song").contains(&tid(5)));
        assert!(!ids(&starred, "song").contains(&tid(6)));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM stars")
            .fetch_one(&pool)
            .await
            .unwrap();
        // The 7 fixture stars plus track 5.
        assert_eq!(count, 8);
    }
);

fixture_test!(star_all_unknown_70, |server, pool| {
    failed(
        &call(&server, "star", &[("id", Uuid::now_v7().to_string())]).await,
        70,
    );
    failed(&call(&server, "star", &[("id", "nope".into())]).await, 70);
    // An albumId that is a track, an artistId that is an album: neither exists as that kind.
    failed(&call(&server, "star", &[("albumId", tid(1))]).await, 70);
    failed(&call(&server, "star", &[("artistId", alid(1))]).await, 70);
});

fixture_test!(star_none_sent_ok, |server, pool| {
    ok(&call(&server, "star", &[]).await);
    ok(&call(&server, "star", &[("id", String::new())]).await);
});

fixture_test!(unstar_always_ok, |server, pool| {
    ok(&call(&server, "unstar", &[]).await);
    ok(&call(
        &server,
        "unstar",
        &[("id", Uuid::now_v7().to_string()), ("id", "x".into())],
    )
    .await);
    // Not starred at all.
    ok(&call(&server, "unstar", &[("id", tid(9)), ("albumId", alid(2))]).await);
    // Starred: track 1, album 1 and artist 1 go.
    ok(&call(
        &server,
        "unstar",
        &[("id", tid(1)), ("albumId", alid(1)), ("artistId", arid(1))],
    )
    .await);
    let starred = call(&server, "getStarred2", &[]).await;
    assert!(ids(&starred, "song").is_empty());
    assert_eq!(ids(&starred, "album"), vec![alid(3)]);
    assert!(ids(&starred, "artist").is_empty());
    // The admin's star is untouched.
    let admin = call_admin(&server, "getStarred2", &[]).await;
    assert_eq!(ids(&admin, "song"), vec![tid(2)]);
});

fixture_test!(starred_lists_newest_first, |server, pool| {
    let starred = call(&server, "getStarred2", &[]).await;
    // Missing track 20 and the album with no present track (6) are hidden.
    assert_eq!(ids(&starred, "song"), vec![tid(1)]);
    assert_eq!(ids(&starred, "album"), vec![alid(1), alid(3)]);
    assert_eq!(ids(&starred, "artist"), vec![arid(1)]);
    ok(&call(&server, "star", &[("id", tid(4))]).await);
    ok(&call(&server, "star", &[("albumId", alid(2))]).await);
    ok(&call(&server, "star", &[("id", tid(3))]).await);
    let starred = call(&server, "getStarred2", &[]).await;
    assert_eq!(ids(&starred, "song"), vec![tid(3), tid(4), tid(1)]);
    assert_eq!(ids(&starred, "album"), vec![alid(2), alid(1), alid(3)]);
    // Starring twice keeps the first star's place.
    ok(&call(&server, "star", &[("id", tid(1))]).await);
    let starred = call(&server, "getStarred2", &[]).await;
    assert_eq!(ids(&starred, "song"), vec![tid(3), tid(4), tid(1)]);

    // `getStarred`: albums in the Child shape.
    let old = call(&server, "getStarred", &[]).await;
    let albums = old["starred"]["album"].as_array().unwrap();
    assert_eq!(albums[0]["id"], alid(2));
    assert_eq!(albums[0]["isDir"], true);
    assert_eq!(albums[0]["title"], "White Album");
    assert!(has_star(&albums[0]));
    assert_eq!(old["starred"]["song"][0]["id"], tid(3));
    assert_eq!(old["starred"]["artist"][0]["id"], arid(1));
});

fixture_test!(starred_hidden_for_other_user, |server, pool| {
    let admin = call_admin(&server, "getStarred2", &[]).await;
    assert_eq!(ids(&admin, "song"), vec![tid(2)]);
    assert!(ids(&admin, "album").is_empty());
    assert!(ids(&admin, "artist").is_empty());
    // Alice's heart on album 1 is not the admin's.
    let album = call_admin(&server, "getAlbum", &[("id", alid(1))]).await;
    assert!(!has_star(&album["album"]));
    // Track 2 is starred for the admin only.
    let song = call(&server, "getSong", &[("id", tid(2))]).await;
    assert!(!has_star(&song["song"]));
    let song = call_admin(&server, "getSong", &[("id", tid(2))]).await;
    assert!(has_star(&song["song"]));
    // Empty lists are arrays, in XML nothing.
    let none = call_admin(&server, "getStarred", &[]).await;
    assert_eq!(none["starred"]["album"], serde_json::json!([]));
});

fixture_test!(set_rating_stores_and_shows_user_rating, |server, pool| {
    ok(&call(
        &server,
        "setRating",
        &[("id", tid(4)), ("rating", "3".into())],
    )
    .await);
    ok(&call(
        &server,
        "setRating",
        &[("id", alid(2)), ("rating", "4".into())],
    )
    .await);
    ok(&call(
        &server,
        "setRating",
        &[("id", arid(1)), ("rating", "2".into())],
    )
    .await);
    let song = call(&server, "getSong", &[("id", tid(4))]).await;
    assert_eq!(song["song"]["userRating"], 3);
    let album = call(&server, "getAlbum", &[("id", alid(2))]).await;
    assert_eq!(album["album"]["userRating"], 4);
    let artist = call(&server, "getArtist", &[("id", arid(1))]).await;
    assert_eq!(artist["artist"]["userRating"], 2);
    // Setting again replaces.
    ok(&call(
        &server,
        "setRating",
        &[("id", tid(4)), ("rating", "5".into())],
    )
    .await);
    let song = call(&server, "getSong", &[("id", tid(4))]).await;
    assert_eq!(song["song"]["userRating"], 5);
    // The admin does not see alice's rating.
    let song = call_admin(&server, "getSong", &[("id", tid(4))]).await;
    assert!(song["song"].get("userRating").is_none());
});

fixture_test!(set_rating_zero_clears, |server, pool| {
    ok(&call(
        &server,
        "setRating",
        &[("id", tid(1)), ("rating", "0".into())],
    )
    .await);
    ok(&call(
        &server,
        "setRating",
        &[("id", alid(1)), ("rating", "0".into())],
    )
    .await);
    let song = call(&server, "getSong", &[("id", tid(1))]).await;
    assert!(song["song"].get("userRating").is_none(), "{song}");
    let album = call(&server, "getAlbum", &[("id", alid(1))]).await;
    assert!(album["album"].get("userRating").is_none(), "{album}");
    // Clearing what was never rated is fine; the admin's rating of track 1 stays.
    ok(&call(
        &server,
        "setRating",
        &[("id", tid(9)), ("rating", "0".into())],
    )
    .await);
    let song = call_admin(&server, "getSong", &[("id", tid(1))]).await;
    assert_eq!(song["song"]["userRating"], 1);
});

fixture_test!(set_rating_out_of_range_0, |server, pool| {
    for bad in ["6", "-1", "100"] {
        let res = call(
            &server,
            "setRating",
            &[("id", tid(1)), ("rating", bad.into())],
        )
        .await;
        failed(&res, 0);
        assert_eq!(res["error"]["message"], "Rating must be between 0 and 5");
    }
    // Nothing changed.
    let song = call(&server, "getSong", &[("id", tid(1))]).await;
    assert_eq!(song["song"]["userRating"], 5);
    // Missing or non-numeric parameters are 10.
    failed(&call(&server, "setRating", &[("id", tid(1))]).await, 10);
    failed(
        &call(&server, "setRating", &[("rating", "3".into())]).await,
        10,
    );
    failed(
        &call(
            &server,
            "setRating",
            &[("id", tid(1)), ("rating", "x".into())],
        )
        .await,
        10,
    );
});

fixture_test!(set_rating_unknown_70, |server, pool| {
    failed(
        &call(
            &server,
            "setRating",
            &[("id", Uuid::now_v7().to_string()), ("rating", "3".into())],
        )
        .await,
        70,
    );
    failed(
        &call(
            &server,
            "setRating",
            &[("id", "nope".into()), ("rating", "3".into())],
        )
        .await,
        70,
    );
    failed(
        &call(
            &server,
            "setRating",
            &[("id", Uuid::now_v7().to_string()), ("rating", "0".into())],
        )
        .await,
        70,
    );
});

fixture_test!(rating_does_not_star, |server, pool| {
    ok(&call(
        &server,
        "setRating",
        &[("id", tid(4)), ("rating", "5".into())],
    )
    .await);
    let song = call(&server, "getSong", &[("id", tid(4))]).await;
    assert!(!has_star(&song["song"]));
    let starred = call(&server, "getStarred2", &[]).await;
    assert!(!ids(&starred, "song").contains(&tid(4)));
    // And starring does not rate.
    ok(&call(&server, "star", &[("id", tid(5))]).await);
    let song = call(&server, "getSong", &[("id", tid(5))]).await;
    assert!(has_star(&song["song"]));
    assert!(song["song"].get("userRating").is_none());
    // Unstarring keeps the rating.
    ok(&call(&server, "unstar", &[("id", tid(1))]).await);
    let song = call(&server, "getSong", &[("id", tid(1))]).await;
    assert_eq!(song["song"]["userRating"], 5);
});

fixture_test!(star_many_ids_one_kind_query, |server, pool| {
    let me = ALICE_ID;
    let ids = [track_id(2), album_id(2), artist_id(2)];
    let params: Vec<(&str, String)> = ids.iter().map(|i| ("id", i.to_string())).collect();
    assert_eq!(call(&server, "star", &params).await["status"], "ok");
    for (col, id) in [
        ("track_id", ids[0]),
        ("album_id", ids[1]),
        ("artist_id", ids[2]),
    ] {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM stars WHERE user_id = $1 AND {col} = $2"
        ))
        .bind(me)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "{col}");
    }
});
