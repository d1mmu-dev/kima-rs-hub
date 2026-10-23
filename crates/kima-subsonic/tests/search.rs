//! Search, random and top songs, genre and similar stubs on the SQL fixture (spec §4.3, §10.2).

mod common;

use std::collections::HashSet;
use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use serde_json::Value;

fn app(pool: PgPool) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool)).layer(MockConnectInfo(peer));
    TestServer::new(router).unwrap()
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

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect()
}

fn track_ids(ns: &[u128]) -> Vec<String> {
    ns.iter().map(|&n| track_id(n).to_string()).collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
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

fn assert_failed(res: &Value, code: u64) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
}

/// Adds `n` present tracks to album 1. The first `twins` share the title "Twin".
async fn seed_extra(pool: &PgPool, n: i32, twins: i32) {
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         SELECT gen_random_uuid(), '00000000-0000-7000-8000-200000000001', 'Extra Artist',
                CASE WHEN i <= $2 THEN 'Twin' ELSE 'Extra ' || lpad(i::text, 5, '0') END,
                'mp3', 'extra/' || i || '.mp3', 1000, now(),
                decode(md5(i::text) || md5(i::text), 'hex')
         FROM generate_series(1, $1) i",
    )
    .bind(n)
    .bind(twins)
    .execute(pool)
    .await
    .unwrap();
}

async fn present_track_ids(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, uuid::Uuid>("SELECT id FROM tracks WHERE missing_since IS NULL")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|u| u.to_string())
        .collect()
}

fixture_test!(search3_query_finds_all_kinds, |server, _pool| {
    let res = alice(&server, "search3", &[("query", "roux")]).await;
    assert_eq!(res["status"], "ok", "{res}");
    let result = &res["searchResult3"];
    assert_eq!(ids(&result["artist"]), [artist_id(2).to_string()]);
    assert_eq!(result["artist"][0]["name"], "La Roux");
    assert_eq!(result["artist"][0]["userRating"], 3);
    let albums = ids(&result["album"]);
    assert!(albums.contains(&album_id(3).to_string()), "{albums:?}");
    let songs = ids(&result["song"]);
    assert!(songs.contains(&track_id(11).to_string()), "{songs:?}");
    // The last term is a prefix.
    let res = alice(&server, "search3", &[("query", "bullet")]).await;
    assert_eq!(ids(&res["searchResult3"]["song"]), track_ids(&[11]));
    assert_eq!(res["searchResult3"]["artist"], serde_json::json!([]));
    // Starred and rated by the caller only.
    let res = alice(&server, "search3", &[("query", "come together")]).await;
    let song = &res["searchResult3"]["song"][0];
    assert_eq!(song["id"], track_id(1).to_string());
    assert_eq!(song["userRating"], 5);
    assert_eq!(song["starred"], "2026-01-01T04:01:00.000Z");
    let res = admin(&server, "search3", &[("query", "come together")]).await;
    let song = &res["searchResult3"]["song"][0];
    assert_eq!(song["userRating"], 1);
    assert!(song.get("starred").is_none());
    // Nothing matches; missing tracks are hidden.
    let res = alice(&server, "search3", &[("query", "curtain")]).await;
    assert_eq!(res["searchResult3"]["song"], serde_json::json!([]));
    let xml = rest_xml(
        &server,
        "search3",
        &[("apiKey", FIXTURE_TOKEN), ("query", "bulletproof")],
    )
    .await;
    assert!(xml.contains("<searchResult3><song id="), "{xml}");
});

