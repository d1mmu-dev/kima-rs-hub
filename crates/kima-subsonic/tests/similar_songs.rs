//! `getSimilarSongs` / `getSimilarSongs2` (M5 Task 19, owner decision Q8, D-190).
//!
//! A track seed with current matching data (verified feel probe, built matching state, an
//! eligible `MATCHING_SOUND` vector) is ranked by the vibe formula (`rank_matches`, as
//! `/api/vibe/similar`); a short ranking is never topped up. Every other seed, and an eligible
//! seed whose ranking is empty (owner, Q8 option 1), takes upstream's artist-based path
//! (`routes/subsonic/search.ts:350-405`): the seed's (album) artist plus its stored similar
//! artists that are in the library, random order, `count` songs.

mod common;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_api::AppState;
use kima_db::PgPool;
use kima_db::vibe::MATCHING_SOUND;
use kima_db::vibe::rank::{WeightedInput, matching_context, rank_matches};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use serde_json::Value;
use uuid::Uuid;

/// Both endpoints and the element each answers in.
const ENDPOINTS: [(&str, &str); 2] = [
    ("getSimilarSongs", "similarSongs"),
    ("getSimilarSongs2", "similarSongs2"),
];

/// Present fixture tracks by album artist: 1 The Beatles, 2 La Roux, 3 Various Artists.
/// (Theatre's only track, 20, is missing.)
const BEATLES: [u128; 9] = [1, 2, 3, 4, 5, 6, 7, 8, 9];
const LA_ROUX: [u128; 6] = [10, 11, 12, 17, 18, 19];
const VARIOUS: [u128; 4] = [13, 14, 15, 16];

fn probe() -> Arc<FeelProbe> {
    let axis = || ProbeAxis {
        alpha: 1.0,
        bias: 0.5,
        weights: vec![0.0; 512],
    };
    Arc::new(FeelProbe {
        model: "synthetic".into(),
        probe_id: "subsonic-similar-probe".into(),
        valence: axis(),
        arousal: axis(),
    })
}

/// The fixture state, with the synthetic feel probe installed when `with_probe`.
fn state(pool: PgPool, with_probe: bool) -> AppState {
    let state = fixture_state(pool);
    let ctx = state.ident_ctx();
    vibe_assets::install(
        &ctx,
        VibeAssets {
            probe: with_probe.then(probe),
            heads: None,
            report: kima_ml::CapabilityEvaluationReport {
                capabilities: Default::default(),
            },
        },
    );
    state.with_ident(ctx)
}

fn app(state: AppState) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    TestServer::new(kima_subsonic::router(state).layer(MockConnectInfo(peer))).unwrap()
}

async fn alice(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> Value {
    let mut all = vec![("apiKey", FIXTURE_TOKEN)];
    all.extend_from_slice(params);
    rest(server, endpoint, &all).await
}

async fn admin(server: &TestServer, endpoint: &str, params: &[(&str, &str)]) -> Value {
    let mut all = vec![("u", ADMIN_NAME), ("p", ADMIN_PASSWORD)];
    all.extend_from_slice(params);
    rest(server, endpoint, &all).await
}

/// The song ids of a successful answer, in order.
fn songs(res: &Value, key: &str) -> Vec<String> {
    assert_eq!(res["status"], "ok", "{res}");
    res[key]["song"]
        .as_array()
        .unwrap_or_else(|| panic!("no song list in {res}"))
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect()
}

fn set(ids: &[String]) -> BTreeSet<String> {
    ids.iter().cloned().collect()
}

fn tracks(ns: &[u128]) -> BTreeSet<String> {
    ns.iter().map(|&n| track_id(n).to_string()).collect()
}

/// Calls both endpoints as alice and returns each answer's ids, checking they agree as sets.
async fn both(server: &TestServer, params: &[(&str, &str)]) -> Vec<String> {
    let mut first: Option<Vec<String>> = None;
    for (endpoint, key) in ENDPOINTS {
        let ids = songs(&alice(server, endpoint, params).await, key);
        match &first {
            None => first = Some(ids),
            Some(f) => assert_eq!(set(f), set(&ids), "{endpoint} disagrees"),
        }
    }
    first.unwrap()
}

/// Adds `n` present tracks to album 1 (The Beatles).
async fn seed_beatles(pool: &PgPool, n: i32) {
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         SELECT gen_random_uuid(), '00000000-0000-7000-8000-200000000001', 'The Beatles',
                'Extra ' || lpad(i::text, 5, '0'), 'mp3', 'extra/' || i || '.mp3', 1000, now(),
                decode(md5(i::text) || md5(i::text), 'hex')
         FROM generate_series(1, $1) i",
    )
    .bind(n)
    .execute(pool)
    .await
    .unwrap();
}

