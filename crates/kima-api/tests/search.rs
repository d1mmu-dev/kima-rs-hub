mod common;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use kima_db::PgPool;
use serde_json::Value;
use time::macros::datetime;
use uuid::Uuid;

struct Env {
    server: TestServer,
    token: String,
}

async fn env(pool: PgPool) -> Env {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "ana").await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        token,
    }
}

async fn search(e: &Env, path: &str) -> Value {
    let res = e.server.get(path).authorization_bearer(&e.token).await;
    res.assert_status_ok();
    res.json()
}

fn len(v: &Value, key: &str) -> usize {
    v[key].as_array().unwrap().len()
}

/// A followed show with `title`/`description`, subscribed by `user`.
async fn followed_show(pool: &PgPool, user: Uuid, title: &str, description: &str) -> Uuid {
    use kima_db::podcasts::{self, NewShow};
    let show = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &format!("https://feeds.example.invalid/{title}"),
            title,
            author: Some("Someone"),
            description: Some(description),
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    podcasts::subscribe(pool, user, show.id).await.unwrap();
    show.id
}

async fn episode(
    pool: &PgPool,
    show: Uuid,
    guid: &str,
    title: &str,
    description: &str,
    published_at: time::OffsetDateTime,
) {
    use kima_db::podcasts::{self, NewEpisode};
    podcasts::insert_episodes(
        pool,
        show,
        &[NewEpisode {
            guid: guid.to_string(),
            title: title.to_string(),
            description: Some(description.to_string()),
            audio_url: format!("https://cdn.example.invalid/{guid}.mp3"),
            duration: 100,
            published_at,
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        }],
    )
    .await
    .unwrap();
}

/// M6 Task 16: `GET /search?type=podcasts` and `?type=episodes` search only the shows and
/// episodes `user` follows (upstream `searchPodcastsFTS`/`searchEpisodes`, followed-only).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_finds_only_followed_shows_and_their_episodes(pool: PgPool) {
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "poduser").await;
    let followed = followed_show(&pool, user.id, "Jazz Café Talk", "about jazz").await;
    // Another show exists but is not followed; it must never surface in search.
    let other = {
        use kima_db::podcasts::{self, NewShow};
        let show = podcasts::insert_show(
            &pool,
            Uuid::now_v7(),
            &NewShow {
                feed_url: "https://feeds.example.invalid/unfollowed",
                title: "Jazz Lounge",
                author: None,
                description: Some("also about jazz"),
                image_url: None,
                itunes_id: None,
                category_ids: &[],
                language: None,
                explicit: false,
                episode_count: 0,
                feed_etag: None,
                feed_last_modified: None,
            },
        )
        .await
        .unwrap();
        show.id
    };
    episode(
        &pool,
        followed,
        "ep-1",
        "Jazz Interview",
        "jazz talk",
        datetime!(2026-01-01 00:00 UTC),
    )
    .await;
    episode(
        &pool,
        other,
        "ep-unfollowed",
        "Jazz Interview Too",
        "jazz talk",
        datetime!(2026-01-01 00:00 UTC),
    )
    .await;

    let e = Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        token,
    };
    let v = search(&e, "/api/search?q=jazz&type=podcasts").await;
    assert_eq!(len(&v, "podcasts"), 1);
    assert_eq!(v["podcasts"][0]["title"], "Jazz Café Talk");
    assert_eq!(v["podcasts"][0]["aiLabel"], "unknown");
    assert!(v["podcasts"][0]["aiLabelSource"].is_null());

    let v = search(&e, "/api/search?q=jazz&type=episodes").await;
    assert_eq!(len(&v, "episodes"), 1);
    assert_eq!(v["episodes"][0]["title"], "Jazz Interview");
    assert_eq!(v["episodes"][0]["podcastTitle"], "Jazz Café Talk");
}