fixture_test!(search3_counts_default_20_capped_500, |server, pool| {
    seed_extra(&pool, 600, 0).await;
    let n = |res: &Value, key: &str| res["searchResult3"][key].as_array().unwrap().len();
    // 619 present songs; the default page is 20.
    let res = alice(&server, "search3", &[("query", "")]).await;
    assert_eq!(n(&res, "song"), 20);
    assert_eq!(n(&res, "album"), 5);
    assert_eq!(n(&res, "artist"), 3);
    let res = alice(&server, "search3", &[("songCount", "9999")]).await;
    assert_eq!(n(&res, "song"), 500);
    let res = alice(
        &server,
        "search3",
        &[("songCount", "0"), ("albumCount", "2")],
    )
    .await;
    assert_eq!(n(&res, "song"), 0);
    assert_eq!(n(&res, "album"), 2);
    // The same limits apply to a text query.
    let res = alice(
        &server,
        "search3",
        &[("query", "extra"), ("songCount", "9999")],
    )
    .await;
    assert_eq!(n(&res, "song"), 500);
    let res = alice(&server, "search3", &[("query", "extra")]).await;
    assert_eq!(n(&res, "song"), 20);
    let res = alice(
        &server,
        "search3",
        &[
            ("query", "extra"),
            ("songCount", "5"),
            ("songOffset", "598"),
        ],
    )
    .await;
    assert!(n(&res, "song") <= 5);
});

// Paging parameters fall back to the default on junk, as upstream's `parseIntParam` and
// Navidrome, instead of error 10 (M2 final review, ruling #19 revised).
fixture_test!(paging_junk_falls_back_to_defaults, |server, pool| {
    seed_extra(&pool, 600, 0).await;
    let n = |res: &Value, key: &str| res["searchResult3"][key].as_array().unwrap().len();
    let res = alice(
        &server,
        "search3",
        &[
            ("query", ""),
            ("songCount", "abc"),
            ("albumCount", "10.5"),
            ("artistCount", " "),
            ("songOffset", "x"),
            ("albumOffset", "1e3"),
            ("artistOffset", "--1"),
        ],
    )
    .await;
    assert_eq!(res["status"], "ok", "{res}");
    assert_eq!(n(&res, "song"), 20);
    assert_eq!(n(&res, "album"), 5);
    assert_eq!(n(&res, "artist"), 3);
    let first = alice(&server, "search3", &[("query", "")]).await;
    assert_eq!(
        res["searchResult3"], first["searchResult3"],
        "offsets default to 0"
    );
    // A surrounding space is still a number.
    let res = alice(&server, "search3", &[("songCount", " 7 ")]).await;
    assert_eq!(n(&res, "song"), 7);

    let res = alice(
        &server,
        "search",
        &[("any", "extra"), ("count", "x"), ("offset", "y")],
    )
    .await;
    assert_eq!(res["status"], "ok", "{res}");
    assert_eq!(res["searchResult"]["match"].as_array().unwrap().len(), 20);
    assert_eq!(res["searchResult"]["offset"], 0);

    let res = alice(&server, "getRandomSongs", &[("size", "lots")]).await;
    assert_eq!(res["randomSongs"]["song"].as_array().unwrap().len(), 10);
    let res = alice(
        &server,
        "getTopSongs",
        &[("artist", "extra"), ("count", "x")],
    )
    .await;
    assert_eq!(res["status"], "ok", "{res}");

    // Numbers that change meaning stay strict.
    assert_failed(
        &alice(
            &server,
            "getRandomSongs",
            &[("size", "x"), ("fromYear", "abc")],
        )
        .await,
        10,
    );
});