/// One eligible vector per fixture track (missing track 20 included) on a circle, then the
/// matching state for the synthetic probe.
async fn embed_fixture(pool: &PgPool, model_id: &str) {
    for n in 1..=20u128 {
        let angle = n as f32 * 0.15;
        let mut v = vec![0.0f32; 512];
        v[0] = angle.cos();
        v[1] = angle.sin();
        sqlx::query(
            "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256,
                                           pipeline_version, embedding)
             VALUES ($1, 0, $2, $3, $4, $5::real[]::vector)",
        )
        .bind(track_id(n))
        .bind(model_id)
        .bind(MATCHING_SOUND.model_sha256.to_vec())
        .bind(MATCHING_SOUND.pipeline_version)
        .bind(v)
        .execute(pool)
        .await
        .unwrap();
    }
    kima_db::vibe::state::build_matching(pool, &probe())
        .await
        .unwrap();
}

/// What the vibe engine ranks for `seed` (the order `/api/vibe/similar` sends).
async fn ranked(pool: &PgPool, seed: Uuid, limit: i64) -> Vec<String> {
    let ctx = matching_context(pool, &probe(), &[seed])
        .await
        .unwrap()
        .expect("matching state built");
    rank_matches(
        pool,
        &ctx,
        &[WeightedInput {
            track_id: seed,
            weight: 1.0,
        }],
        limit,
        &[seed],
    )
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.track.id.to_string())
    .collect()
}

macro_rules! fixture_test {
    ($name:ident, |$pool:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "fixtures", scripts("subsonic"))
        )]
        async fn $name($pool: PgPool) {
            $body
        }
    };
}

// ---------------------------------------------------------------------------------------------
// The artist-based path (upstream), used whenever the seed has no current matching data.

fixture_test!(missing_id_is_error_10_required_parameter, |pool| {
    let server = app(state(pool, true));
    for (endpoint, _) in ENDPOINTS {
        for params in [&[][..], &[("id", "")][..]] {
            let res = alice(&server, endpoint, params).await;
            assert_eq!(res["status"], "failed", "{res}");
            assert_eq!(res["error"]["code"], 10, "{res}");
            assert_eq!(
                res["error"]["message"], "Required parameter is missing: id",
                "{res}"
            );
        }
    }
});

fixture_test!(count_default_50_clamped_1_500, |pool| {
    seed_beatles(&pool, 600).await;
    let server = app(state(pool, false));
    let seed = track_id(1).to_string();
    for (count, expected) in [
        (None, 50),
        (Some("0"), 1),
        (Some("-7"), 1),
        (Some("1"), 1),
        (Some("20"), 20),
        (Some("abc"), 50),
        (Some("9999"), 500),
    ] {
        let mut params = vec![("id", seed.as_str())];
        if let Some(c) = count {
            params.push(("count", c));
        }
        for (endpoint, key) in ENDPOINTS {
            let ids = songs(&alice(&server, endpoint, &params).await, key);
            assert_eq!(ids.len(), expected, "{endpoint} count={count:?}");
        }
    }
});

fixture_test!(song_id_resolves_to_its_album_artist, |pool| {
    let server = app(state(pool.clone(), false));
    // The seed itself is one of its artist's songs (upstream does not exclude it).
    let ids = both(&server, &[("id", &track_id(1).to_string())]).await;
    assert_eq!(set(&ids), tracks(&BEATLES));
    // A compilation track credited to Kesha resolves to the album artist, Various Artists.
    let ids = both(&server, &[("id", &track_id(13).to_string())]).await;
    assert_eq!(set(&ids), tracks(&VARIOUS));
    // A missing track still names its album artist; only present songs come back.
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track_id(2))
        .execute(&pool)
        .await
        .unwrap();
    let ids = both(&server, &[("id", &track_id(2).to_string())]).await;
    assert_eq!(set(&ids), tracks(&[1, 3, 4, 5, 6, 7, 8, 9]));
});

