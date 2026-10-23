//! Browsing, album lists and scan endpoints on the SQL fixture (spec §4.1, §4.2, §10.2).

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_db::PgPool;
use serde_json::{Value, json};

fn app(pool: PgPool) -> TestServer {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool)).layer(MockConnectInfo(peer));
    TestServer::new(router).unwrap()
}

/// Calls as alice, with her app token.
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

fn album_ids(ns: &[u128]) -> Vec<String> {
    ns.iter().map(|&n| album_id(n).to_string()).collect()
}

fn track_ids(ns: &[u128]) -> Vec<String> {
    ns.iter().map(|&n| track_id(n).to_string()).collect()
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

fixture_test!(beatles_under_b_la_roux_under_r, |server, _pool| {
    let res = alice(&server, "getArtists", &[]).await;
    assert_eq!(res["status"], "ok");
    let indexes = res["artists"]["index"].as_array().unwrap();
    let names: Vec<_> = indexes
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    // Theatre has only a missing track; there is no T (The) and no L (La).
    assert_eq!(names, ["B", "R", "V"]);
    let beatles = &indexes[0]["artist"][0];
    assert_eq!(beatles["name"], "The Beatles");
    assert_eq!(beatles["id"], artist_id(1).to_string());
    assert_eq!(beatles["albumCount"], 2);
    assert_eq!(beatles["coverArt"], format!("ar-{}", artist_id(1)));
    assert_eq!(beatles["starred"], "2026-01-01T04:06:00.000Z");
    assert!(beatles.get("userRating").is_none());
    let roux = &indexes[1]["artist"][0];
    assert_eq!(roux["name"], "La Roux");
    assert_eq!(roux["userRating"], 3);
    assert!(roux.get("starred").is_none());
    assert_eq!(roux["albumCount"], 2);

    let xml = rest_xml(&server, "getArtists", &[("apiKey", FIXTURE_TOKEN)]).await;
    assert!(xml.contains("<index name=\"B\"><artist id=\""), "{xml}");
    assert!(xml.contains("ignoredArticles=\"The A An Le La Les Los Las Die Der Das\""));
});

fixture_test!(ignored_articles_lists_all, |server, _pool| {
    let res = alice(&server, "getArtists", &[]).await;
    let articles = res["artists"]["ignoredArticles"].as_str().unwrap();
    assert_eq!(articles, "The A An Le La Les Los Las Die Der Das");
    for a in [
        "The", "A", "An", "Le", "La", "Les", "Los", "Las", "Die", "Der", "Das",
    ] {
        assert!(articles.split(' ').any(|x| x == a), "{a}");
    }
});

fixture_test!(
    get_indexes_last_modified_now_ignores_if_modified_since,
    |server, _pool| {
        let now_ms = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
        };
        let before = now_ms();
        // A watermark in the far future: still the full index.
        let res = alice(
            &server,
            "getIndexes",
            &[("ifModifiedSince", "99999999999999")],
        )
        .await;
        let after = now_ms();
        assert_eq!(res["status"], "ok");
        let last = res["indexes"]["lastModified"].as_i64().unwrap();
        assert!(
            (before..=after).contains(&last),
            "{last} not in {before}..{after}"
        );
        assert_eq!(
            res["indexes"]["ignoredArticles"],
            "The A An Le La Les Los Las Die Der Das"
        );
        let names: Vec<_> = res["indexes"]["index"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["B", "R", "V"]);
    }
);