fixture_test!(empty_query_pages_whole_library, |server, pool| {
    seed_extra(&pool, 23, 5).await;
    let expected = present_track_ids(&pool).await;
    assert_eq!(expected.len(), 19 + 23);
    let mut seen = Vec::new();
    let mut offset = 0;
    loop {
        let res = alice(
            &server,
            "search3",
            &[
                ("query", ""),
                ("artistCount", "0"),
                ("albumCount", "0"),
                ("songCount", "7"),
                ("songOffset", &offset.to_string()),
            ],
        )
        .await;
        let page = ids(&res["searchResult3"]["song"]);
        let short = page.len() < 7;
        offset += page.len();
        seen.extend(page);
        if short {
            break;
        }
    }
    assert_eq!(seen.len(), expected.len(), "gaps or repeats");
    let unique: HashSet<_> = seen.iter().cloned().collect();
    assert_eq!(unique.len(), seen.len(), "duplicates");
    assert_eq!(sorted(seen), sorted(expected));
    // Past the end.
    let res = alice(&server, "search3", &[("songOffset", "1000")]).await;
    assert_eq!(res["searchResult3"]["song"], serde_json::json!([]));
    // Albums and artists page the same way, missing-only ones left out.
    let res = alice(
        &server,
        "search3",
        &[("albumCount", "2"), ("albumOffset", "4")],
    )
    .await;
    assert_eq!(res["searchResult3"]["album"].as_array().unwrap().len(), 1);
    assert!(
        !ids(&res["searchResult3"]["album"]).contains(&album_id(6).to_string()),
        "{res}"
    );
});

fixture_test!(empty_query_variants, |server, _pool| {
    let base = alice(&server, "search3", &[("songCount", "500")]).await;
    let base_ids = ids(&base["searchResult3"]["song"]);
    assert_eq!(base_ids.len(), 19);
    assert!(!base_ids.contains(&track_id(20).to_string()));
    for q in ["", "''", "\"\"", " ", " '' "] {
        let res = alice(&server, "search3", &[("query", q), ("songCount", "500")]).await;
        assert_eq!(ids(&res["searchResult3"]["song"]), base_ids, "{q:?}");
    }
    // Sorted by title, ties by id: the two Intro tracks are adjacent and in id order.
    let at = |id: u128| base_ids.iter().position(|x| *x == track_id(id).to_string());
    assert_eq!(at(18).unwrap(), at(17).unwrap() + 1);
    // A text query is not the whole library.
    let res = alice(
        &server,
        "search3",
        &[("query", "intro"), ("songCount", "500")],
    )
    .await;
    assert_eq!(
        sorted(ids(&res["searchResult3"]["song"])),
        sorted(track_ids(&[17, 18]))
    );
});

fixture_test!(search2_same_under_search_result2, |server, _pool| {
    for params in [&[("query", "beatles")][..], &[("query", "")][..]] {
        let three = alice(&server, "search3", params).await;
        let two = alice(&server, "search2", params).await;
        assert_eq!(three["searchResult3"], two["searchResult2"]);
        assert!(three["searchResult3"]["song"].as_array().unwrap().len() > 1);
    }
});

