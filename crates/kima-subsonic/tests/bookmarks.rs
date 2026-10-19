//! Bookmarks on the SQL fixture (spec §4.5 #39-#41). Alice holds a bookmark on track 3 (90 s,
//! "resume here") and one on the missing track 20, which lists hide.

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

fn bookmarks(res: &Value) -> Vec<Value> {
    res["bookmarks"]["bookmark"].as_array().unwrap().clone()
}

async fn rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM subsonic_bookmarks")
        .fetch_one(pool)
        .await
        .unwrap()
}

fixture_test!(create_bookmark_upsert, |server, pool| {
    let first = call(&server, "getBookmarks", &[]).await;
    let b = &bookmarks(&first)[0];
    assert_eq!(b["position"], 90000);
    assert_eq!(b["username"], "alice");
    assert_eq!(b["comment"], "resume here");
    assert_eq!(b["created"], "2026-01-01T06:00:00.000Z");
    assert_eq!(b["changed"], "2026-01-01T06:05:00.000Z");
    assert_eq!(b["entry"]["id"], tid(3));

    // Move it: the position and comment change, the creation time stays.
    ok(&call(
        &server,
        "createBookmark",
        &[
            ("id", tid(3)),
            ("position", "5000".into()),
            ("comment", "later".into()),
        ],
    )
    .await);
    let list = bookmarks(&call(&server, "getBookmarks", &[]).await);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["position"], 5000);
    assert_eq!(list[0]["comment"], "later");
    assert_eq!(list[0]["created"], "2026-01-01T06:00:00.000Z");
    assert_ne!(list[0]["changed"], "2026-01-01T06:05:00.000Z");

    // A new one on another track, no comment, most recently changed first.
    ok(&call(
        &server,
        "createBookmark",
        &[("id", tid(4)), ("position", "0".into())],
    )
    .await);
    let list = bookmarks(&call(&server, "getBookmarks", &[]).await);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["entry"]["id"], tid(4));
    assert_eq!(list[0]["position"], 0);
    assert!(list[0].get("comment").is_none());
    assert_eq!(list[1]["entry"]["id"], tid(3));
    // Alice's own bookmarks only.
    let admin = call_admin(&server, "getBookmarks", &[]).await;
    assert!(bookmarks(&admin).is_empty());
});

fixture_test!(create_bookmark_unknown_track_70, |server, pool| {
    let res = call(
        &server,
        "createBookmark",
        &[("id", Uuid::now_v7().to_string()), ("position", "1".into())],
    )
    .await;
    failed(&res, 70);
    failed(
        &call(
            &server,
            "createBookmark",
            &[("id", "nope".into()), ("position", "1".into())],
        )
        .await,
        70,
    );
    assert_eq!(rows(&pool).await, 2);
});

fixture_test!(
    create_bookmark_position_required_nonnegative_10,
    |server, pool| {
        failed(
            &call(&server, "createBookmark", &[("id", tid(4))]).await,
            10,
        );
        failed(
            &call(
                &server,
                "createBookmark",
                &[("id", tid(4)), ("position", "-1".into())],
            )
            .await,
            10,
        );
        failed(
            &call(
                &server,
                "createBookmark",
                &[("id", tid(4)), ("position", "1.5".into())],
            )
            .await,
            10,
        );
        failed(
            &call(
                &server,
                "createBookmark",
                &[("id", tid(4)), ("position", "abc".into())],
            )
            .await,
            10,
        );
        failed(
            &call(&server, "createBookmark", &[("position", "5".into())]).await,
            10,
        );
        assert_eq!(rows(&pool).await, 2);
    }
);

fixture_test!(bookmarks_hide_missing, |server, pool| {
    // The bookmark on missing track 20 exists but is not listed.
    assert_eq!(rows(&pool).await, 2);
    let list = bookmarks(&call(&server, "getBookmarks", &[]).await);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["entry"]["id"], tid(3));
    // A track that goes missing drops out of the list but keeps its row.
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1::uuid")
        .bind(tid(3))
        .execute(&pool)
        .await
        .unwrap();
    let res = call(&server, "getBookmarks", &[]).await;
    assert!(bookmarks(&res).is_empty());
    assert_eq!(rows(&pool).await, 2);
});

fixture_test!(delete_bookmark_ok_when_none, |server, pool| {
    ok(&call(&server, "deleteBookmark", &[("id", tid(9))]).await);
    assert_eq!(rows(&pool).await, 2);
    ok(&call(&server, "deleteBookmark", &[("id", tid(3))]).await);
    assert_eq!(rows(&pool).await, 1);
    assert!(bookmarks(&call(&server, "getBookmarks", &[]).await).is_empty());
    // Gone already: still ok. An id that is no UUID is 70, a missing one 10.
    ok(&call(&server, "deleteBookmark", &[("id", tid(3))]).await);
    failed(
        &call(&server, "deleteBookmark", &[("id", "nope".into())]).await,
        70,
    );
    failed(&call(&server, "deleteBookmark", &[]).await, 10);
    // Another user's delete leaves alice's bookmark alone.
    ok(&call_admin(&server, "deleteBookmark", &[("id", tid(20))]).await);
    assert_eq!(rows(&pool).await, 1);
});
