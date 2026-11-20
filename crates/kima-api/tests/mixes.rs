//! `/api/mixes` (M5b Task 9; port of upstream `routes/__tests__/mixesTimeZone.test.ts`, `mixesSave.test.ts` and the
//! route half of `services/mixes/__tests__/mixRotationFailures.test.ts` at the pin `c7527577`): the day's list, one
//! mix with its track rows (owner Q6 = B, D-264), the refresh, and saving a mix as a playlist. The mood heads are
//! absent here (their live state), so the day's five come from the generators that read no recipe.

mod common;

use std::collections::BTreeSet;

use axum::http::StatusCode;
use axum_test::TestServer;
use common::library::{Seed, mark_missing, seed};
use common::mix_keys::{ALBUM_KEYS, TRACK_KEYS, assert_in_order, key_set};
use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use kima_db::users::User;
use serde_json::{Value, json};
use uuid::Uuid;

struct Env {
    server: TestServer,
    state: kima_api::AppState,
    ana: String,
    ana_profile: ProfileId,
    ana_account: AccountId,
    /// A sibling profile of ana's account.
    kid: String,
    ben: String,
    tracks: Vec<Uuid>,
}

fn token_for(state: &kima_api::AppState, user: &User, profile: ProfileId) -> String {
    state.jwt.issue_access(
        user.account(),
        profile,
        &user.username,
        &user.role,
        user.token_version,
    )
}