fixture_test!(album_id_and_artist_id_resolve, |pool| {
    let server = app(state(pool, false));
    for id in [album_id(3), album_id(5), artist_id(2)] {
        let ids = both(&server, &[("id", &id.to_string())]).await;
        assert_eq!(set(&ids), tracks(&LA_ROUX), "{id}");
    }
    let ids = both(&server, &[("id", &album_id(2).to_string())]).await;
    assert_eq!(set(&ids), tracks(&BEATLES));
});

fixture_test!(unknown_id_answers_empty_container, |pool| {
    let server = app(state(pool, false));
    let unknown = Uuid::now_v7().to_string();
    // Upstream looks the raw id up: prefixed forms and non-UUIDs are unknown too.
    let prefixed = format!("ar-{}", artist_id(1));
    for id in [unknown.as_str(), "whatever", prefixed.as_str()] {
        for (endpoint, key) in ENDPOINTS {
            let res = alice(&server, endpoint, &[("id", id)]).await;
            assert_eq!(res["status"], "ok", "{res}");
            assert_eq!(res[key]["song"], serde_json::json!([]), "{endpoint} {id}");
            let xml = rest_xml(&server, endpoint, &[("apiKey", FIXTURE_TOKEN), ("id", id)]).await;
            assert!(xml.contains(&format!("<{key}/>")), "{xml}");
        }
    }
});

fixture_test!(
    seed_artist_plus_similar_artists_with_library_albums,
    |pool| {
        // La Roux (in the library), Theatre (only a missing track: no library album) and an
        // artist the library does not have.
        sqlx::query(
            r#"UPDATE artists SET "similar" = '[{"name":"Nobody Here","mbid":null,"match":0.99},
                                            {"name":"theatre","mbid":null,"match":0.95},
                                            {"name":"La Roux","mbid":null,"match":0.4}]'
           WHERE id = $1"#,
        )
        .bind(artist_id(1))
        .execute(&pool)
        .await
        .unwrap();
        let server = app(state(pool.clone(), false));
        let ids = both(
            &server,
            &[("id", &track_id(1).to_string()), ("count", "500")],
        )
        .await;
        let expected: BTreeSet<String> =
            tracks(&BEATLES).union(&tracks(&LA_ROUX)).cloned().collect();
        assert_eq!(set(&ids), expected);
        // The relation is one-way: La Roux names nobody.
        let ids = both(
            &server,
            &[("id", &track_id(10).to_string()), ("count", "500")],
        )
        .await;
        assert_eq!(set(&ids), tracks(&LA_ROUX));
    }
);

fixture_test!(similar_artists_capped_at_50_by_match, |pool| {
    // 60 library artists, one present track each, similar to the Beatles with match i/100,
    // listed in ascending match so the stored order is not the ranking.
    sqlx::query(
        "INSERT INTO artists (id, name, name_key)
         SELECT ('00000000-0000-7000-8000-c' || lpad(i::text, 11, '0'))::uuid,
                'Similar ' || i, 'similar ' || i
         FROM generate_series(1, 60) i",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key)
         SELECT ('00000000-0000-7000-8000-d' || lpad(i::text, 11, '0'))::uuid,
                ('00000000-0000-7000-8000-c' || lpad(i::text, 11, '0'))::uuid,
                'Album ' || i, 'album ' || i
         FROM generate_series(1, 60) i",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         SELECT ('00000000-0000-7000-8000-e' || lpad(i::text, 11, '0'))::uuid,
                ('00000000-0000-7000-8000-d' || lpad(i::text, 11, '0'))::uuid,
                'Similar ' || i, 'Song ' || i, 'flac', 'Similar/' || i || '.flac', 1000, now(),
                sha256(('similar-fp' || i)::bytea)
         FROM generate_series(1, 60) i",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE artists SET "similar" = (
               SELECT jsonb_agg(jsonb_build_object('name', 'Similar ' || i, 'mbid', NULL,
                                                   'match', i / 100.0) ORDER BY i)
               FROM generate_series(1, 60) i)
           WHERE id = $1"#,
    )
    .bind(artist_id(1))
    .execute(&pool)
    .await
    .unwrap();
    let server = app(state(pool, false));
    let ids = both(
        &server,
        &[("id", &artist_id(1).to_string()), ("count", "500")],
    )
    .await;
    let similar: BTreeSet<String> = (11..=60u128)
        .map(|i| Uuid::from_u128(0x0000_0000_0000_7000_8000_e000_0000_0000 | dec(i)).to_string())
        .collect();
    let expected: BTreeSet<String> = tracks(&BEATLES).union(&similar).cloned().collect();
    assert_eq!(set(&ids), expected);
});

