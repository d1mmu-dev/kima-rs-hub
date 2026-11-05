mod common;

use axum::http::StatusCode;
use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use serde_json::{Value, json};
use uuid::Uuid;

/// An artist with one album of `n` present tracks and one file genre.
async fn artist_with(pool: &PgPool, name: &str, n: usize, genre: &str) -> Uuid {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, lower($2))")
        .bind(a)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $1::text)",
    )
    .bind(b)
    .bind(a)
    .execute(pool)
    .await
    .unwrap();
    for _ in 0..n {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
             VALUES ($1, $2, $3, 't', 'mp3', $4, 1, now(), $5)",
        )
        .bind(id)
        .bind(b)
        .bind(name)
        .bind(format!("/{id}"))
        .bind(id.as_bytes().to_vec())
        .execute(pool)
        .await
        .unwrap();
    }
    let mut c = pool.acquire().await.unwrap();
    genres::replace_artist_genres(&mut c, a, GenreSource::File, &[genre.to_string()])
        .await
        .unwrap();
    a
}

async fn get(pool: PgPool) -> (axum_test::TestServer, String) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "bob").await;
    (
        axum_test::TestServer::new(kima_api::router(state)).unwrap(),
        token,
    )
}

async fn list(pool: PgPool) -> Value {
    let (server, token) = get(pool).await;
    let res = server
        .get("/api/library/genres")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    res.json()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_needs_auth(pool: PgPool) {
    let (server, _) = get(pool).await;
    server
        .get("/api/library/genres")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_lowercase(pool: PgPool) {
    artist_with(&pool, "A", 15, "Hip Hop").await;
    artist_with(&pool, "B", 14, "Small").await;
    let v = list(pool).await;
    assert_eq!(v, json!({"genres": [{"genre": "hip hop", "count": 15}]}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_threshold_15_and_limit_20(pool: PgPool) {
    artist_with(&pool, "Edge", 15, "e15").await;
    artist_with(&pool, "Under", 14, "e14").await;
    for i in 0..21 {
        artist_with(&pool, &format!("X{i}"), 20 + i, &format!("g{i:02}")).await;
    }
    let v = list(pool).await;
    let g = v["genres"].as_array().unwrap();
    assert_eq!(g.len(), 20);
    assert_eq!(g[0], json!({"genre": "g20", "count": 40}));
    assert_eq!(g[19], json!({"genre": "g01", "count": 21}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_drops_artist_named_genres(pool: PgPool) {
    artist_with(&pool, "Jazz", 15, "Jazz").await; // genre equals an artist's name
    artist_with(&pool, "Someone", 16, "Beyonce").await;
    artist_with(&pool, "Beyoncé", 1, "Pop").await; // accented artist drops "Beyonce"
    artist_with(&pool, "Other", 17, "Blues").await;
    let v = list(pool).await;
    assert_eq!(v, json!({"genres": [{"genre": "blues", "count": 17}]}));
}

async fn mb_genres(pool: &PgPool, artist: Uuid, names: &[&str]) {
    let mut c = pool.acquire().await.unwrap();
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    genres::replace_artist_genres(&mut c, artist, GenreSource::MusicBrainz, &names)
        .await
        .unwrap();
}

async fn user_genres(pool: &PgPool, artist: Uuid, names: &[&str]) {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    sqlx::query("UPDATE artists SET user_genres = $2 WHERE id = $1")
        .bind(artist)
        .bind(names)
        .execute(pool)
        .await
        .unwrap();
}

async fn page(server: &axum_test::TestServer, token: &str, artist: Uuid) -> Value {
    let res = server
        .get(&format!("/api/library/artists/{artist}"))
        .authorization_bearer(token)
        .await;
    res.assert_status_ok();
    res.json()
}

/// M-SYNC Task 9 (upstream `getMergedGenres`, D-166): a non-empty list of a person's own genres
/// *is* the artist's genres; the enriched ones show again only when they have none.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_genres_replace_enriched(pool: PgPool) {
    let a = artist_with(&pool, "Band", 15, "rock").await;
    mb_genres(&pool, a, &["rock", "indie"]).await;
    user_genres(&pool, a, &["jazz"]).await;
    let (server, token) = get(pool.clone()).await;

    let v = page(&server, &token, a).await;
    assert_eq!(v["genres"], json!(["jazz"]));
    assert_eq!(v["userGenres"], json!(["jazz"]));
    // `tags` stays the MusicBrainz list, whatever a person set.
    assert_eq!(v["tags"], json!(["rock", "indie"]));

    // The genre list counts the artist under jazz only.
    let res = server
        .get("/api/library/genres")
        .authorization_bearer(&token)
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({"genres": [{"genre": "jazz", "count": 15}]})
    );

    // Emptying their list brings the enriched genres back.
    user_genres(&pool, a, &[]).await;
    let v = page(&server, &token, a).await;
    assert_eq!(v["genres"], json!(["rock", "indie"]));
    let res = server
        .get("/api/library/genres")
        .authorization_bearer(&token)
        .await;
    let names: Vec<_> = res.json::<Value>()["genres"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["genre"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["indie", "rock"]);
}

/// A genre a person removes from the list is gone from the page and the genre list, not merged
/// back from the enriched genres.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn removed_genre_disappears_everywhere(pool: PgPool) {
    let a = artist_with(&pool, "Band", 15, "rock").await;
    mb_genres(&pool, a, &["rock", "indie"]).await;
    let state = common::state(pool.clone());
    let (_, admin) = common::user_with_token(&state, "ana").await;
    let server = axum_test::TestServer::new(kima_api::router(state)).unwrap();

    // The editor opens on the page's genres [rock, indie] and saves [indie].
    let res = server
        .put(&format!("/api/enrichment/artists/{a}/metadata"))
        .authorization_bearer(&admin)
        .json(&json!({"genres": ["indie"]}))
        .await;
    res.assert_status_ok();

    let v = page(&server, &admin, a).await;
    assert_eq!(v["genres"], json!(["indie"]));
    let res = server
        .get("/api/library/genres")
        .authorization_bearer(&admin)
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"genres": [{"genre": "indie", "count": 15}]})
    );
}

// `non_list_genre_json_reads_empty` (upstream `efcea10f`) does not apply: kima-rs keeps genres
// in relational tables (`genres`, `*_genres`) and a person's list in a `text[]` column, so there
// is no JSON value that can be anything but a list.