fixture_test!(get_artist_albums_newest_first, |server, _pool| {
    let res = alice(&server, "getArtist", &[("id", &artist_id(1).to_string())]).await;
    assert_eq!(res["artist"]["name"], "The Beatles");
    assert_eq!(res["artist"]["albumCount"], 2);
    assert_eq!(ids(&res["artist"]["album"]), album_ids(&[1, 2]));
    let abbey = &res["artist"]["album"][0];
    assert_eq!(abbey["name"], "Abbey Road");
    assert_eq!(abbey["artist"], "The Beatles");
    assert_eq!(abbey["artistId"], artist_id(1).to_string());
    assert_eq!(abbey["songCount"], 5);
    assert_eq!(abbey["duration"], 259 + 183 + 207 + 206 + 185);
    assert_eq!(abbey["year"], 1969);
    assert_eq!(abbey["userRating"], 4);
    assert_eq!(abbey["starred"], "2026-01-01T04:04:00.000Z");
    assert_eq!(abbey["created"], "2026-01-01T00:01:00.000Z");
    // Undated albums sort last.
    let res = alice(&server, "getArtist", &[("id", &artist_id(2).to_string())]).await;
    assert_eq!(ids(&res["artist"]["album"]), album_ids(&[3, 5]));
    assert!(res["artist"]["album"][1].get("year").is_none());
});

fixture_test!(get_artist_unknown_70, |server, _pool| {
    let res = alice(&server, "getArtist", &[("id", &artist_id(99).to_string())]).await;
    assert_failed(&res, 70);
    assert_eq!(res["error"]["message"], "Artist not found");
    // Theatre exists but has no present track.
    let res = alice(&server, "getArtist", &[("id", &artist_id(4).to_string())]).await;
    assert_failed(&res, 70);
    assert_failed(&alice(&server, "getArtist", &[]).await, 10);
});

fixture_test!(music_directory_root_artist_album, |server, _pool| {
    let res = alice(&server, "getMusicDirectory", &[("id", "1")]).await;
    let dir = &res["directory"];
    assert_eq!(dir["id"], "1");
    assert_eq!(dir["name"], "Music");
    let children = dir["child"].as_array().unwrap();
    // By sort name: beatles, roux, various artists.
    assert_eq!(
        ids(&dir["child"]),
        [artist_id(1), artist_id(2), artist_id(3)].map(|u| u.to_string())
    );
    assert_eq!(children[0]["isDir"], true);
    assert_eq!(children[0]["title"], "The Beatles");
    assert_eq!(children[0]["parent"], "1");

    // An artist, bare and prefixed, lists its albums as folders.
    for id in [
        artist_id(1).to_string(),
        format!("ar-{}", artist_id(1)),
        format!("artist:{}", artist_id(1)),
    ] {
        let res = alice(&server, "getMusicDirectory", &[("id", &id)]).await;
        let dir = &res["directory"];
        assert_eq!(dir["id"], artist_id(1).to_string(), "{id}");
        assert_eq!(dir["name"], "The Beatles");
        assert_eq!(dir["parent"], "1");
        assert_eq!(ids(&dir["child"]), album_ids(&[1, 2]));
        assert_eq!(dir["child"][0]["isDir"], true);
        assert_eq!(dir["child"][0]["title"], "Abbey Road");
        assert_eq!(dir["child"][0]["album"], "Abbey Road");
        assert_eq!(dir["child"][0]["parent"], artist_id(1).to_string());
    }

    // An album lists its songs as children.
    for id in [
        album_id(2).to_string(),
        format!("al-{}", album_id(2)),
        format!("album:{}", album_id(2)),
    ] {
        let res = alice(&server, "getMusicDirectory", &[("id", &id)]).await;
        let dir = &res["directory"];
        assert_eq!(dir["id"], album_id(2).to_string(), "{id}");
        assert_eq!(dir["name"], "White Album");
        assert_eq!(dir["parent"], artist_id(1).to_string());
        assert_eq!(ids(&dir["child"]), track_ids(&[6, 7, 8, 9]));
        assert_eq!(dir["child"][0]["isDir"], false);
    }
});

fixture_test!(music_directory_unknown_70, |server, _pool| {
    for id in [
        artist_id(99).to_string(),
        "zzz".to_owned(),
        // The wrong prefix for the kind of id.
        format!("al-{}", artist_id(1)),
        format!("ar-{}", album_id(1)),
        // Theatre's only album has no present track.
        album_id(6).to_string(),
    ] {
        let res = alice(&server, "getMusicDirectory", &[("id", &id)]).await;
        assert_failed(&res, 70);
        assert_eq!(res["error"]["message"], "Directory not found", "{id}");
    }
    assert_failed(&alice(&server, "getMusicDirectory", &[]).await, 10);
});