/// `n` in decimal digits, one per nibble (the fixture's id scheme).
fn dec(mut n: u128) -> u128 {
    let (mut out, mut shift) = (0, 0);
    while n > 0 {
        out |= (n % 10) << shift;
        shift += 4;
        n /= 10;
    }
    out
}

fixture_test!(only_present_playable_songs, |pool| {
    let server = app(state(pool.clone(), false));
    // Theatre's only track is missing: its artist and album exist but have no songs.
    for id in [artist_id(4), album_id(6), track_id(20)] {
        assert!(
            both(&server, &[("id", &id.to_string())]).await.is_empty(),
            "{id}"
        );
    }
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = ANY($1)")
        .bind(vec![track_id(3), track_id(7)])
        .execute(&pool)
        .await
        .unwrap();
    let ids = both(&server, &[("id", &artist_id(1).to_string())]).await;
    assert_eq!(set(&ids), tracks(&[1, 2, 4, 5, 6, 8, 9]));
});

fixture_test!(order_is_random_limit_count, |pool| {
    seed_beatles(&pool, 200).await;
    let server = app(state(pool, false));
    let seed = track_id(1).to_string();
    let mut orders = BTreeSet::new();
    for _ in 0..4 {
        for (endpoint, key) in ENDPOINTS {
            let ids = songs(
                &alice(&server, endpoint, &[("id", &seed), ("count", "30")]).await,
                key,
            );
            assert_eq!(ids.len(), 30);
            assert_eq!(set(&ids).len(), 30, "no duplicates");
            orders.insert(ids);
        }
    }
    // Eight draws of 30 out of 209 songs: equal orders would mean no randomness.
    assert!(orders.len() > 1, "{orders:?}");
});

fixture_test!(no_vibe_engine_dependency, |pool| {
    // Zero embeddings, no feel probe, no matching state: the artist-based answer.
    let server = app(state(pool.clone(), false));
    let ids = both(&server, &[("id", &track_id(10).to_string())]).await;
    assert_eq!(set(&ids), tracks(&LA_ROUX));
    // The probe alone (still no vectors, no state) changes nothing.
    let server = app(state(pool, true));
    let ids = both(&server, &[("id", &track_id(10).to_string())]).await;
    assert_eq!(set(&ids), tracks(&LA_ROUX));
});

// ---------------------------------------------------------------------------------------------
// The vibe path (Q8): a track seed with current matching data.

fixture_test!(vibe_seed_is_ranked_by_rank_matches_seed_excluded, |pool| {
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let server = app(state(pool.clone(), true));
    let seed = track_id(1);
    let expected = ranked(&pool, seed, 50).await;
    assert!(expected.len() > 4, "{expected:?}");
    // Not the artist-based answer: other artists come back, ranked.
    assert!(!set(&expected).is_subset(&tracks(&BEATLES)), "{expected:?}");
    for (endpoint, key) in ENDPOINTS {
        let ids = songs(
            &alice(&server, endpoint, &[("id", &seed.to_string())]).await,
            key,
        );
        assert_eq!(ids, expected, "{endpoint}");
        assert!(!ids.contains(&seed.to_string()));
        // Repeated calls give the same order (no randomness on this path).
        let again = songs(
            &alice(&server, endpoint, &[("id", &seed.to_string())]).await,
            key,
        );
        assert_eq!(again, ids);
    }
    // The song shape is the one every song list uses (D-082): title, album, artist, path.
    let res = alice(&server, "getSimilarSongs2", &[("id", &seed.to_string())]).await;
    let first = &res["similarSongs2"]["song"][0];
    let one = alice(&server, "getSong", &[("id", first["id"].as_str().unwrap())]).await;
    assert_eq!(first, &one["song"]);
});

fixture_test!(vibe_count_default_50_clamped_1_500, |pool| {
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let server = app(state(pool.clone(), true));
    let seed = track_id(10);
    let all = ranked(&pool, seed, 500).await;
    for (count, limit) in [
        (None, 50),
        (Some("0"), 1),
        (Some("-3"), 1),
        (Some("2"), 2),
        (Some("9999"), 500),
    ] {
        let mut params = vec![("id", seed.to_string())];
        if let Some(c) = count {
            params.push(("count", c.to_owned()));
        }
        let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let want: Vec<String> = all.iter().take(limit).cloned().collect();
        for (endpoint, key) in ENDPOINTS {
            let ids = songs(&alice(&server, endpoint, &params).await, key);
            assert_eq!(ids, want, "{endpoint} count={count:?}");
        }
    }
});

