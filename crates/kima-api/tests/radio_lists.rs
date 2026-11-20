//! M5b Task 11: `GET /api/library/decades`, `GET /api/library/radio/stations` and the parity of the existing
//! `GET /api/library/genres` with upstream's `libraryGenres` (`services/mixes/radioRow.ts:40-76`, routes
//! `routes/library/tracks.ts:573-596`), with the cases of `routes/library/__tests__/genreSpellings.integration.test.ts`
//! that concern `GET /genres` (its `GET /radio?type=genre` cases belong to Task 13).

mod common;

use axum::http::StatusCode;
use common::mix_keys::assert_in_order;
use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use serde_json::{Value, json};
use uuid::Uuid;

async fn server(pool: PgPool) -> (axum_test::TestServer, String) {
    let state = common::state(pool);
    let (_, token) = common::user_with_token(&state, "bob").await;
    (
        axum_test::TestServer::new(kima_api::router(state)).unwrap(),
        token,
    )
}

async fn artist(pool: &PgPool, name: &str) -> Uuid {
    let a = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $1::text)")
        .bind(a)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    a
}

/// An album of `artist` with `n` tracks, `missing` of them missing.
async fn album(
    pool: &PgPool,
    artist: Uuid,
    year: Option<i32>,
    display_year: Option<i32>,
    n: usize,
    missing: usize,
) -> Uuid {
    let b = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year, display_year)
         VALUES ($1, $2, 't', $1::text, $3, $4)",
    )
    .bind(b)
    .bind(artist)
    .bind(year)
    .bind(display_year)
    .execute(pool)
    .await
    .unwrap();
    for i in 0..n {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint, missing_since)
             VALUES ($1, $2, 'x', 't', 'mp3', $3, 1, now(), $4, CASE WHEN $5 THEN now() END)",
        )
        .bind(id)
        .bind(b)
        .bind(format!("/{id}"))
        .bind(id.as_bytes().to_vec())
        .bind(i < missing)
        .execute(pool)
        .await
        .unwrap();
    }
    b
}

/// An artist named `name` with one album of `n` present tracks and the file genres `tags`.
async fn tagged(pool: &PgPool, name: &str, n: usize, tags: &[&str]) -> Uuid {
    let a = artist(pool, name).await;
    album(pool, a, None, None, n, 0).await;
    let mut c = pool.acquire().await.unwrap();
    let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    genres::replace_artist_genres(&mut c, a, GenreSource::File, &tags)
        .await
        .unwrap();
    a
}

async fn own_list(pool: &PgPool, artist: Uuid, names: &[&str]) {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    sqlx::query("UPDATE artists SET user_genres = $2 WHERE id = $1")
        .bind(artist)
        .bind(names)
        .execute(pool)
        .await
        .unwrap();
}