fixture_test!(get_album_song_order, |server, _pool| {
    let res = alice(&server, "getAlbum", &[("id", &album_id(2).to_string())]).await;
    let album = &res["album"];
    assert_eq!(album["name"], "White Album");
    assert_eq!(album["songCount"], 4);
    // Disc 1 then disc 2, each by track number.
    assert_eq!(ids(&album["song"]), track_ids(&[6, 7, 8, 9]));
    assert_eq!(album["song"][2]["discNumber"], 2);
    assert_eq!(album["song"][0]["bitRate"], 320);
    assert_eq!(album["song"][0]["suffix"], "mp3");
    assert_eq!(album["song"][0]["contentType"], "audio/mpeg");

    // A compilation: the song's artist is the track artist, artistId the album artist.
    let res = alice(&server, "getAlbum", &[("id", &album_id(4).to_string())]).await;
    let song = &res["album"]["song"][0];
    assert_eq!(song["artist"], "Kesha");
    assert_eq!(song["artistId"], artist_id(3).to_string());
    assert_eq!(song["year"], 2009);
    assert_eq!(res["album"]["song"][1]["year"], 2010);
    assert_eq!(res["album"]["userRating"], 5);

    // The first Abbey Road track is starred and rated by alice.
    let res = alice(&server, "getAlbum", &[("id", &album_id(1).to_string())]).await;
    let first = &res["album"]["song"][0];
    assert_eq!(first["starred"], "2026-01-01T04:01:00.000Z");
    assert_eq!(first["userRating"], 5);
    assert_eq!(first["bitRate"], 900);
    assert_eq!(first["contentType"], "audio/flac");
    assert!(first.get("path").is_none());
    assert_eq!(first["genre"], "Rock");
    // Another user's stars do not show.
    let res = admin(&server, "getAlbum", &[("id", &album_id(1).to_string())]).await;
    let song = &res["album"]["song"];
    assert!(song[0].get("starred").is_none());
    assert_eq!(song[0]["userRating"], 1);
    assert_eq!(song[1]["starred"], "2026-01-01T04:07:00.000Z");
});

fixture_test!(get_song_missing_track_70, |server, _pool| {
    let res = alice(&server, "getSong", &[("id", &track_id(3).to_string())]).await;
    assert_eq!(res["song"]["title"], "Maxwell's Silver Hammer");
    assert_eq!(res["song"]["album"], "Abbey Road");
    let res = alice(&server, "getSong", &[("id", &track_id(20).to_string())]).await;
    assert_failed(&res, 70);
    assert_eq!(res["error"]["message"], "Song not found");
    let res = alice(&server, "getSong", &[("id", &track_id(99).to_string())]).await;
    assert_failed(&res, 70);
});

fixture_test!(non_uuid_id_70, |server, _pool| {
    for (endpoint, message) in [
        ("getArtist", "Artist not found"),
        ("getAlbum", "Album not found"),
        ("getSong", "Song not found"),
        ("getMusicDirectory", "Directory not found"),
    ] {
        for id in ["abc", "tr-x", "42"] {
            let res = alice(&server, endpoint, &[("id", id)]).await;
            assert_failed(&res, 70);
            assert_eq!(res["error"]["message"], message, "{endpoint} {id}");
        }
    }
    // Prefixes are not accepted where upstream takes bare ids only.
    let res = alice(
        &server,
        "getAlbum",
        &[("id", &format!("al-{}", album_id(1)))],
    )
    .await;
    assert_failed(&res, 70);
});