fixture_test!(vibe_answers_only_present_playable_songs, |pool| {
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let server = app(state(pool.clone(), true));
    let seed = track_id(1);
    let before = ranked(&pool, seed, 50).await;
    let gone = before[0].clone();
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1::uuid")
        .bind(&gone)
        .execute(&pool)
        .await
        .unwrap();
    let ids = both(&server, &[("id", &seed.to_string())]).await;
    assert!(!ids.contains(&gone), "{ids:?}");
    assert!(
        !ids.contains(&track_id(20).to_string()),
        "missing fixture track"
    );
    assert_eq!(ids, ranked(&pool, seed, 50).await);
});

fixture_test!(vibe_seed_without_eligible_embedding_falls_back, |pool| {
    // Vectors of another sound identity (today's hf_audio) are never eligible.
    embed_fixture(&pool, "hf_audio-clap-htsat-unfused").await;
    let server = app(state(pool.clone(), true));
    let ids = both(&server, &[("id", &track_id(1).to_string())]).await;
    assert_eq!(set(&ids), tracks(&BEATLES));
});

fixture_test!(vibe_needs_the_probe_and_a_track_seed, |pool| {
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    // Without the verified feel probe the seed has no matching data: artist-based.
    let server = app(state(pool.clone(), false));
    let ids = both(&server, &[("id", &track_id(1).to_string())]).await;
    assert_eq!(set(&ids), tracks(&BEATLES));
    // Album and artist ids always take the artist-based path.
    let server = app(state(pool.clone(), true));
    for id in [album_id(3), artist_id(2)] {
        let ids = both(&server, &[("id", &id.to_string())]).await;
        assert_eq!(set(&ids), tracks(&LA_ROUX), "{id}");
    }
    // A missing seed track has no eligible vector: artist-based, present songs only.
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track_id(2))
        .execute(&pool)
        .await
        .unwrap();
    let ids = both(&server, &[("id", &track_id(2).to_string())]).await;
    assert_eq!(set(&ids), tracks(&[1, 3, 4, 5, 6, 7, 8, 9]));
});

fixture_test!(vibe_seed_with_empty_ranking_falls_back, |pool| {
    // Owner decision Q8 option 1: the seed keeps its eligible vector (and the built state),
    // but nothing else is eligible, so the vibe ranking is empty: artist-based answer.
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let seed = track_id(1);
    sqlx::query("DELETE FROM track_embeddings WHERE track_id <> $1")
        .bind(seed)
        .execute(&pool)
        .await
        .unwrap();
    assert!(ranked(&pool, seed, 500).await.is_empty());
    let server = app(state(pool.clone(), true));
    let ids = both(&server, &[("id", &seed.to_string())]).await;
    assert_eq!(set(&ids), tracks(&BEATLES));
});

fixture_test!(vibe_short_ranking_is_not_topped_up, |pool| {
    // Partial analysis: only La Roux and the seed have vectors. The short ranking is the whole
    // answer; no artist-based songs are added to reach `count`.
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let seed = track_id(1);
    let la_roux: Vec<Uuid> = LA_ROUX.iter().map(|&n| track_id(n)).collect();
    sqlx::query("DELETE FROM track_embeddings WHERE track_id <> $1 AND track_id <> ALL($2)")
        .bind(seed)
        .bind(&la_roux)
        .execute(&pool)
        .await
        .unwrap();
    let expected = ranked(&pool, seed, 500).await;
    assert!(!expected.is_empty() && expected.len() < 50, "{expected:?}");
    let server = app(state(pool.clone(), true));
    let seed = seed.to_string();
    for (endpoint, key) in ENDPOINTS {
        for params in [
            vec![("id", seed.as_str())],
            vec![("id", &seed), ("count", "500")],
        ] {
            let ids = songs(&alice(&server, endpoint, &params).await, key);
            assert_eq!(ids, expected, "{endpoint} {params:?}");
        }
    }
});