fixture_test!(
    legacy_search_falls_back_any_title_album_artist,
    |server, _pool| {
        let matches = |res: &Value| ids(&res["searchResult"]["match"]);
        let res = alice(&server, "search", &[("query", "bulletproof")]).await;
        assert_eq!(matches(&res), track_ids(&[11]));
        assert_eq!(res["searchResult"]["offset"], 0);
        assert_eq!(res["searchResult"]["totalHits"], 1);
        assert_eq!(res["searchResult"]["match"][0]["title"], "Bulletproof");
        // query wins over any, any over title, and so on.
        let res = alice(
            &server,
            "search",
            &[("query", "bulletproof"), ("any", "kesha")],
        )
        .await;
        assert_eq!(matches(&res), track_ids(&[11]));
        let res = alice(
            &server,
            "search",
            &[("any", "kesha"), ("title", "quicksand")],
        )
        .await;
        assert_eq!(matches(&res), track_ids(&[13]));
        let res = alice(
            &server,
            "search",
            &[("title", "quicksand"), ("album", "abbey")],
        )
        .await;
        assert_eq!(matches(&res), track_ids(&[12]));
        let res = alice(
            &server,
            "search",
            &[("album", "abbey"), ("artist", "kesha")],
        )
        .await;
        assert_eq!(sorted(matches(&res)), sorted(track_ids(&[1, 2, 3, 4, 5])));
        let res = alice(&server, "search", &[("artist", "kesha")]).await;
        assert_eq!(matches(&res), track_ids(&[13]));
        // An empty `query` falls through to the next one.
        let res = alice(&server, "search", &[("query", ""), ("artist", "kesha")]).await;
        assert_eq!(matches(&res), track_ids(&[13]));
        // count and offset page the songs.
        let res = alice(
            &server,
            "search",
            &[("album", "abbey"), ("count", "2"), ("offset", "4")],
        )
        .await;
        assert_eq!(matches(&res).len(), 1);
        assert_eq!(res["searchResult"]["offset"], 4);
        // The page ends the results: no extra row, so the total is exact.
        assert_eq!(res["searchResult"]["totalHits"], 5);
        // More rows exist past the page: a lower bound, one past what is shown.
        let res = alice(
            &server,
            "search",
            &[("album", "abbey"), ("count", "2"), ("offset", "1")],
        )
        .await;
        assert_eq!(matches(&res).len(), 2);
        assert_eq!(res["searchResult"]["totalHits"], 4);
        let res = alice(&server, "search", &[("album", "abbey"), ("count", "5")]).await;
        assert_eq!(res["searchResult"]["totalHits"], 5);
        // No text at all lists the library, as the empty `search3` does.
        let res = alice(&server, "search", &[("count", "500")]).await;
        assert_eq!(matches(&res).len(), 19);
        let xml = rest_xml(
            &server,
            "search",
            &[("apiKey", FIXTURE_TOKEN), ("query", "bulletproof")],
        )
        .await;
        assert!(
            xml.contains("<searchResult offset=\"0\" totalHits=\"1\"><match id="),
            "{xml}"
        );
    }
);

fixture_test!(
    random_songs_year_filter_uses_track_then_album_year,
    |server, _pool| {
        let random = |params: &'static [(&'static str, &'static str)]| {
            let server = &server;
            async move {
                let mut all = vec![("size", "500")];
                all.extend_from_slice(params);
                let res = alice(server, "getRandomSongs", &all).await;
                assert_eq!(res["status"], "ok", "{res}");
                sorted(ids(&res["randomSongs"]["song"]))
            }
        };
        // Track 2 has its own year (1970); the rest of album 1 falls back to 1969.
        assert_eq!(
            random(&[("fromYear", "1970"), ("toYear", "1970")]).await,
            sorted(track_ids(&[2]))
        );
        assert_eq!(
            random(&[("fromYear", "1969"), ("toYear", "1969")]).await,
            sorted(track_ids(&[1, 3, 4, 5]))
        );
        assert_eq!(
            random(&[("fromYear", "1969"), ("toYear", "1970")]).await,
            sorted(track_ids(&[1, 2, 3, 4, 5]))
        );
        // Swapped bounds, and one open end each way.
        assert_eq!(
            random(&[("fromYear", "1970"), ("toYear", "1969")]).await,
            sorted(track_ids(&[1, 2, 3, 4, 5]))
        );
        assert_eq!(
            random(&[("toYear", "1968")]).await,
            sorted(track_ids(&[6, 7, 8, 9]))
        );
        // Track 13 has its own year 2009 inside an album dated 2010; the missing track 20 never shows.
        assert_eq!(
            random(&[("fromYear", "2010")]).await,
            sorted(track_ids(&[14, 15, 16]))
        );
        assert_eq!(
            random(&[("fromYear", "2009"), ("toYear", "2009")]).await,
            sorted(track_ids(&[10, 11, 12, 13]))
        );
        // The undated album is only in when there is no filter.
        assert_eq!(random(&[]).await.len(), 19);
        assert_failed(
            &alice(&server, "getRandomSongs", &[("fromYear", "abc")]).await,
            10,
        );
    }
);