fixture_test!(album_list2_each_type, |server, _pool| {
    let list = |res: &Value| ids(&res["albumList2"]["album"]);
    let case = |kind: &'static str| {
        let server = &server;
        async move { list(&alice(server, "getAlbumList2", &[("type", kind)]).await) }
    };
    assert_eq!(case("newest").await, album_ids(&[5, 4, 3, 2, 1]));
    assert_eq!(case("highest").await, album_ids(&[4, 1]));
    assert_eq!(case("frequent").await, album_ids(&[3, 1, 4]));
    assert_eq!(case("recent").await, album_ids(&[1, 3, 4]));
    assert_eq!(
        case("alphabeticalByName").await,
        album_ids(&[1, 4, 3, 5, 2])
    );
    assert_eq!(
        case("alphabeticalByArtist").await,
        album_ids(&[1, 2, 3, 5, 4])
    );
    // Starred albums, newest star first; Empty Stage has no present track.
    assert_eq!(case("starred").await, album_ids(&[1, 3]));
    let mut random = case("random").await;
    random.sort();
    assert_eq!(random, album_ids(&[1, 2, 3, 4, 5]));
    // Years 1968..=1969 ascending, and descending when from > to.
    let by_year = |from: &'static str, to: &'static str| {
        let server = &server;
        async move {
            list(
                &alice(
                    server,
                    "getAlbumList2",
                    &[("type", "byYear"), ("fromYear", from), ("toYear", to)],
                )
                .await,
            )
        }
    };
    assert_eq!(by_year("1968", "1969").await, album_ids(&[2, 1]));
    assert_eq!(by_year("1969", "1968").await, album_ids(&[1, 2]));
    assert_eq!(by_year("2009", "2010").await, album_ids(&[3, 4]));
    // Another user's plays and stars do not leak in.
    let res = admin(&server, "getAlbumList2", &[("type", "frequent")]).await;
    assert_eq!(ids(&res["albumList2"]["album"]), album_ids(&[2]));
    let res = admin(&server, "getAlbumList2", &[("type", "starred")]).await;
    assert_eq!(ids(&res["albumList2"]["album"]), Vec::<String>::new());
    // size and offset page the list.
    let res = alice(
        &server,
        "getAlbumList2",
        &[("type", "newest"), ("size", "2"), ("offset", "1")],
    )
    .await;
    assert_eq!(ids(&res["albumList2"]["album"]), album_ids(&[4, 3]));
    // The ID3 shape: no folder fields.
    let first = &res["albumList2"]["album"][0];
    assert_eq!(first["name"], "Now Hits");
    assert!(first.get("isDir").is_none() && first.get("title").is_none());
});

fixture_test!(album_list2_by_year_missing_param_10, |server, _pool| {
    let res = alice(
        &server,
        "getAlbumList2",
        &[("type", "byYear"), ("fromYear", "1968")],
    )
    .await;
    assert_failed(&res, 10);
    assert_eq!(
        res["error"]["message"],
        "Required parameter is missing: toYear"
    );
    let res = alice(&server, "getAlbumList2", &[("type", "byYear")]).await;
    assert_failed(&res, 10);
    assert_eq!(
        res["error"]["message"],
        "Required parameter is missing: fromYear"
    );
});

fixture_test!(album_list2_unknown_type_is_newest, |server, _pool| {
    for params in [&[("type", "bogus")][..], &[]] {
        let res = alice(&server, "getAlbumList2", params).await;
        assert_eq!(
            ids(&res["albumList2"]["album"]),
            album_ids(&[5, 4, 3, 2, 1])
        );
    }
});

fixture_test!(album_list2_size_capped_500, |server, pool| {
    // 600 more albums with one track each.
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year, is_compilation, created_at)
         SELECT gen_random_uuid(), '00000000-0000-7000-8000-100000000001', 'Bulk ' || n, 'bulk ' || n,
                2000, false, '2027-01-01T00:00:00Z'
         FROM generate_series(1, 600) n",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, disc, track_no, duration_secs, codec,
                             path, size, mtime, fingerprint, created_at)
         SELECT gen_random_uuid(), al.id, 'The Beatles', al.title, 1, 1, 100, 'mp3',
                'bulk/' || al.title || '.mp3', 1, now(), sha256(al.title::bytea), now()
         FROM albums al WHERE al.title LIKE 'Bulk %'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let res = alice(&server, "getAlbumList2", &[("size", "100000")]).await;
    assert_eq!(res["albumList2"]["album"].as_array().unwrap().len(), 500);
    let res = alice(
        &server,
        "getAlbumList2",
        &[("size", "500"), ("offset", "500")],
    )
    .await;
    assert_eq!(res["albumList2"]["album"].as_array().unwrap().len(), 105);
    // The default is 10.
    let res = alice(&server, "getAlbumList2", &[]).await;
    assert_eq!(res["albumList2"]["album"].as_array().unwrap().len(), 10);
    // Junk paging falls back to the defaults (size 10, offset 0), as upstream and Navidrome.
    let junk = alice(
        &server,
        "getAlbumList2",
        &[("size", "ten"), ("offset", "10.5")],
    )
    .await;
    assert_eq!(junk["albumList2"], res["albumList2"]);
});

