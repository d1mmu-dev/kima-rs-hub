mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::PgPool;
use serde_json::{Value, json};
use uuid::Uuid;

async fn server(pool: PgPool) -> (TestServer, String, String) {
    let state = common::state(pool);
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let (_, user) = common::user_with_token(&state, "bob").await;
    (
        TestServer::new(kima_api::router(state)).unwrap(),
        admin,
        user,
    )
}

async fn guess(pool: &PgPool, track: Uuid, flags: Value) {
    sqlx::query("UPDATE tracks SET guessed = $2 WHERE id = $1")
        .bind(track)
        .bind(flags)
        .execute(pool)
        .await
        .unwrap();
}

async fn set_path(pool: &PgPool, track: Uuid, path: &str) {
    sqlx::query("UPDATE tracks SET path = $2 WHERE id = $1")
        .bind(track)
        .bind(path)
        .execute(pool)
        .await
        .unwrap();
}

fn issues_of(report: &Value, album: Uuid) -> Vec<String> {
    report
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["albumId"] == json!(album))
        .map(|a| {
            a["issues"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reports_each_issue_code(pool: PgPool) {
    // Guess flags.
    let g = seed(&pool, Seed::new("Guessy", "Guessed", Some(2000), "t")).await;
    guess(
        &pool,
        g.track,
        json!({"artistFromPath": true, "albumFromPath": true,
               "unknownArtist": true, "unknownAlbum": true}),
    )
    .await;

    // No album year.
    let noy = seed(&pool, Seed::new("Yearless", "NoYear", None, "t")).await;

    // Conflicting years: flag on one track.
    let flag = seed(&pool, Seed::new("Flagged", "Conflict", Some(2001), "t")).await;
    guess(&pool, flag.track, json!({"yearConflict": true})).await;

    // Conflicting years: distinct track years in one album.
    let a = seed(&pool, Seed::new("Years", "Mixed", Some(2001), "a")).await;
    seed(&pool, Seed::new("Years", "Mixed", Some(2001), "b").at(1, 2)).await;
    sqlx::query("UPDATE tracks SET year = 1999 WHERE id = $1")
        .bind(a.track)
        .execute(&pool)
        .await
        .unwrap();

    // Missing track numbers (0 is the unset value).
    let mn = seed(
        &pool,
        Seed::new("Numbers", "Unnumbered", Some(2002), "t").at(1, 0),
    )
    .await;

    // Duplicate numbers, same disc.
    let d = seed(&pool, Seed::new("Dupes", "Twice", Some(2003), "a").at(1, 3)).await;
    seed(&pool, Seed::new("Dupes", "Twice", Some(2003), "b").at(1, 3)).await;
    // Same number on another disc is fine.
    let ok = seed(
        &pool,
        Seed::new("Fine", "TwoDiscs", Some(2004), "a").at(1, 1),
    )
    .await;
    seed(
        &pool,
        Seed::new("Fine", "TwoDiscs", Some(2004), "b").at(2, 1),
    )
    .await;

    // Folder split: two albums with tracks in one folder.
    let s1 = seed(&pool, Seed::new("Split", "One", Some(2005), "a")).await;
    let s2 = seed(&pool, Seed::new("Split", "Two", Some(2005), "b")).await;
    set_path(&pool, s1.track, "mix/a.mp3").await;
    set_path(&pool, s2.track, "mix/b.mp3").await;

    // Missing tracks are ignored: this album's only issue sits on a missing track.
    let gone = seed(&pool, Seed::new("Gone", "Away", Some(2006), "a")).await;
    let gone_bad = seed(&pool, Seed::new("Gone", "Away", Some(2006), "b").at(1, 0)).await;
    mark_missing(&pool, gone_bad.track).await;

    let (srv, admin, _) = server(pool).await;
    let res = srv
        .get("/api/library/attention")
        .authorization_bearer(&admin)
        .await;
    res.assert_status_ok();
    let v: Value = res.json();

    assert_eq!(
        issues_of(&v, g.album),
        [
            "artistFromPath",
            "albumFromPath",
            "unknownArtist",
            "unknownAlbum"
        ]
    );
    assert_eq!(issues_of(&v, noy.album), ["missingYear"]);
    assert_eq!(issues_of(&v, flag.album), ["conflictingYears"]);
    assert_eq!(issues_of(&v, a.album), ["conflictingYears"]);
    assert_eq!(issues_of(&v, mn.album), ["missingTrackNumbers"]);
    assert_eq!(issues_of(&v, d.album), ["duplicateTrackNumbers"]);
    assert!(issues_of(&v, ok.album).is_empty());
    assert_eq!(issues_of(&v, s1.album), ["folderSplit"]);
    assert_eq!(issues_of(&v, s2.album), ["folderSplit"]);
    assert!(issues_of(&v, gone.album).is_empty());

    let entry = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["albumId"] == json!(noy.album))
        .unwrap();
    assert_eq!(entry["artist"], "Yearless");
    assert_eq!(entry["album"], "NoYear");
    assert!(entry["year"].is_null());

    // Ordered by artist name.
    let artists: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["artist"].as_str().unwrap())
        .collect();
    let mut sorted = artists.clone();
    sorted.sort();
    assert_eq!(artists, sorted);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_admin(pool: PgPool) {
    let (srv, _, user) = server(pool).await;
    let res = srv
        .get("/api/library/attention")
        .authorization_bearer(&user)
        .await;
    res.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<Value>(),
        json!({"error": "Admin access required"})
    );
    srv.get("/api/library/attention")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