fixture_test!(random_songs_size_bounds, |server, _pool| {
    let count = |res: &Value| res["randomSongs"]["song"].as_array().unwrap().len();
    assert_eq!(count(&alice(&server, "getRandomSongs", &[]).await), 10);
    assert_eq!(
        count(&alice(&server, "getRandomSongs", &[("size", "5")]).await),
        5
    );
    assert_eq!(
        count(&alice(&server, "getRandomSongs", &[("size", "0")]).await),
        1
    );
    assert_eq!(
        count(&alice(&server, "getRandomSongs", &[("size", "-4")]).await),
        1
    );
    let res = alice(&server, "getRandomSongs", &[("size", "9999")]).await;
    assert_eq!(count(&res), 19);
    let all = ids(&res["randomSongs"]["song"]);
    let unique: HashSet<_> = all.iter().collect();
    assert_eq!(unique.len(), 19);
    assert!(!all.contains(&track_id(20).to_string()));
});

fixture_test!(random_songs_genre_filter, |server, _pool| {
    // Effective track genres, case-insensitive exact: Rock is tracks 1, 2, 3 and 5; the missing
    // track 20 carries it too and stays out.
    let rock = [1, 2, 3, 5];
    for genre in ["Rock", "rock", "  ROCK "] {
        let res = alice(
            &server,
            "getRandomSongs",
            &[("genre", genre), ("size", "500")],
        )
        .await;
        assert_eq!(
            sorted(ids(&res["randomSongs"]["song"])),
            sorted(track_ids(&rock)),
            "{genre}"
        );
    }
    let res = alice(
        &server,
        "getRandomSongs",
        &[("genre", "post-rock"), ("size", "500")],
    )
    .await;
    assert_eq!(ids(&res["randomSongs"]["song"]), track_ids(&[4]));
    let res = alice(
        &server,
        "getRandomSongs",
        &[("genre", "Nope"), ("size", "500")],
    )
    .await;
    assert_eq!(res["randomSongs"]["song"], serde_json::json!([]));
    // The size still caps the page, and an empty genre filters nothing.
    let res = alice(
        &server,
        "getRandomSongs",
        &[("genre", "rock"), ("size", "2")],
    )
    .await;
    assert_eq!(res["randomSongs"]["song"].as_array().unwrap().len(), 2);
    let res = alice(&server, "getRandomSongs", &[("genre", ""), ("size", "500")]).await;
    assert_eq!(res["randomSongs"]["song"].as_array().unwrap().len(), 19);
});

fixture_test!(songs_by_genre_missing_10, |server, _pool| {
    let res = alice(&server, "getSongsByGenre", &[]).await;
    assert_failed(&res, 10);
    assert_eq!(
        res["error"]["message"],
        "Required parameter is missing: genre"
    );
    let res = alice(&server, "getSongsByGenre", &[("genre", "Nope")]).await;
    assert_eq!(res["status"], "ok");
    assert_eq!(res["songsByGenre"]["song"], serde_json::json!([]));
});

fixture_test!(songs_by_genre_exact_case_insensitive, |server, _pool| {
    for genre in ["rock", "ROCK", " Rock "] {
        let res = alice(&server, "getSongsByGenre", &[("genre", genre)]).await;
        let got = ids(&res["songsByGenre"]["song"]);
        // Not track 4 (post-rock), not the missing track 20, not track 1's shadowed file Pop.
        assert_eq!(got, track_ids(&[1, 5, 3, 2]), "{genre}");
    }
    let res = alice(&server, "getSongsByGenre", &[("genre", "post-rock")]).await;
    assert_eq!(ids(&res["songsByGenre"]["song"]), track_ids(&[4]));
    let res = alice(&server, "getSongsByGenre", &[("genre", "ock")]).await;
    assert_eq!(res["songsByGenre"]["song"], serde_json::json!([]));
    // A percent sign is text, not a pattern.
    let res = alice(&server, "getSongsByGenre", &[("genre", "%")]).await;
    assert_eq!(res["songsByGenre"]["song"], serde_json::json!([]));
    // Track 1 has MusicBrainz Rock, so its file Pop does not count.
    let res = alice(&server, "getSongsByGenre", &[("genre", "pop")]).await;
    assert_eq!(ids(&res["songsByGenre"]["song"]), track_ids(&[5]));
});