/// `searchEpisodes`: rank desc, then `publishedAt` desc.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_episodes_rank_then_newest(pool: PgPool) {
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "poduser2").await;
    let show = followed_show(&pool, user.id, "A Show", "about things").await;
    episode(
        &pool,
        show,
        "older",
        "Older Dungeons Episode",
        "",
        datetime!(2026-01-01 00:00 UTC),
    )
    .await;
    episode(
        &pool,
        show,
        "newer",
        "Newer Dungeons Episode",
        "",
        datetime!(2026-02-01 00:00 UTC),
    )
    .await;

    let e = Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        token,
    };
    let v = search(&e, "/api/search?q=dungeons&type=episodes").await;
    assert_eq!(len(&v, "episodes"), 2);
    assert_eq!(v["episodes"][0]["title"], "Newer Dungeons Episode");
    assert_eq!(v["episodes"][1]["title"], "Older Dungeons Episode");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn finds_artist_album_track(pool: PgPool) {
    seed(
        &pool,
        Seed::new(
            "Radiohead",
            "Kid A",
            Some(2000),
            "Everything In Its Right Place",
        ),
    )
    .await;
    seed(
        &pool,
        Seed::new("Other", "Unrelated", Some(2001), "Nothing"),
    )
    .await;
    let e = env(pool).await;

    let v = search(&e, "/api/search?q=radiohead").await;
    assert_eq!(v["artists"][0]["name"], "Radiohead");
    assert_eq!(v["artists"][0]["trackCount"], 1);
    assert_eq!(len(&v, "albums"), 1);
    assert_eq!(v["albums"][0]["title"], "Kid A");
    assert_eq!(v["albums"][0]["artist"]["name"], "Radiohead");
    assert_eq!(len(&v, "tracks"), 1);
    for k in ["audiobooks", "podcasts", "episodes"] {
        assert_eq!(len(&v, k), 0);
    }

    let v = search(&e, "/api/search?q=kid").await;
    assert_eq!(v["albums"][0]["title"], "Kid A");
    let v = search(&e, "/api/search?q=right+place").await;
    assert_eq!(v["tracks"][0]["title"], "Everything In Its Right Place");
    assert!(v["tracks"][0]["album"]["coverUrl"].is_string());

    let v = search(&e, "/api/search?q=radiohead&type=albums").await;
    assert_eq!(len(&v, "artists"), 0);
    assert_eq!(len(&v, "albums"), 1);
    assert_eq!(len(&v, "tracks"), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn prefix_matches_partial_word(pool: PgPool) {
    seed(
        &pool,
        Seed::new("Radiohead", "Kid A", Some(2000), "Idioteque"),
    )
    .await;
    let e = env(pool).await;
    let v = search(&e, "/api/search?q=radioh").await;
    assert_eq!(v["artists"][0]["name"], "Radiohead");
    let v = search(&e, "/api/search?q=kid%20a").await;
    assert_eq!(len(&v, "albums"), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn case_and_accents(pool: PgPool) {
    seed(&pool, Seed::new("Beyoncé", "Lemonade", Some(2016), "Sorry")).await;
    let e = env(pool).await;
    for q in ["beyonce", "BEYONC%C3%89", "BeYoN"] {
        let v = search(&e, &format!("/api/search?q={q}")).await;
        assert_eq!(v["artists"][0]["name"], "Beyoncé", "query {q}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_tracks_excluded(pool: PgPool) {
    let gone = seed(&pool, Seed::new("Ghost", "Vanished", Some(2000), "Poof")).await;
    seed(&pool, Seed::new("Ghost", "Present", Some(2001), "Here")).await;
    mark_missing(&pool, gone.track).await;
    let e = env(pool).await;

    let v = search(&e, "/api/search?q=poof").await;
    assert_eq!(len(&v, "tracks"), 0);
    let v = search(&e, "/api/search?q=vanished").await;
    assert_eq!(len(&v, "albums"), 0);
    let v = search(&e, "/api/search?q=ghost").await;
    assert_eq!(len(&v, "artists"), 1);
    assert_eq!(v["artists"][0]["trackCount"], 1);
    assert_eq!(len(&v, "albums"), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn empty_query_returns_empty_lists(pool: PgPool) {
    let e = env(pool).await;
    for path in ["/api/search", "/api/search?q=", "/api/search?q=%20%20"] {
        let v = search(&e, path).await;
        for k in [
            "artists",
            "albums",
            "tracks",
            "audiobooks",
            "podcasts",
            "episodes",
        ] {
            assert_eq!(len(&v, k), 0, "{path} {k}");
        }
    }
    let v = search(&e, "/api/search?q=%21%21").await;
    assert_eq!(len(&v, "tracks"), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn odd_queries_never_500(pool: PgPool) {
    seed(&pool, Seed::new("x", "A B", Some(2000), "a b")).await;
    let e = env(pool).await;
    let long = "a".repeat(2000);
    let cases = [
        "x -\"a b\"".to_string(),
        "-\"a b\"".to_string(),
        "\"unterminated".to_string(),
        "a & | b".to_string(),
        ":*".to_string(),
        long,
    ];
    for q in cases {
        let res = e
            .server
            .get("/api/search")
            .add_query_param("q", &q)
            .authorization_bearer(&e.token)
            .await;
        res.assert_status_ok();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn requires_auth(pool: PgPool) {
    let e = env(pool).await;
    e.server
        .get("/api/search?q=a")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// M-SYNC Task 10 (upstream `4338048e`, `nonAsciiNames.integration.test.ts`, intent only):
/// names in any script are found in any case, accents are folded, a decomposed accent reads as
/// the composed letter and curly apostrophes match straight ones. kima-rs's database is UTF-8
/// (`unaccent` + the `simple` configuration), so none of upstream's `SQL_ASCII` workarounds
/// apply; this pins the outcome.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_non_ascii_names_any_case(pool: PgPool) {
    seed(
        &pool,
        Seed::new("Beyoncé", "Lemonade", Some(2016), "Formation"),
    )
    .await;
    seed(
        &pool,
        Seed::new("ДДТ", "Актриса Весна", Some(1992), "Родина"),
    )
    .await;
    seed(&pool, Seed::new("東京事変", "大人", Some(2006), "群青日和")).await;
    seed(
        &pool,
        Seed::new("Guns N’ Roses", "Appetite", Some(1987), "Welcome"),
    )
    .await;
    seed(
        &pool,
        Seed::new("Zoë Keating", "Into the Trees", Some(2010), "Escape"),
    )
    .await;
    let e = env(pool).await;
    let artist = |v: &Value| -> Vec<String> {
        v["artists"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect()
    };
    for (query, expected) in [
        ("beyonce", "Beyoncé"),
        ("BEYONCÉ", "Beyoncé"),
        ("beyoncé", "Beyoncé"),
        ("ддт", "ДДТ"),
        ("ДДТ", "ДДТ"),
        ("東京事変", "東京事変"),
        // Curly in the library, straight typed, and the reverse.
        ("guns n' roses", "Guns N’ Roses"),
        ("Guns N’ Roses", "Guns N’ Roses"),
        // `e` + U+0308 (decomposed) against the composed `ë`.
        ("zoe\u{308} keating", "Zoë Keating"),
        ("ZOË", "Zoë Keating"),
    ] {
        let v = search(&e, &format!("/api/search?q={}", urlencode(query))).await;
        assert_eq!(artist(&v), [expected], "{query:?}");
    }
}

fn urlencode(s: &str) -> String {
    s.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
        out
    })
}

/// M6 Task 8 (D-207): the podcast groups hide AI-labelled shows and episodes by default, say
/// how many with `X-Kima-Ai-Hidden`, and show them labelled with `?ai=show`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_hides_ai_shows_and_episodes_by_default(pool: PgPool) {
    let state = common::state(pool.clone());
    let (user, token) = common::user_with_token(&state, "poduser").await;
    let ai = followed_show(&pool, user.id, "Jazz Robots", "about jazz").await;
    let plain = followed_show(&pool, user.id, "Jazz People", "about jazz").await;
    sqlx::query("UPDATE podcasts SET ai_feed = true WHERE id = $1")
        .bind(ai)
        .execute(&pool)
        .await
        .unwrap();
    let t = datetime!(2026-01-01 00:00 UTC);
    episode(&pool, ai, "r1", "Jazz Synth", "jazz", t).await;
    episode(&pool, plain, "h1", "Jazz Live", "jazz", t).await;
    episode(&pool, plain, "h2", "Jazz Generated", "jazz", t).await;
    sqlx::query("UPDATE podcast_episodes SET ai_feed = true WHERE guid = 'h2'")
        .execute(&pool)
        .await
        .unwrap();
    let e = Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        token,
    };
    let res = e
        .server
        .get("/api/search?q=jazz&type=all")
        .authorization_bearer(&e.token)
        .await;
    res.assert_status_ok();
    assert_eq!(res.headers()["x-kima-ai-hidden"], "3");
    let v: Value = res.json();
    assert_eq!(len(&v, "podcasts"), 1);
    assert_eq!(v["podcasts"][0]["title"], "Jazz People");
    assert_eq!(len(&v, "episodes"), 1);
    assert_eq!(v["episodes"][0]["title"], "Jazz Live");

    let res = e
        .server
        .get("/api/search?q=jazz&type=all&ai=show")
        .authorization_bearer(&e.token)
        .await;
    assert!(res.headers().get("x-kima-ai-hidden").is_none());
    let v: Value = res.json();
    assert_eq!(len(&v, "podcasts"), 2);
    assert_eq!(len(&v, "episodes"), 3);
    let robots = v["podcasts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["title"] == "Jazz Robots")
        .unwrap();
    assert_eq!(robots["aiLabel"], "ai");
    assert_eq!(robots["aiLabelSource"], "feed-show");
}