/// Ten artists with one 2005 album of four songs each (40 songs), and ana's plays: songs 0..5 heard 6, 5, .., 1
/// times yesterday. Enough for Era, Top Tracks, Rediscover, Random Discovery and Deep Cuts with no mood heads.
async fn env(pool: PgPool) -> Env {
    let state = common::state(pool.clone());
    let (ana_user, ana) = common::user_with_token(&state, "ana").await;
    let (_, ben) = common::user_with_token(&state, "ben").await;
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
    .unwrap();
    let kid = token_for(&state, &ana_user, kid_profile.id);
    let mut tracks = Vec::new();
    for a in 0..10 {
        let artist = format!("Artist {a}");
        let album = format!("Album {a}");
        for n in 0..4 {
            let title = format!("Song {a}-{n}");
            tracks.push(
                seed(
                    &pool,
                    Seed::new(&artist, &album, Some(2005), &title).at(1, n + 1),
                )
                .await
                .track,
            );
        }
    }
    let ana_profile = ana_user.main_profile();
    for (i, &t) in tracks.iter().take(6).enumerate() {
        for _ in 0..(6 - i) {
            sqlx::query(
                "INSERT INTO plays (id, profile_id, track_id, played_at, heard)
                 VALUES ($1, $2, $3, now() - interval '1 day', true)",
            )
            .bind(Uuid::now_v7())
            .bind(ana_profile.into_uuid())
            .bind(t)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    Env {
        server: TestServer::new(kima_api::router(state.clone())).unwrap(),
        state,
        ana,
        ana_profile,
        ana_account: ana_user.account(),
        kid,
        ben,
        tracks,
    }
}

async fn mixes(e: &Env, token: &str, tz: Option<&str>) -> Vec<Value> {
    let mut req = e.server.get("/api/mixes").authorization_bearer(token);
    if let Some(tz) = tz {
        req = req.add_query_param("tz", tz);
    }
    let res = req.await;
    res.assert_status_ok();
    res.json::<Vec<Value>>()
}

fn ids(list: &[Value]) -> Vec<String> {
    list.iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect()
}

fn mix_with<'a>(list: &'a [Value], kind: &str) -> &'a Value {
    list.iter()
        .find(|m| m["type"] == kind)
        .unwrap_or_else(|| panic!("no {kind} mix in {list:?}"))
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_mixes_returns_five_plus_saved_mood_mix(pool: PgPool) {
    let e = env(pool.clone()).await;
    let list = mixes(&e, &e.ana, None).await;
    assert_eq!(list.len(), 5, "{list:?}");
    for m in &list {
        let keys: BTreeSet<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "id",
                "type",
                "name",
                "description",
                "trackIds",
                "coverUrls",
                "trackCount",
                "color"
            ]),
            "{m}"
        );
        assert_eq!(
            m["trackCount"],
            json!(m["trackIds"].as_array().unwrap().len())
        );
    }
    let kinds: BTreeSet<&str> = list.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.len(), 5, "five different types: {kinds:?}");

    // A saved mood mix is appended after the five, read again from the library (a missing song is not counted).
    sqlx::query(
        "INSERT INTO user_mood_mix (profile_id, mood, track_ids, cover_urls, served_track_ids, generated_at)
         VALUES ($1, 'chill', $2, '{}', $2, '2026-11-19T10:00:00Z')",
    )
    .bind(e.ana_profile.into_uuid())
    .bind(&e.tracks[..3])
    .execute(&pool)
    .await
    .unwrap();
    mark_missing(&pool, e.tracks[1]).await;
    e.state.listeners.drop_mix_lists(e.ana_profile.into_uuid());
    let list = mixes(&e, &e.ana, None).await;
    assert_eq!(list.len(), 6, "{list:?}");
    let saved = &list[5];
    assert_eq!(
        saved["id"],
        json!(format!("your-mood-mix-{}", 1_795_082_400_000_i64))
    );
    assert_eq!(saved["type"], "mood");
    assert_eq!(saved["mood"], "chill");
    assert_eq!(saved["name"], "Your Calm Mix");
    assert_eq!(saved["description"], "Based on your calm preferences");
    assert_eq!(
        saved["trackIds"],
        json!([e.tracks[0].to_string(), e.tracks[2].to_string()])
    );
    assert_eq!(saved["trackCount"], 2);
    // A sibling profile and another account have no saved mix.
    assert_eq!(
        mixes(&e, &e.kid, None)
            .await
            .iter()
            .filter(|m| m["type"] == "mood")
            .count(),
        0
    );
    assert_eq!(
        mixes(&e, &e.ben, None)
            .await
            .iter()
            .filter(|m| m["type"] == "mood")
            .count(),
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_generator_is_replaced_by_next_untried_type(pool: PgPool) {
    // No mood heads: every recipe generator has no mix, so whatever the day picks, the list is filled from the
    // generators that need none, in upstream's order, one per type.
    let e = env(pool).await;
    let list = mixes(&e, &e.ana, None).await;
    let kinds: Vec<&str> = list.iter().map(|m| m["type"].as_str().unwrap()).collect();
    let allowed = [
        "era",
        "genre",
        "top-tracks",
        "rediscover",
        "artist-similar",
        "discovery",
        "deep-cuts",
        "key-journey",
        "tempo-flow",
        "vocal-detox",
        // Minor Key Mondays is of type melancholy.
        "melancholy",
    ];
    assert!(kinds.iter().all(|k| allowed.contains(k)), "{kinds:?}");
    assert_eq!(kinds.len(), 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn same_day_same_list_refresh_differs(pool: PgPool) {
    let e = env(pool).await;
    let first = mixes(&e, &e.ana, None).await;
    // Same day, even with the cache dropped: the same list.
    e.state.listeners.drop_mix_lists(e.ana_profile.into_uuid());
    assert_eq!(mixes(&e, &e.ana, None).await, first);

    let res = e
        .server
        .post("/api/mixes/refresh")
        .authorization_bearer(&e.ana)
        .await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["message"], "Mixes refreshed");
    let refreshed = body["mixes"].as_array().unwrap().clone();
    assert_eq!(refreshed.len(), 5);
    // A refresh draws the day again: the dated mix ids carry the reroll.
    assert_ne!(ids(&refreshed), ids(&first));
    // ...and its list is what the zone now serves.
    assert_eq!(mixes(&e, &e.ana, None).await, refreshed);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn zone_keyed_cache(pool: PgPool) {
    let e = env(pool).await;
    let warsaw = mixes(&e, &e.ana, Some("Europe/Warsaw")).await;
    let utc = mixes(&e, &e.ana, Some("UTC")).await;
    assert_eq!(warsaw, utc, "same day, same draw");
    // A refresh in one zone replaces that zone's list only.
    let res = e
        .server
        .post("/api/mixes/refresh")
        .authorization_bearer(&e.ana)
        .add_query_param("tz", "UTC")
        .await;
    res.assert_status_ok();
    let refreshed = res.json::<Value>()["mixes"].as_array().unwrap().clone();
    assert_eq!(mixes(&e, &e.ana, Some("UTC")).await, refreshed);
    assert_eq!(mixes(&e, &e.ana, Some("Europe/Warsaw")).await, warsaw);
    // An unknown zone is the server's own zone (UTC here), so it reads UTC's list.
    assert_eq!(mixes(&e, &e.ana, Some("Not/AZone")).await, refreshed);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_mix_tracks_in_mix_order_present_only(pool: PgPool) {
    let e = env(pool.clone()).await;
    let list = mixes(&e, &e.ana, None).await;
    let top = mix_with(&list, "top-tracks");
    let id = top["id"].as_str().unwrap();
    let order: Vec<String> = top["trackIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(order.len(), 6);
    // Most played first.
    assert_eq!(order[0], e.tracks[0].to_string());
    let gone: Uuid = order[2].parse().unwrap();
    mark_missing(&pool, gone).await;

    let res = e
        .server
        .get(&format!("/api/mixes/{id}"))
        .authorization_bearer(&e.ana)
        .await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["id"], json!(id));
    // The kept mix still lists the song; the rows leave it out, in the mix's order otherwise.
    assert_eq!(body["trackIds"].as_array().unwrap().len(), 6);
    let rows: Vec<String> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect();
    let want: Vec<String> = order
        .iter()
        .filter(|t| **t != gone.to_string())
        .cloned()
        .collect();
    assert_eq!(rows, want);

    let res = e
        .server
        .get("/api/mixes/no-such-mix")
        .authorization_bearer(&e.ana)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({ "error": "Mix not found" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_mixes_refresh_is_404_mix_not_found_not_405(pool: PgPool) {
    let e = env(pool).await;
    let res = e
        .server
        .get("/api/mixes/refresh")
        .authorization_bearer(&e.ana)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({ "error": "Mix not found" }));
    // Every route needs a session.
    for (method, path) in [
        ("GET", "/api/mixes"),
        ("GET", "/api/mixes/top-tracks"),
        ("POST", "/api/mixes/refresh"),
        ("POST", "/api/mixes/top-tracks/save"),
    ] {
        let res = match method {
            "GET" => e.server.get(path).await,
            _ => e.server.post(path).await,
        };
        res.assert_status(StatusCode::UNAUTHORIZED);
    }
}

async fn save(e: &Env, token: &str, id: &str, body: Value) -> axum_test::TestResponse {
    e.server
        .post(&format!("/api/mixes/{id}/save"))
        .authorization_bearer(token)
        .json(&body)
        .await
}

async fn playlist_items(pool: &PgPool, playlist: &str) -> Vec<Uuid> {
    sqlx::query_scalar(
        "SELECT track_id FROM playlist_items WHERE playlist_id = $1::uuid ORDER BY sort",
    )
    .bind(playlist)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_creates_playlist_atomically_private_name_or_custom(pool: PgPool) {
    let e = env(pool.clone()).await;
    let list = mixes(&e, &e.ana, None).await;
    let top = mix_with(&list, "top-tracks");
    let res = save(&e, &e.ana, "top-tracks", json!({})).await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["name"], top["name"]);
    assert_eq!(body["trackCount"], 6);
    let keys: BTreeSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, BTreeSet::from(["id", "name", "trackCount"]));
    let id = body["id"].as_str().unwrap();
    let (is_public, mix_id, profile): (bool, Option<String>, Uuid) =
        sqlx::query_as("SELECT is_public, mix_id, profile_id FROM playlists WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!is_public);
    assert_eq!(mix_id.as_deref(), Some("top-tracks"));
    assert_eq!(profile, e.ana_profile.into_uuid());
    let want: Vec<Uuid> = top["trackIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(playlist_items(&pool, id).await, want);

    // A custom name; and an unknown mix is 404.
    let era = mix_with(&list, "era")["id"].as_str().unwrap().to_owned();
    let res = save(&e, &e.ana, &era, json!({ "name": "Noughties" })).await;
    res.assert_status_ok();
    assert_eq!(res.json::<Value>()["name"], "Noughties");
    let res = save(&e, &e.ana, "no-such-mix", json!({})).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({ "error": "Mix not found" }));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_twice_409_with_playlist_id_and_name(pool: PgPool) {
    let e = env(pool).await;
    mixes(&e, &e.ana, None).await;
    let first = save(&e, &e.ana, "top-tracks", json!({ "name": "Mine" })).await;
    first.assert_status_ok();
    let first = first.json::<Value>();
    let res = save(&e, &e.ana, "top-tracks", json!({})).await;
    res.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "Mix already saved as playlist", "playlistId": first["id"], "name": "Mine" })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_saves_one_playlist(pool: PgPool) {
    let e = env(pool.clone()).await;
    mixes(&e, &e.ana, None).await;
    let (a, b, c) = tokio::join!(
        save(&e, &e.ana, "top-tracks", json!({})),
        save(&e, &e.ana, "top-tracks", json!({})),
        save(&e, &e.ana, "top-tracks", json!({})),
    );
    let codes: Vec<u16> = [&a, &b, &c]
        .iter()
        .map(|r| r.status_code().as_u16())
        .collect();
    assert_eq!(codes.iter().filter(|c| **c == 200).count(), 1, "{codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == 409).count(), 2, "{codes:?}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM playlists WHERE mix_id = 'top-tracks'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_all_missing_422(pool: PgPool) {
    let e = env(pool.clone()).await;
    let list = mixes(&e, &e.ana, None).await;
    for t in mix_with(&list, "top-tracks")["trackIds"]
        .as_array()
        .unwrap()
    {
        mark_missing(&pool, t.as_str().unwrap().parse().unwrap()).await;
    }
    let res = save(&e, &e.ana, "top-tracks", json!({})).await;
    res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "None of this mix's songs are in the library any more" })
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM playlists")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "nothing written");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_announces_playlist_event(pool: PgPool) {
    let e = env(pool).await;
    mixes(&e, &e.ana, None).await;
    let mut rx = e.state.events.subscribe();
    let res = save(&e, &e.ana, "top-tracks", json!({})).await;
    res.assert_status_ok();
    let id = res.json::<Value>()["id"].clone();
    let event = rx.try_recv().expect("one event");
    assert_eq!(
        event.data,
        json!({ "type": "playlist:updated", "playlistId": id, "deleted": false })
    );
    assert!(event.is_for(e.ana_account, e.ana_profile));
    assert!(rx.try_recv().is_err(), "exactly one");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sibling_profile_cannot_see_or_save(pool: PgPool) {
    let e = env(pool.clone()).await;
    let ana = mixes(&e, &e.ana, None).await;
    assert!(ana.iter().any(|m| m["type"] == "top-tracks"));
    // The sibling profile has no plays: no Top Tracks mix of its own, and ana's is not its.
    let kid = mixes(&e, &e.kid, None).await;
    assert!(kid.iter().all(|m| m["type"] != "top-tracks"), "{kid:?}");
    let res = e
        .server
        .get("/api/mixes/top-tracks")
        .authorization_bearer(&e.kid)
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    save(&e, &e.kid, "top-tracks", json!({}))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // Ana's saved mix does not block the same mix id for another profile that has it.
    save(&e, &e.ana, "top-tracks", json!({}))
        .await
        .assert_status_ok();
    let era = mix_with(&ana, "era")["id"].as_str().unwrap().to_owned();
    let ben = mixes(&e, &e.ben, None).await;
    if ben.iter().any(|m| m["id"] == json!(era)) {
        save(&e, &e.ben, &era, json!({})).await.assert_status_ok();
        save(&e, &e.ana, &era, json!({})).await.assert_status_ok();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mix_track_rows_carry_every_upstream_column(pool: PgPool) {
    use kima_db::vibe::MATCHING_SOUND;
    let e = env(pool.clone()).await;
    let list = mixes(&e, &e.ana, None).await;
    let top = mix_with(&list, "top-tracks");
    let id = top["id"].as_str().unwrap().to_owned();
    let first: Uuid = top["trackIds"][0].as_str().unwrap().parse().unwrap();
    // The fifth song is on another album (Album 1).
    let second: Uuid = top["trackIds"][4].as_str().unwrap().parse().unwrap();

    // The first song: analysis done with a current row, a current CLAP reading, an edited title, an ISRC, an album
    // with a user cover, an edited year and genres, an artist with an MBID; the embedding stage still working.
    let mut v = vec![0.0f32; 512];
    v[0] = 1.0;
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding)
         VALUES ($1, 0, $2, $3, $4, $5::real[]::vector)",
    )
    .bind(first)
    .bind(MATCHING_SOUND.model_id)
    .bind(MATCHING_SOUND.model_sha256.to_vec())
    .bind(MATCHING_SOUND.pipeline_version)
    .bind(v)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)
         SELECT track_id, created_at, 0.25, 0.5, 0.75, -0.5 FROM track_embeddings WHERE track_id = $1",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_analysis (track_id, file_generation,
             dsp_loudness_pipeline_version, integrated_lufs, loudness_range_lu, loudness_computed_at,
             dsp_key_pipeline_version, key_tonic, key_mode, key_strength, key_computed_at,
             dsp_tempo_pipeline_version, tempo_bpm, tempo_confidence, tempo_computed_at)
         VALUES ($1, 0, 'l1', -9.5, 6.25, now(), 'k1', 3, 'minor', 0.5, now(), 't1', 120.0, 0.5, now())",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, attempts, last_error, started_at, finished_at, updated_at)
         VALUES ($1, 'analysis', 'done', 1, 'old', '2026-11-18T10:00:00Z', '2026-11-18T10:01:00Z', '2026-11-18T10:01:00Z'),
                ($1, 'embedding', 'working', 2, 'slow', '2026-11-18T11:00:00Z', NULL, '2026-11-18T11:00:30Z')",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE tracks SET display_title = 'Edited', isrc = 'USABC1234567', isrc_source = 'id3',
             mood_tags = '{calm}' WHERE id = $1",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE albums SET image_path = 'x.jpg', image_source = 'user', display_year = 1999, user_genres = '{Rock}',
             label = 'Lbl', release_group_mbid = '5b11f4ce-a62d-471e-81fc-a69a8278c7da', release_group_mbid_source = 'tag'
         WHERE id = (SELECT album_id FROM tracks WHERE id = $1)",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE artists SET mbid = 'a74b1b7f-71a5-4011-9441-d0b5e4122711'
         WHERE id = (SELECT al.artist_id FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.id = $1)",
    )
    .bind(first)
    .execute(&pool)
    .await
    .unwrap();

    let res = e
        .server
        .get(&format!("/api/mixes/{id}"))
        .authorization_bearer(&e.ana)
        .await;
    res.assert_status_ok();
    let text = res.text();
    let body: Value = serde_json::from_str(&text).unwrap();
    let rows = body["tracks"].as_array().unwrap();
    let mut track_keys: BTreeSet<&str> = TRACK_KEYS.into_iter().collect();
    track_keys.insert("album");
    let mut album_keys: BTreeSet<&str> = ALBUM_KEYS.into_iter().collect();
    album_keys.insert("artist");
    for row in rows {
        assert_eq!(key_set(row), track_keys, "{row}");
        assert_eq!(key_set(&row["album"]), album_keys);
        assert_eq!(
            key_set(&row["album"]["artist"]),
            BTreeSet::from(["id", "name", "mbid"])
        );
    }
    // Upstream's model order: the track's keys, then `album` with its keys, then its `artist`.
    let mut cursor = text.find("\"tracks\":").unwrap();
    for _ in rows {
        cursor = assert_in_order(&text, cursor, &TRACK_KEYS);
        cursor = assert_in_order(&text, cursor, &["album"]);
        cursor = assert_in_order(&text, cursor, &ALBUM_KEYS);
        cursor = assert_in_order(&text, cursor, &["artist", "id", "name", "mbid"]);
    }
    let a = &rows[0];
    assert_eq!(a["id"], json!(first.to_string()));
    assert_eq!(a["title"], "Song 0-0");
    assert_eq!(a["trackNo"], 1);
    assert_eq!(a["duration"], 180);
    assert_eq!(a["mime"], Value::Null);
    assert_eq!(a["filePath"], "Artist 0/Album 0/1-1 Song 0-0.mp3");
    assert!(a["fileModified"].is_string());
    assert_eq!(a["fileSize"], 1);
    assert_eq!(a["bpm"], 120.0);
    assert!((a["bpmConfidence"].as_f64().unwrap() - 0.5 * 5.32).abs() < 1e-9);
    assert_eq!(a["beatsCount"], Value::Null);
    assert_eq!(a["key"], "Eb");
    assert_eq!(a["keyScale"], "minor");
    assert_eq!(a["keyStrength"], 0.5);
    assert_eq!(a["energy"], Value::Null);
    assert_eq!(a["loudness"], -9.5);
    assert_eq!(a["dynamicRange"], 6.25);
    assert_eq!(a["danceability"], Value::Null);
    assert_eq!(a["clapValenceRaw"], 0.75);
    assert_eq!(a["clapArousalRaw"], -0.5);
    // Upstream `syncSharedValenceArousal` falls back to the CLAP raw pair when there is no DEAM answer (D-264).
    assert_eq!(a["valence"], 0.75);
    assert_eq!(a["arousal"], -0.5);
    assert_eq!(a["deamValence"], Value::Null);
    assert_eq!(a["deamArousal"], Value::Null);
    assert_eq!(a["instrumentalness"], 0.25);
    assert_eq!(a["acousticness"], 0.5);
    assert_eq!(a["moodTags"], json!(["calm"]));
    assert_eq!(a["essentiaGenres"], Value::Null);
    assert_eq!(a["analysisStatus"], "completed");
    assert_eq!(a["analyzedAt"], "2026-11-18T10:01:00Z");
    assert_eq!(a["analysisError"], "old");
    assert_eq!(a["analysisRetryCount"], 1);
    assert_eq!(a["analysisStartedAt"], Value::Null, "only while running");
    assert!(a["updatedAt"].is_string());
    assert_eq!(a["displayTitle"], "Edited");
    assert_eq!(a["hasUserOverrides"], true);
    assert_eq!(a["vibeAnalysisStatus"], "processing");
    assert_eq!(a["vibeAnalysisStartedAt"], "2026-11-18T11:00:00Z");
    assert_eq!(a["vibeAnalysisError"], "slow");
    assert_eq!(a["vibeAnalysisRetryCount"], 2);
    assert_eq!(a["vibeAnalysisStatusUpdatedAt"], "2026-11-18T11:00:30Z");
    assert_eq!(a["isrc"], "USABC1234567");
    assert_eq!(a["isrcSource"], "id3");
    assert_eq!(a["acoustidFingerprint"], Value::Null);
    assert_eq!(a["corrupt"], false);
    assert_eq!(a["missingSince"], Value::Null);
    assert_eq!(a["discNumber"], 1);
    assert_eq!(a["discSubtitle"], Value::Null);
    let al = &a["album"];
    let album_id = al["id"].as_str().unwrap();
    assert_eq!(a["albumId"], al["id"]);
    assert_eq!(al["rgMbid"], "5b11f4ce-a62d-471e-81fc-a69a8278c7da");
    assert_eq!(al["rgMbidSource"], "tag");
    assert_eq!(al["title"], "Album 0");
    assert_eq!(al["sortName"], "album 0");
    assert_eq!(al["year"], 2005);
    assert_eq!(al["coverUrl"], json!(album_id));
    assert_eq!(al["userCoverUrl"], json!(album_id));
    assert_eq!(al["label"], "Lbl");
    assert_eq!(al["location"], "LIBRARY");
    assert_eq!(al["displayYear"], 1999);
    assert_eq!(al["hasUserOverrides"], true);
    assert_eq!(al["userGenres"], json!(["Rock"]));
    assert_eq!(al["primaryType"], Value::Null);
    assert_eq!(al["artistId"], al["artist"]["id"]);
    assert_eq!(al["artist"]["name"], "Artist 0");
    assert_eq!(al["artist"]["mbid"], "a74b1b7f-71a5-4011-9441-d0b5e4122711");

    // The fifth song has no analysis, readings or stage rows, its album no cover: every such key is null.
    let b = &rows[4];
    assert_eq!(b["id"], json!(second.to_string()));
    for k in [
        "bpm",
        "bpmConfidence",
        "key",
        "keyScale",
        "keyStrength",
        "loudness",
        "dynamicRange",
        "clapValenceRaw",
        "clapArousalRaw",
        "valence",
        "arousal",
        "instrumentalness",
        "acousticness",
        "analysisStatus",
        "analyzedAt",
        "analysisError",
        "analysisRetryCount",
        "vibeAnalysisStatus",
        "vibeAnalysisRetryCount",
        "displayTitle",
        "isrc",
    ] {
        assert_eq!(b[k], Value::Null, "{k}");
    }
    assert_eq!(b["moodTags"], json!([]));
    assert_eq!(b["hasUserOverrides"], false);
    assert_eq!(b["album"]["coverUrl"], Value::Null);
    assert_eq!(b["album"]["userCoverUrl"], Value::Null);
    assert_eq!(b["album"]["hasUserOverrides"], false);
    assert_eq!(b["album"]["artist"]["mbid"], Value::Null);
}