fixture_test!(songs_by_genre_paging_and_order, |server, _pool| {
    let page = |count: &'static str, offset: &'static str| {
        let server = &server;
        async move {
            let res = alice(
                server,
                "getSongsByGenre",
                &[("genre", "rock"), ("count", count), ("offset", offset)],
            )
            .await;
            ids(&res["songsByGenre"]["song"])
        }
    };
    // Come Together, Here Comes the Sun, Maxwell's Silver Hammer, Something.
    assert_eq!(page("2", "0").await, track_ids(&[1, 5]));
    assert_eq!(page("2", "2").await, track_ids(&[3, 2]));
    assert_eq!(page("2", "3").await, track_ids(&[2]));
    assert_eq!(page("2", "4").await, Vec::<String>::new());
    assert_eq!(page("0", "0").await.len(), 1, "count clamps to 1..500");
    assert_eq!(page("9999", "0").await.len(), 4);
    assert_eq!(page("2", "-5").await, track_ids(&[1, 5]));
    // The default count is 50.
    let res = alice(&server, "getSongsByGenre", &[("genre", "rock")]).await;
    assert_eq!(res["songsByGenre"]["song"].as_array().unwrap().len(), 4);
});

fixture_test!(song_genre_falls_back_to_artist, |server, _pool| {
    let song = |n: u128| {
        let server = &server;
        async move {
            let res = alice(server, "getSong", &[("id", &track_id(n).to_string())]).await;
            res["song"].clone()
        }
    };
    // Own effective genre: MusicBrainz Rock wins over the file's Pop (track 1); file Post-Rock
    // (4); first of two by rank (5).
    assert_eq!(song(1).await["genre"], "Rock");
    assert_eq!(song(4).await["genre"], "Post-Rock");
    assert_eq!(song(5).await["genre"], "Rock");
    assert_eq!(song(10).await["genre"], "Electronic");
    // No track genre: the album artist's first effective genre (La Roux: Pop, not the file Rock).
    assert_eq!(song(11).await["genre"], "Pop");
    assert_eq!(song(12).await["genre"], "Pop");
    // Beatles tracks without their own take the artist's Rock.
    assert_eq!(song(6).await["genre"], "Rock");
    // Neither track nor artist genre: no attribute.
    assert!(song(13).await.get("genre").is_none());
});

fixture_test!(music_brainz_ids_only_when_present, |server, _pool| {
    let res = alice(&server, "getAlbum", &[("id", &album_id(1).to_string())]).await;
    let album = &res["album"];
    assert_eq!(
        album["musicBrainzId"],
        "2a2c6d57-2f46-4b5e-9d0a-7e2f4a1d5c11"
    );
    assert_eq!(album["genre"], "Rock");
    let songs = album["song"].as_array().unwrap();
    assert_eq!(
        songs[0]["musicBrainzId"],
        "9d1c2f0e-6b57-4a46-8a9c-3f3b1a0c7d21"
    );
    assert_eq!(
        songs[2]["musicBrainzId"],
        "5e4a3b2c-1d0f-4e9a-b8c7-6d5e4f3a2b10"
    );
    assert!(songs[1].get("musicBrainzId").is_none());
    // Albums without a release id.
    let res = alice(&server, "getAlbum", &[("id", &album_id(3).to_string())]).await;
    assert!(res["album"].get("musicBrainzId").is_none());
    assert_eq!(res["album"]["genre"], "Pop");
    let res = alice(&server, "getAlbum", &[("id", &album_id(4).to_string())]).await;
    assert!(res["album"].get("genre").is_none());
    // Artists: the Beatles' own, none for La Roux.
    let res = alice(&server, "getArtist", &[("id", &artist_id(1).to_string())]).await;
    assert_eq!(
        res["artist"]["musicBrainzId"],
        "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d"
    );
    let res = alice(&server, "getArtist", &[("id", &artist_id(2).to_string())]).await;
    assert!(res["artist"].get("musicBrainzId").is_none());
    // No null or empty attribute in XML either.
    let xml = rest_xml(
        &server,
        "getArtist",
        &[("apiKey", FIXTURE_TOKEN), ("id", &artist_id(2).to_string())],
    )
    .await;
    assert!(
        !xml.contains("musicBrainzId") && !xml.contains("genre=\"\""),
        "{xml}"
    );
    // Search results carry them too.
    let res = alice(&server, "search3", &[("query", "abbey")]).await;
    assert_eq!(
        res["searchResult3"]["album"][0]["musicBrainzId"],
        "2a2c6d57-2f46-4b5e-9d0a-7e2f4a1d5c11"
    );
});

