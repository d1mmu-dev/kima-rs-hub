//! `GET /api/recommendations/for-you` (M5b Task 18; upstream `routes/recommendations.ts:12-157` at the M5b reference
//! `9450c94b`): the artists most played in the profile's last 50 plays seed the recommendations; each seed's 10
//! strongest similar artists, deduplicated, without the artists the library owns, the first `limit` of them with their
//! album count and cover. kima keeps similar artists as the stored `artists."similar"` list (D-113) and has no album
//! ownership before M7: "owned" = an artist with a present library track (D-271). Real router, real database.

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use common::mix_keys::assert_in_order;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct Env {
    server: TestServer,
    pool: PgPool,
    ana: String,
    ana_profile: ProfileId,
    kid: String,
    bob: String,
    bob_profile: ProfileId,
    /// Library artists by name, with one of their present tracks.
    artists: HashMap<String, (Uuid, Uuid)>,
}

async fn env(pool: PgPool) -> Env {
    let state = common::state(pool.clone());
    let (ana_user, ana) = common::user_with_token(&state, "ana").await;
    let (bob_user, bob) = common::user_with_token(&state, "bob").await;
    let kid_profile = profiles::create(
        &pool,
        ana_user.account(),
        NewProfile {
            name: "Kid".into(),
            kind: ProfileKind::Kids,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
    .id;
    let kid = state.jwt.issue_access(
        ana_user.account(),
        kid_profile,
        &ana_user.username,
        &ana_user.role,
        ana_user.token_version,
    );
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        pool,
        ana,
        ana_profile: ana_user.main_profile(),
        kid,
        bob,
        bob_profile: bob_user.main_profile(),
        artists: HashMap::new(),
    }
}

impl Env {
    /// A library artist `name` with one present track; returns its id.
    async fn artist(&mut self, name: &str) -> Uuid {
        let s = seed(
            &self.pool,
            Seed::new(name, &format!("{name} album"), None, "song"),
        )
        .await;
        self.artists.insert(name.to_string(), (s.artist, s.track));
        s.artist
    }

    fn track_of(&self, name: &str) -> Uuid {
        self.artists[name].1
    }

    /// Stores `similar` (name, mbid, match) as the artist's similar artists.
    async fn similar(&self, artist: Uuid, similar: &[(&str, Option<&str>, f64)]) {
        let list: Vec<Value> = similar
            .iter()
            .map(|(name, mbid, m)| json!({ "name": name, "mbid": mbid, "match": m }))
            .collect();
        sqlx::query("UPDATE artists SET \"similar\" = $2 WHERE id = $1")
            .bind(artist)
            .bind(Value::Array(list))
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// `n` plays of `track` by `profile`, the newest `minutes_ago` minutes ago, one a second before.
    async fn plays(&self, profile: ProfileId, track: Uuid, n: usize, minutes_ago: i64) {
        let newest = OffsetDateTime::now_utc() - Duration::minutes(minutes_ago);
        for i in 0..n {
            sqlx::query(
                "INSERT INTO plays (id, profile_id, track_id, played_at) VALUES ($1, $2, $3, $4)",
            )
            .bind(Uuid::now_v7())
            .bind(profile)
            .bind(track)
            .bind(newest - Duration::seconds(i as i64))
            .execute(&self.pool)
            .await
            .unwrap();
        }
    }

    async fn for_you(&self, token: &str, query: &str) -> (Vec<Value>, String) {
        let res = self
            .server
            .get(&format!("/api/recommendations/for-you{query}"))
            .authorization_bearer(token)
            .await;
        res.assert_status_ok();
        let text = res.text();
        let body: Value = serde_json::from_str(&text).unwrap();
        (body["artists"].as_array().unwrap().clone(), text)
    }

    async fn names(&self, token: &str, query: &str) -> Vec<String> {
        self.for_you(token, query)
            .await
            .0
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect()
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_plays_empty_artists(pool: PgPool) {
    let mut e = env(pool).await;
    let a = e.artist("Alpha").await;
    e.similar(a, &[("Stranger", None, 0.9)]).await;
    let res = e
        .server
        .get("/api/recommendations/for-you")
        .authorization_bearer(&e.ana)
        .await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>(), json!({ "artists": [] }));
    assert!(e.for_you(&e.ana, "?limit=abc").await.0.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn top3_seed_artists_from_last_50_plays(pool: PgPool) {
    let mut e = env(pool).await;
    for (name, sim) in [("A", "simA"), ("B", "simB"), ("C", "simC"), ("D", "simD")] {
        let id = e.artist(name).await;
        e.similar(id, &[(sim, None, 0.5)]).await;
    }
    // The last 50 plays: A 20, B 15, C 10, D 5; 30 older plays of D do not count.
    e.plays(e.ana_profile, e.track_of("A"), 20, 1).await;
    e.plays(e.ana_profile, e.track_of("B"), 15, 2).await;
    e.plays(e.ana_profile, e.track_of("C"), 10, 3).await;
    e.plays(e.ana_profile, e.track_of("D"), 5, 4).await;
    e.plays(e.ana_profile, e.track_of("D"), 30, 600).await;
    assert_eq!(e.names(&e.ana, "").await, ["simA", "simB", "simC"]);
    // A tie keeps the order the artists were first met in, newest play first.
    sqlx::query("DELETE FROM plays")
        .execute(&e.pool)
        .await
        .unwrap();
    e.plays(e.ana_profile, e.track_of("C"), 2, 1).await;
    e.plays(e.ana_profile, e.track_of("A"), 2, 2).await;
    e.plays(e.ana_profile, e.track_of("D"), 2, 3).await;
    e.plays(e.ana_profile, e.track_of("B"), 2, 4).await;
    assert_eq!(e.names(&e.ana, "").await, ["simC", "simA", "simD"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ten_similar_each_by_match_deduped(pool: PgPool) {
    let mut e = env(pool).await;
    let a = e.artist("A").await;
    let b = e.artist("B").await;
    // Twelve entries stored out of order: the ten strongest count.
    let list: Vec<(String, f64)> = (1..=12)
        .map(|i| (format!("a{i:02}"), f64::from(i) / 100.0))
        .collect();
    let mut shuffled: Vec<(&str, Option<&str>, f64)> =
        list.iter().map(|(n, m)| (n.as_str(), None, *m)).collect();
    shuffled.reverse();
    shuffled.swap(0, 7);
    e.similar(a, &shuffled).await;
    // B shares one name with A: it comes once, where A put it.
    e.similar(b, &[("b1", None, 0.9), ("a10", None, 0.8)]).await;
    e.plays(e.ana_profile, e.track_of("A"), 3, 1).await;
    e.plays(e.ana_profile, e.track_of("B"), 2, 2).await;
    let names = e.names(&e.ana, "?limit=50").await;
    let expected: Vec<String> = (3..=12)
        .rev()
        .map(|i| format!("a{i:02}"))
        .chain(["b1".to_string()])
        .collect();
    assert_eq!(names, expected);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_artists_filtered_out(pool: PgPool) {
    let mut e = env(pool).await;
    let seed_artist = e.artist("Seed").await;
    e.artist("Owned One").await;
    // An artist whose only song went missing has no present track: not owned (until M7).
    let gone = e.artist("Gone").await;
    mark_missing(&e.pool, e.track_of("Gone")).await;
    e.similar(
        seed_artist,
        &[
            ("Owned One", None, 0.9),
            ("Gone", None, 0.8),
            ("Stranger", None, 0.7),
        ],
    )
    .await;
    e.plays(e.ana_profile, e.track_of("Seed"), 1, 1).await;
    let (artists, _) = e.for_you(&e.ana, "").await;
    let ids: Vec<&str> = artists.iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [gone.to_string().as_str(), "Stranger"]);
    // Matched by MBID before the name.
    let mbid = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    sqlx::query("UPDATE artists SET mbid = $2 WHERE id = $1")
        .bind(gone)
        .bind(mbid.parse::<Uuid>().unwrap())
        .execute(&e.pool)
        .await
        .unwrap();
    e.similar(seed_artist, &[("Another Spelling", Some(mbid), 0.9)])
        .await;
    let (artists, _) = e.for_you(&e.ana, "").await;
    assert_eq!(artists[0]["id"], gone.to_string());
    assert_eq!(artists[0]["name"], "Gone");
    assert_eq!(artists[0]["mbid"], mbid);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_count_by_release_group(pool: PgPool) {
    let mut e = env(pool).await;
    let seed_artist = e.artist("Seed").await;
    let gone = e.artist("Gone").await;
    mark_missing(&e.pool, e.track_of("Gone")).await;
    // A second album of Gone; only albums with a release group count (upstream `_count: { rgMbid }`).
    let second = seed(&e.pool, Seed::new("Gone", "Second", None, "other")).await;
    mark_missing(&e.pool, second.track).await;
    sqlx::query("UPDATE albums SET release_group_mbid = gen_random_uuid() WHERE id = $1")
        .bind(second.album)
        .execute(&e.pool)
        .await
        .unwrap();
    e.similar(seed_artist, &[("Gone", None, 0.9), ("Stranger", None, 0.5)])
        .await;
    e.plays(e.ana_profile, e.track_of("Seed"), 1, 1).await;
    let (artists, _) = e.for_you(&e.ana, "").await;
    assert_eq!(artists[0]["id"], gone.to_string());
    assert_eq!(artists[0]["albumCount"], 1);
    assert_eq!(artists[1]["albumCount"], 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_from_artist_image_token_else_null(pool: PgPool) {
    let mut e = env(pool).await;
    let seed_artist = e.artist("Seed").await;
    let pictured = e.artist("Pictured").await;
    mark_missing(&e.pool, e.track_of("Pictured")).await;
    sqlx::query("UPDATE artists SET image_path = 'p.jpg' WHERE id = $1")
        .bind(pictured)
        .execute(&e.pool)
        .await
        .unwrap();
    let plain = e.artist("Plain").await;
    mark_missing(&e.pool, e.track_of("Plain")).await;
    e.similar(
        seed_artist,
        &[
            ("Pictured", None, 0.9),
            ("Plain", None, 0.8),
            ("Stranger", Some("not-an-mbid"), 0.7),
        ],
    )
    .await;
    e.plays(e.ana_profile, e.track_of("Seed"), 1, 1).await;
    let (artists, text) = e.for_you(&e.ana, "").await;
    assert_eq!(
        artists,
        [
            json!({ "id": pictured.to_string(), "mbid": null, "name": "Pictured", "heroUrl": null,
                    "coverArt": pictured.to_string(), "albumCount": 0 }),
            json!({ "id": plain.to_string(), "mbid": null, "name": "Plain", "heroUrl": null,
                    "coverArt": null, "albumCount": 0 }),
            json!({ "id": "Stranger", "mbid": null, "name": "Stranger", "heroUrl": null,
                    "coverArt": null, "albumCount": 0 }),
        ]
    );
    // Upstream's key order: the artist row's `id, mbid, name, heroUrl`, then `coverArt`, `albumCount`.
    let mut at = 0;
    for _ in 0..3 {
        at = assert_in_order(
            &text,
            at,
            &["id", "mbid", "name", "heroUrl", "coverArt", "albumCount"],
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn limit_parse_like_parseint(pool: PgPool) {
    let mut e = env(pool).await;
    let a = e.artist("A").await;
    let sims: Vec<String> = (0..12).map(|i| format!("s{i:02}")).collect();
    let list: Vec<(&str, Option<&str>, f64)> = sims
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), None, 1.0 - i as f64 / 100.0))
        .collect();
    e.similar(a, &list).await;
    e.plays(e.ana_profile, e.track_of("A"), 1, 1).await;
    let count = async |q: &str| e.for_you(&e.ana, q).await.0.len();
    // Default 10; ten similar per seed at most.
    assert_eq!(count("").await, 10);
    assert_eq!(count("?limit=2").await, 2);
    assert_eq!(
        count("?limit=%202x").await,
        2,
        "parseInt reads the digit prefix"
    );
    assert_eq!(count("?limit=50").await, 10);
    // NaN: `slice(0, NaN)` is empty, as is 0 and an empty value.
    assert_eq!(count("?limit=abc").await, 0);
    assert_eq!(count("?limit=0").await, 0);
    assert_eq!(count("?limit=").await, 0);
    // A negative end counts from the back: `slice(0, -3)` of ten is seven.
    assert_eq!(count("?limit=-3").await, 7);
    // A repeated value reaches parseInt as Express's array joined with commas.
    assert_eq!(count("?limit=3&limit=9").await, 3);
    assert_eq!(e.names(&e.ana, "?limit=3").await, ["s00", "s01", "s02"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_isolation(pool: PgPool) {
    let mut e = env(pool).await;
    let a = e.artist("A").await;
    let b = e.artist("B").await;
    e.similar(a, &[("simA", None, 0.5)]).await;
    e.similar(b, &[("simB", None, 0.5)]).await;
    e.plays(e.ana_profile, e.track_of("A"), 3, 1).await;
    e.plays(e.bob_profile, e.track_of("B"), 3, 1).await;
    assert_eq!(e.names(&e.ana, "").await, ["simA"]);
    assert_eq!(e.names(&e.bob, "").await, ["simB"]);
    // The kids profile of ana's account has no plays of its own.
    assert!(e.for_you(&e.kid, "").await.0.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_a_signed_in_user(pool: PgPool) {
    let e = env(pool).await;
    e.server
        .get("/api/recommendations/for-you")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