fixture_test!(album_list2_by_genre, |server, _pool| {
    let names = |res: &Value| -> Vec<String> {
        res["albumList2"]["album"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_owned())
            .collect()
    };
    // Effective album genres, ordered by sort name. Empty Stage (6) carries Rock but has no
    // present track.
    for genre in ["Rock", "rock", " ROCK"] {
        let res = alice(
            &server,
            "getAlbumList2",
            &[("type", "byGenre"), ("genre", genre)],
        )
        .await;
        assert_eq!(res["status"], "ok");
        assert_eq!(names(&res), ["Abbey Road", "White Album"], "{genre}");
        assert_eq!(res["albumList2"]["album"][0]["genre"], "Rock");
    }
    let res = alice(
        &server,
        "getAlbumList2",
        &[("type", "byGenre"), ("genre", "pop")],
    )
    .await;
    assert_eq!(names(&res), ["Abbey Road", "La Roux"]);
    // No substring match, and an unused genre is an empty list.
    for genre in ["ock", "Post-Rock", "Nope"] {
        let res = alice(
            &server,
            "getAlbumList2",
            &[("type", "byGenre"), ("genre", genre)],
        )
        .await;
        assert_eq!(res["status"], "ok");
        assert_eq!(res["albumList2"]["album"], json!([]), "{genre}");
    }
    // Paging.
    let res = alice(
        &server,
        "getAlbumList2",
        &[
            ("type", "byGenre"),
            ("genre", "rock"),
            ("size", "1"),
            ("offset", "1"),
        ],
    )
    .await;
    assert_eq!(names(&res), ["White Album"]);
    // The legacy folder list takes it too.
    let res = alice(
        &server,
        "getAlbumList",
        &[("type", "byGenre"), ("genre", "pop")],
    )
    .await;
    assert_eq!(res["albumList"]["album"].as_array().unwrap().len(), 2);
});