fixture_test!(
    top_songs_exact_then_substring_by_play_count,
    |server, _pool| {
        let top = |res: &Value| ids(&res["topSongs"]["song"]);
        // Case-insensitive exact name. Plays count over all users: track 1 twice, track 6 once.
        let res = alice(&server, "getTopSongs", &[("artist", "the beatles")]).await;
        assert_eq!(top(&res), track_ids(&[1, 6, 8, 7, 9, 5, 3, 4, 2]), "{res}");
        let res = alice(
            &server,
            "getTopSongs",
            &[("artist", "The Beatles"), ("count", "3")],
        )
        .await;
        assert_eq!(top(&res), track_ids(&[1, 6, 8]));
        // No exact match: substring.
        let res = alice(&server, "getTopSongs", &[("artist", "ROUX")]).await;
        assert_eq!(top(&res), track_ids(&[10, 11, 17, 18, 19, 12]));
        // A track artist on a compilation.
        let res = alice(&server, "getTopSongs", &[("artist", "Kesha")]).await;
        assert_eq!(top(&res), track_ids(&[13]));
        // count is at least 1.
        let res = alice(
            &server,
            "getTopSongs",
            &[("artist", "roux"), ("count", "0")],
        )
        .await;
        assert_eq!(top(&res), track_ids(&[10]));
        assert_eq!(
            res["topSongs"]["song"][0]["starred"],
            serde_json::Value::Null
        );
        assert_failed(&alice(&server, "getTopSongs", &[]).await, 10);
    }
);

fixture_test!(top_songs_unknown_artist_empty, |server, _pool| {
    let res = alice(&server, "getTopSongs", &[("artist", "Nobody At All")]).await;
    assert_eq!(res["status"], "ok");
    assert_eq!(res["topSongs"]["song"], serde_json::json!([]));
    // Theatre's only track is missing.
    let res = alice(&server, "getTopSongs", &[("artist", "Theatre")]).await;
    assert_eq!(res["topSongs"]["song"], serde_json::json!([]));
    let xml = rest_xml(
        &server,
        "getTopSongs",
        &[("apiKey", FIXTURE_TOKEN), ("artist", "Nobody")],
    )
    .await;
    assert!(xml.contains("<topSongs/>"), "{xml}");
});

fixture_test!(similar_songs_missing_id_10_else_empty, |server, _pool| {
    assert_failed(&alice(&server, "getSimilarSongs", &[]).await, 10);
    let res = alice(
        &server,
        "getSimilarSongs",
        &[("id", &track_id(1).to_string())],
    )
    .await;
    assert_eq!(res["similarSongs"]["song"], serde_json::json!([]));
    let res = alice(&server, "getSimilarSongs", &[("id", "whatever")]).await;
    assert_eq!(res["similarSongs"]["song"], serde_json::json!([]));
});