async fn get_text(server: &axum_test::TestServer, token: &str, path: &str) -> (Value, String) {
    let res = server.get(path).authorization_bearer(token).await;
    res.assert_status_ok();
    let text = res.text();
    (serde_json::from_str(&text).unwrap(), text)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lists_need_a_session(pool: PgPool) {
    let (server, _) = server(pool).await;
    for path in [
        "/api/library/decades",
        "/api/library/radio/stations",
        "/api/library/genres",
    ] {
        server
            .get(path)
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}

/// Upstream `libraryDecades`: an album's year is `getEffectiveYear` (`displayYear ?? originalYear ?? year`; kima keeps
/// no original year, D-272/D-276), its songs count in that decade, a decade with fewer than 15 is left out, newest
/// first. kima counts present songs (D-276).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn decades_effective_year_display_original_year_min15_newest_first(pool: PgPool) {
    let a = artist(&pool, "Decade Artist").await;
    album(&pool, a, Some(1987), None, 15, 0).await; // 1980: 15
    album(&pool, a, Some(1995), Some(2003), 10, 0).await; // shown as 2003: 2000
    album(&pool, a, Some(2001), None, 5, 0).await; // 2000: 15 with the one above
    album(&pool, a, Some(1992), None, 14, 0).await; // 1990: 14, too few
    album(&pool, a, None, None, 20, 0).await; // no year: no decade
    album(&pool, a, Some(1975), None, 16, 2).await; // 1970: 14 present, too few
    album(&pool, a, Some(2019), Some(2020), 15, 0).await; // the display year's decade: 2020
    let (server, token) = server(pool).await;
    let (v, text) = get_text(&server, &token, "/api/library/decades").await;
    assert_eq!(
        v,
        json!({"decades": [
            {"decade": 2020, "count": 15},
            {"decade": 2000, "count": 15},
            {"decade": 1980, "count": 15},
        ]})
    );
    assert_in_order(&text, 0, &["decades", "decade", "count"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn decades_of_an_empty_library_are_none(pool: PgPool) {
    let (server, token) = server(pool).await;
    let (v, _) = get_text(&server, &token, "/api/library/decades").await;
    assert_eq!(v, json!({"decades": []}));
}

/// Upstream `moodStationList`: the 14 stations (13 mood stations and Wildcard) in their order, each with the
/// subtitle generated from its recipe, as `{value, name, description}`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stations_list_14_with_subtitles(pool: PgPool) {
    let (server, token) = server(pool).await;
    let (v, text) = get_text(&server, &token, "/api/library/radio/stations").await;
    let expected: Vec<Value> = kima_mixes::stations::mood_station_list()
        .into_iter()
        .map(|s| json!({"value": s.value, "name": s.name, "description": s.description}))
        .collect();
    assert_eq!(expected.len(), 14);
    assert_eq!(v, json!({ "stations": expected }));
    let mut at = assert_in_order(&text, 0, &["stations"]);
    for _ in 0..14 {
        at = assert_in_order(&text, at, &["value", "name", "description"]);
    }
    assert!(
        v["stations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| !s["description"].as_str().unwrap().is_empty())
    );
    assert_eq!(v["stations"][13]["value"], "wildcard");
}

/// `genreSpellings`' `GET /genres` cases on kima's genre key (lower-case, whitespace collapsed; upstream's looser
/// `genreKey` is the open option of D-166 (2)), plus `libraryGenres`' 15-song floor, top 20, lower-cased names, most
/// songs first then by name.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn library_genres_matches_upstream_min15_top20(pool: PgPool) {
    // One genre in two spellings: listed under the spelling carrying the most songs, counting both.
    tagged(&pool, "Spelling Artist 1", 20, &["Hip Hop"]).await;
    let a2 = tagged(&pool, "Spelling Artist 2", 3, &[]).await;
    own_list(&pool, a2, &["Hip  Hop"]).await;
    // Hyphenated spellings stay their own genre on kima's key (D-166 (2), open).
    tagged(&pool, "Spelling Artist 3", 15, &["hip-hop"]).await;
    // Too few songs.
    tagged(&pool, "Spelling Artist 4", 5, &["alternative hip hop"]).await;
    // Different words in any script stay apart.
    tagged(&pool, "Spelling Artist 5", 16, &["ロック"]).await;
    tagged(&pool, "Spelling Artist 6", 16, &["ジャズ"]).await;
    tagged(&pool, "Spelling Artist 7", 16, &["rock"]).await;
    // An artist tagged with its own name, spelled another way, adds nothing.
    tagged(&pool, "Synth-Wave", 16, &["synth wave"]).await;
    // A band named like a genre neither adds its self-tag nor hides everyone else's jazz.
    tagged(&pool, "Jazz", 4, &["jazz"]).await;
    tagged(&pool, "Spelling Artist 16", 16, &["jazz"]).await;
    // Upstream's internal marker names are no genres.
    tagged(&pool, "Spelling Artist 17", 30, &["_hidden"]).await;
    // A name made only of spaces and punctuation is no genre.
    tagged(&pool, "Spelling Artist 18", 30, &["-"]).await;

    let (server, token) = server(pool.clone()).await;
    let (v, text) = get_text(&server, &token, "/api/library/genres").await;
    assert_eq!(
        v,
        json!({"genres": [
            {"genre": "hip hop", "count": 23},
            {"genre": "jazz", "count": 16},
            {"genre": "rock", "count": 16},
            {"genre": "ジャズ", "count": 16},
            {"genre": "ロック", "count": 16},
            {"genre": "hip-hop", "count": 15},
        ]})
    );
    assert_in_order(&text, 0, &["genres", "genre", "count"]);

    // At most 20, most songs first.
    for i in 0..21 {
        tagged(&pool, &format!("Many {i}"), 30 + i, &[&format!("g{i:02}")]).await;
    }
    let (v, _) = get_text(&server, &token, "/api/library/genres").await;
    let g = v["genres"].as_array().unwrap();
    assert_eq!(g.len(), 20);
    assert_eq!(g[0], json!({"genre": "g20", "count": 50}));
    assert_eq!(g[19], json!({"genre": "g01", "count": 31}));
}

/// Upstream answers 500 with its own message when the lists cannot be read.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failures_answer_upstream_messages(pool: PgPool) {
    sqlx::query("ALTER TABLE albums RENAME COLUMN display_year TO display_year_gone")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE artists RENAME COLUMN user_genres TO user_genres_gone")
        .execute(&pool)
        .await
        .unwrap();
    let (server, token) = server(pool).await;
    for (path, message) in [
        ("/api/library/decades", "Failed to get decades"),
        ("/api/library/genres", "Failed to get genres"),
    ] {
        let res = server.get(path).authorization_bearer(&token).await;
        res.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(res.json::<Value>(), json!({ "error": message }));
    }
}