fixture_test!(
    vibe_caps_count_the_seed_two_per_album_four_per_artist,
    |pool| {
        // The seed (track 1, Abbey Road, The Beatles) takes one of its album's two places and one
        // of its artist's four, as `/api/vibe/similar` counts it.
        embed_fixture(&pool, MATCHING_SOUND.model_id).await;
        let server = app(state(pool, true));
        let abbey_road = tracks(&[2, 3, 4, 5]);
        let beatles = tracks(&BEATLES);
        for (endpoint, key) in ENDPOINTS {
            let ids = songs(
                &alice(
                    &server,
                    endpoint,
                    &[("id", &track_id(1).to_string()), ("count", "500")],
                )
                .await,
                key,
            );
            let same_album = ids.iter().filter(|id| abbey_road.contains(*id)).count();
            let same_artist = ids.iter().filter(|id| beatles.contains(*id)).count();
            assert_eq!(same_album, 1, "{endpoint} {ids:?}");
            assert_eq!(same_artist, 3, "{endpoint} {ids:?}");
        }
    }
);

fixture_test!(vibe_ids_match_the_http_api_similar_route, |pool| {
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let state = state(pool.clone(), true);
    let server = app(state.clone());
    let admin_user = kima_db::users::find_by_id(&pool, ADMIN_ID)
        .await
        .unwrap()
        .unwrap();
    let jwt = state.jwt.issue_access(
        admin_user.account(),
        admin_user.main_profile(),
        &admin_user.username,
        &admin_user.role,
        admin_user.token_version,
    );
    let api = TestServer::new(kima_api::router(state)).unwrap();
    for seed in [track_id(1), track_id(10), track_id(14)] {
        for limit in ["1", "5", "20", "50"] {
            let res = api
                .get(&format!("/api/vibe/similar/{seed}"))
                .add_query_param("limit", limit)
                .authorization_bearer(&jwt)
                .await;
            res.assert_status_ok();
            let body = res.json::<Value>();
            let http: Vec<String> = body["tracks"]
                .as_array()
                .unwrap_or_else(|| panic!("no tracks in {body}"))
                .iter()
                .map(|t| t["id"].as_str().unwrap().to_owned())
                .collect();
            assert!(!http.is_empty());
            for (endpoint, key) in ENDPOINTS {
                let ids = songs(
                    &alice(
                        &server,
                        endpoint,
                        &[("id", &seed.to_string()), ("count", limit)],
                    )
                    .await,
                    key,
                );
                assert_eq!(ids, http, "{endpoint} seed={seed} limit={limit}");
            }
        }
    }
});

fixture_test!(vibe_path_database_failure_is_error_0, |pool| {
    // The seed is eligible, so the vibe path runs and its read fails; the artist-based path
    // (which never reads `matching_feel`) must not hide the failure (`/api/vibe/similar`: 500).
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let server = app(state(pool.clone(), true));
    sqlx::query("ALTER TABLE matching_feel RENAME TO matching_feel_broken")
        .execute(&pool)
        .await
        .unwrap();
    for (endpoint, _) in ENDPOINTS {
        let res = alice(&server, endpoint, &[("id", &track_id(1).to_string())]).await;
        assert_eq!(res["status"], "failed", "{endpoint} {res}");
        assert_eq!(res["error"]["code"], 0, "{endpoint} {res}");
    }
    // The vibe path runs (and fails) before the artist-based one, for every kind of seed, while
    // the probe and the state exist: an album or artist id does not hide the failure either.
    for id in [album_id(3), artist_id(2)] {
        let res = alice(&server, "getSimilarSongs", &[("id", &id.to_string())]).await;
        assert_eq!(res["status"], "failed", "{id} {res}");
        assert_eq!(res["error"]["code"], 0, "{id} {res}");
    }
});

fixture_test!(vibe_list_is_library_global_with_the_callers_stars, |pool| {
    embed_fixture(&pool, MATCHING_SOUND.model_id).await;
    let server = app(state(pool.clone(), true));
    let seed = track_id(1);
    let top = ranked(&pool, seed, 1).await.remove(0);
    let starred = admin(&server, "star", &[("id", &top)]).await;
    assert_eq!(starred["status"], "ok", "{starred}");
    let seed = seed.to_string();
    let a = alice(
        &server,
        "getSimilarSongs2",
        &[("id", &seed), ("count", "500")],
    )
    .await;
    let b = admin(
        &server,
        "getSimilarSongs2",
        &[("id", &seed), ("count", "500")],
    )
    .await;
    assert_eq!(songs(&a, "similarSongs2"), songs(&b, "similarSongs2"));
    // Only the admin starred the best match.
    let find = |res: &Value| {
        res["similarSongs2"]["song"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == top.as_str())
            .cloned()
            .expect("best match listed")
    };
    assert!(find(&b).get("starred").is_some(), "{b}");
    assert!(find(&a).get("starred").is_none(), "{a}");
});
