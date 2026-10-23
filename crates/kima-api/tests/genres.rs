mod common;

use axum::http::StatusCode;
use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use serde_json::{Value, json};
use uuid::Uuid;

/// An artist with one album of `n` present tracks and one file genre.
async fn artist_with(pool: &PgPool, name: &str, n: usize, genre: &str) {
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