fixture_test!(album_list2_by_genre_missing_10, |server, _pool| {
    let res = alice(&server, "getAlbumList2", &[("type", "byGenre")]).await;
    assert_failed(&res, 10);
    assert_eq!(res["error"]["message"], "byGenre requires genre");
    let xml = rest_xml(
        &server,
        "getAlbumList2",
        &[("apiKey", FIXTURE_TOKEN), ("type", "byGenre")],
    )
    .await;
    assert!(
        xml.contains(r#"code="10""#) && xml.contains("byGenre requires genre"),
        "{xml}"
    );
});

fixture_test!(album_list_child_shape, |server, _pool| {
    let res = alice(
        &server,
        "getAlbumList",
        &[("type", "newest"), ("size", "1")],
    )
    .await;
    let album = &res["albumList"]["album"][0];
    assert_eq!(album["id"], album_id(5).to_string());
    assert_eq!(album["isDir"], true);
    assert_eq!(album["title"], "Untitled Demos");
    assert_eq!(album["parent"], artist_id(2).to_string());
    // Plus the ID3 fields.
    assert_eq!(album["name"], "Untitled Demos");
    assert_eq!(album["artist"], "La Roux");
    assert_eq!(album["artistId"], artist_id(2).to_string());
    assert_eq!(album["coverArt"], album_id(5).to_string());
    assert_eq!(album["songCount"], 3);
    let res = alice(&server, "getAlbumList", &[("type", "byYear")]).await;
    assert_failed(&res, 10);
});

fixture_test!(
    alphabetical_and_starred_order_by_sort_name,
    |server, pool| {
        // "The Zebra" files under Z, not T, and "An Apple" under A.
        sqlx::query("INSERT INTO artists (id, name, name_key) VALUES (gen_random_uuid(), 'The Zebra', 'the zebra'), (gen_random_uuid(), 'An Apple', 'an apple')")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO albums (id, artist_id, title, title_key, year, is_compilation)
         SELECT gen_random_uuid(), id, name, name_key, 2001, false
         FROM artists WHERE name IN ('The Zebra', 'An Apple')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, disc, track_no, duration_secs, codec,
                             path, size, mtime, fingerprint)
         SELECT gen_random_uuid(), al.id, 'x', 'song', 1, 1, 100, 'mp3', al.title || '.mp3', 1, now(),
                sha256(al.title::bytea)
         FROM albums al WHERE al.title IN ('The Zebra', 'An Apple')",
    )
    .execute(&pool)
    .await
    .unwrap();
        // The directory root, albums by name and albums by artist all follow sort_name.
        let res = alice(&server, "getMusicDirectory", &[("id", "1")]).await;
        let titles: Vec<_> = res["directory"]["child"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["title"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            titles,
            [
                "An Apple",
                "The Beatles",
                "La Roux",
                "Various Artists",
                "The Zebra"
            ]
        );
        let res = alice(&server, "getAlbumList2", &[("type", "alphabeticalByName")]).await;
        let names: Vec<_> = res["albumList2"]["album"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "Abbey Road",
                "An Apple",
                "Now Hits",
                "La Roux",
                "Untitled Demos",
                "White Album",
                "The Zebra"
            ]
        );
        let res = alice(
            &server,
            "getAlbumList2",
            &[("type", "alphabeticalByArtist")],
        )
        .await;
        let artists: Vec<_> = res["albumList2"]["album"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["artist"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            artists,
            [
                "An Apple",
                "The Beatles",
                "The Beatles",
                "La Roux",
                "La Roux",
                "Various Artists",
                "The Zebra"
            ]
        );
        // Starred albums come newest star first, never by name.
        let res = alice(&server, "getAlbumList2", &[("type", "starred")]).await;
        assert_eq!(ids(&res["albumList2"]["album"]), album_ids(&[1, 3]));
    }
);

fixture_test!(start_scan_non_admin_50, |server, pool| {
    let res = alice(&server, "startScan", &[]).await;
    assert_failed(&res, 50);
    assert_eq!(res["error"]["message"], "Admin privileges required");
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'library'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
});

fixture_test!(start_scan_admin_queues_reconcile, |server, pool| {
    let res = admin(&server, "getScanStatus", &[]).await;
    assert_eq!(res["scanStatus"]["scanning"], false);
    let res = admin(&server, "startScan", &[]).await;
    assert_eq!(res["status"], "ok", "{res}");
    assert_eq!(res["scanStatus"]["scanning"], true);
    assert_eq!(res["scanStatus"]["count"], 19);
    // A second call does not queue a second reconcile.
    let res = admin(&server, "startScan", &[]).await;
    assert_eq!(res["scanStatus"]["scanning"], true);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE queue = 'library' AND payload ->> 'kind' = 'reconcile'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 1);
    // Anyone signed in sees it scanning.
    let res = alice(&server, "getScanStatus", &[]).await;
    assert_eq!(res["scanStatus"]["scanning"], true);
});

fixture_test!(scan_status_counts_present_tracks, |server, pool| {
    let res = alice(&server, "getScanStatus", &[]).await;
    // 20 tracks, one of them flagged missing.
    assert_eq!(res["scanStatus"]["count"], 19);
    assert_eq!(res["scanStatus"]["scanning"], false);
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track_id(1))
        .execute(&pool)
        .await
        .unwrap();
    let res = alice(&server, "getScanStatus", &[]).await;
    assert_eq!(res["scanStatus"]["count"], 18);
});