fixture_test!(similar_songs2_empty, |server, _pool| {
    let res = alice(
        &server,
        "getSimilarSongs2",
        &[("id", &track_id(1).to_string())],
    )
    .await;
    assert_eq!(res["similarSongs2"]["song"], serde_json::json!([]));
    assert_failed(&alice(&server, "getSimilarSongs2", &[]).await, 10);
});

fixture_test!(
    get_genres_counts_distinct_tracks_and_albums,
    |server, _pool| {
        let res = alice(&server, "getGenres", &[]).await;
        assert_eq!(res["status"], "ok");
        // Ghost is carried only by the missing track 20; Rock's albums are 1 and 2 (6 has no present
        // track), Pop's are 1 (second rank) and 3.
        assert_eq!(
            res["genres"]["genre"],
            serde_json::json!([
                {"songCount": 1, "albumCount": 1, "value": "Electronic"},
                {"songCount": 1, "albumCount": 2, "value": "Pop"},
                {"songCount": 1, "albumCount": 0, "value": "Post-Rock"},
                {"songCount": 4, "albumCount": 2, "value": "Rock"},
            ])
        );
        let xml = rest_xml(&server, "getGenres", &[("apiKey", FIXTURE_TOKEN)]).await;
        assert!(
            xml.contains(r#"<genre songCount="4" albumCount="2">Rock</genre>"#),
            "{xml}"
        );
    }
);

fixture_test!(get_genres_sorted_by_name, |server, pool| {
    sqlx::query(
        "INSERT INTO genres (id, name, key) VALUES
         ('00000000-0000-7000-8000-b00000000010', 'ambient', 'ambient'),
         ('00000000-0000-7000-8000-b00000000011', 'Zydeco', 'zydeco')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for g in ["ambient", "Zydeco"] {
        sqlx::query(
            "INSERT INTO track_genres (track_id, genre_id, source, rank)
             SELECT $1, id, 'file', 0 FROM genres WHERE name = $2",
        )
        .bind(track_id(7))
        .bind(g)
        .execute(&pool)
        .await
        .unwrap();
    }
    let res = alice(&server, "getGenres", &[]).await;
    let names: Vec<_> = res["genres"]["genre"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["value"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "ambient",
            "Electronic",
            "Pop",
            "Post-Rock",
            "Rock",
            "Zydeco"
        ]
    );
});

/// Spec §13: one empty-query page of 500 songs over 10 000 tracks, a tenth of them starred.
/// Run by hand: `cargo test -p kima-subsonic --test search -- --ignored --nocapture`.
#[ignore]
#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn search3_sync_page_timing_10k(pool: PgPool) {
    seed_extra(&pool, 10_000, 0).await;
    sqlx::query(
        "INSERT INTO stars (id, user_id, track_id, starred_at)
         SELECT gen_random_uuid(), $1, t.id, now()
         FROM (SELECT id, row_number() OVER (ORDER BY id) AS n FROM tracks
               WHERE artist_name = 'Extra Artist') t
         WHERE t.n % 10 = 0",
    )
    .bind(ALICE_ID)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE tracks").execute(&pool).await.unwrap();
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN SELECT t.id FROM tracks t WHERE t.missing_since IS NULL
         ORDER BY lower(t.title) ASC, t.id ASC LIMIT 500 OFFSET 5000",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    println!("{}", plan.join("\n"));
    let server = app(pool);
    for offset in ["0", "5000", "9500"] {
        let start = std::time::Instant::now();
        let res = alice(
            &server,
            "search3",
            &[
                ("songCount", "500"),
                ("songOffset", offset),
                ("artistCount", "0"),
                ("albumCount", "0"),
            ],
        )
        .await;
        let elapsed = start.elapsed();
        let songs = res["searchResult3"]["song"].as_array().unwrap();
        let starred = songs.iter().filter(|s| s.get("starred").is_some()).count();
        println!(
            "search3 empty query, songCount=500 songOffset={offset}: {} songs, {starred} starred, {elapsed:?}",
            songs.len()
        );
        assert!(!songs.is_empty());
    }
}
