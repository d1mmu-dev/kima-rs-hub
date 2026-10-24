//! Artist and album info and lyrics (spec §18 rows 58-63) on the SQL fixture. These endpoints read
//! stored data only: no request ever leaves the server (`get_lyrics_never_fetches`).

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_net::testing::{Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use serde_json::{Value, json};

struct Env {
    server: TestServer,
    pool: PgPool,
}

fn env(pool: PgPool) -> Env {
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(fixture_state(pool.clone())).layer(MockConnectInfo(peer));
    Env {
        server: TestServer::new(router).unwrap(),
        pool,
    }
}

impl Env {
    async fn json(&self, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let res = self
            .server
            .get(&format!("/{endpoint}.view"))
            .add_query_params([("f", "json"), ("apiKey", FIXTURE_TOKEN)])
            .add_query_params(params)
            .await;
        res.assert_status_ok();
        res.json::<Value>()["subsonic-response"].clone()
    }

    async fn xml(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        let res = self
            .server
            .get(&format!("/{endpoint}.view"))
            .add_query_params([("apiKey", FIXTURE_TOKEN)])
            .add_query_params(params)
            .await;
        res.assert_status_ok();
        res.text()
    }

    async fn ok(&self, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let body = self.json(endpoint, params).await;
        assert_eq!(body["status"], "ok", "{endpoint} {params:?}: {body}");
        body
    }

    async fn assert_error(&self, endpoint: &str, params: &[(&str, &str)], code: u32, msg: &str) {
        let body = self.json(endpoint, params).await;
        assert_eq!(body["status"], "failed", "{endpoint} {params:?}: {body}");
        assert_eq!(body["error"]["code"], code, "{endpoint} {params:?}: {body}");
        assert_eq!(body["error"]["message"], msg, "{endpoint} {params:?}");
    }

    async fn sql(&self, sql: &str) {
        sqlx::query(sql).execute(&self.pool).await.unwrap();
    }
}

fn s(id: uuid::Uuid) -> String {
    id.to_string()
}

// ---- getArtistInfo / getArtistInfo2 ----

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn artist_info2_biography_mbid_similar(pool: PgPool) {
    let env = env(pool);
    // La Roux is in the library; Theatre has only a missing track; the third is unknown.
    env.sql(&format!(
        r#"UPDATE artists SET bio = 'Liverpool <b>band</b> & more',
               "similar" = '[{{"name":"la roux","mbid":null,"match":0.9}},
                             {{"name":"Theatre","mbid":null,"match":0.8}},
                             {{"name":"Nobody Outside","mbid":null,"match":0.7}}]'
           WHERE id = '{}'"#,
        artist_id(1)
    ))
    .await;
    let beatles = s(artist_id(1));
    let la_roux = s(artist_id(2));
    for (endpoint, element) in [
        ("getArtistInfo2", "artistInfo2"),
        ("getArtistInfo", "artistInfo"),
    ] {
        let body = env.ok(endpoint, &[("id", &beatles)]).await;
        let info = &body[element];
        assert_eq!(info["biography"], "Liverpool <b>band</b> & more");
        assert_eq!(
            info["musicBrainzId"],
            "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d"
        );
        assert_eq!(info["coverArt"], format!("ar-{beatles}"));
        assert_eq!(
            info["similarArtist"],
            json!([{
                "id": la_roux,
                "name": "La Roux",
                "coverArt": format!("ar-{la_roux}"),
                "albumCount": 2,
            }])
        );
        // Prefixed ids work as in M2.
        let prefixed = env.ok(endpoint, &[("id", &format!("ar-{beatles}"))]).await;
        assert_eq!(prefixed[element], *info);
    }
    // The XML escapes the biography and carries it as element text.
    let xml = env.xml("getArtistInfo2", &[("id", &beatles)]).await;
    assert!(
        xml.contains("<biography>Liverpool &lt;b&gt;band&lt;/b&gt; &amp; more</biography>"),
        "{xml}"
    );
    assert!(
        xml.contains("<musicBrainzId>b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d</musicBrainzId>"),
        "{xml}"
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn artist_info_user_bio_wins(pool: PgPool) {
    let env = env(pool);
    let beatles = s(artist_id(1));
    env.sql(&format!(
        "UPDATE artists SET bio = 'From Wikipedia', user_bio = 'My words' WHERE id = '{beatles}'"
    ))
    .await;
    let body = env.ok("getArtistInfo2", &[("id", &beatles)]).await;
    assert_eq!(body["artistInfo2"]["biography"], "My words");
    // An empty user bio does not hide the stored one (upstream `userSummary || summary`).
    env.sql(&format!(
        "UPDATE artists SET user_bio = '' WHERE id = '{beatles}'"
    ))
    .await;
    let body = env.ok("getArtistInfo2", &[("id", &beatles)]).await;
    assert_eq!(body["artistInfo2"]["biography"], "From Wikipedia");
    // No bio and no MBID: both omitted; no similar artists is an empty list.
    let body = env.ok("getArtistInfo2", &[("id", &s(artist_id(2)))]).await;
    let info = body["artistInfo2"].as_object().unwrap();
    assert!(!info.contains_key("biography"), "{info:?}");
    assert!(!info.contains_key("musicBrainzId"), "{info:?}");
    assert_eq!(info["similarArtist"], json!([]));
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn artist_info_no_image_url_attributes(pool: PgPool) {
    let env = env(pool);
    let beatles = s(artist_id(1));
    env.sql(&format!(
        r#"UPDATE artists SET bio = 'x', "similar" = '[{{"name":"La Roux","match":0.9}}]'
           WHERE id = '{beatles}'"#
    ))
    .await;
    for (endpoint, element) in [
        ("getArtistInfo2", "artistInfo2"),
        ("getArtistInfo", "artistInfo"),
    ] {
        let body = env.ok(endpoint, &[("id", &beatles)]).await;
        let mut keys: Vec<&String> = body[element].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["biography", "coverArt", "musicBrainzId", "similarArtist"],
            "{endpoint}"
        );
        let xml = env.xml(endpoint, &[("id", &beatles)]).await;
        assert!(!xml.contains("ImageUrl"), "{xml}");
        assert!(!xml.contains("lastFmUrl"), "{xml}");
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn artist_info_count_capped_50(pool: PgPool) {
    let env = env(pool);
    // 60 library artists, each with one album and one present track, all similar to the Beatles.
    env.sql(
        "INSERT INTO artists (id, name, name_key)
         SELECT ('00000000-0000-7000-8000-c' || lpad(i::text, 11, '0'))::uuid,
                'Similar ' || i, 'similar ' || i
         FROM generate_series(1, 60) AS i",
    )
    .await;
    env.sql(
        "INSERT INTO albums (id, artist_id, title, title_key)
         SELECT ('00000000-0000-7000-8000-d' || lpad(i::text, 11, '0'))::uuid,
                ('00000000-0000-7000-8000-c' || lpad(i::text, 11, '0'))::uuid,
                'Album ' || i, 'album ' || i
         FROM generate_series(1, 60) AS i",
    )
    .await;
    env.sql(
        "INSERT INTO tracks (id, album_id, artist_name, title, disc, track_no, duration_secs,
                             codec, path, size, mtime, fingerprint)
         SELECT ('00000000-0000-7000-8000-e' || lpad(i::text, 11, '0'))::uuid,
                ('00000000-0000-7000-8000-d' || lpad(i::text, 11, '0'))::uuid,
                'Similar ' || i, 'Song ' || i, 1, 1, 100, 'flac',
                'Similar/' || i || '.flac', 1000, now(), sha256(('fp' || i)::bytea)
         FROM generate_series(1, 60) AS i",
    )
    .await;
    env.sql(&format!(
        r#"UPDATE artists SET "similar" = (
               SELECT jsonb_agg(jsonb_build_object('name', 'Similar ' || i, 'mbid', NULL,
                                                   'match', 1.0 - i / 100.0) ORDER BY i)
               FROM generate_series(1, 60) AS i)
           WHERE id = '{}'"#,
        artist_id(1)
    ))
    .await;
    let beatles = s(artist_id(1));
    let names = |body: &Value| -> Vec<String> {
        body["artistInfo2"]["similarArtist"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let expected = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("Similar {i}")).collect() };
    let body = env.ok("getArtistInfo2", &[("id", &beatles)]).await;
    assert_eq!(names(&body), expected(20), "default 20");
    for (count, n) in [("100", 50), ("50", 50), ("3", 3), ("junk", 20)] {
        let body = env
            .ok("getArtistInfo2", &[("id", &beatles), ("count", count)])
            .await;
        assert_eq!(names(&body), expected(n), "count={count}");
    }
    let first =
        &env.ok("getArtistInfo2", &[("id", &beatles)]).await["artistInfo2"]["similarArtist"][0];
    assert_eq!(first["albumCount"], 1);
    assert_eq!(first["coverArt"], "ar-00000000-0000-7000-8000-c00000000001");
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn artist_info_unknown_70_missing_10(pool: PgPool) {
    let env = env(pool);
    // Theatre (artist 4) has only a missing track: not visible, as in getArtist.
    for endpoint in ["getArtistInfo", "getArtistInfo2"] {
        for id in [
            uuid::Uuid::now_v7().to_string(),
            "nope".to_owned(),
            s(artist_id(4)),
            s(album_id(1)),
        ] {
            env.assert_error(endpoint, &[("id", &id)], 70, "Artist not found")
                .await;
        }
        env.assert_error(endpoint, &[], 10, "Required parameter is missing: id")
            .await;
    }
}

// ---- getAlbumInfo / getAlbumInfo2 ----

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn album_info_mbid_and_errors(pool: PgPool) {
    let env = env(pool);
    let abbey = s(album_id(1));
    for endpoint in ["getAlbumInfo", "getAlbumInfo2"] {
        for id in [
            abbey.clone(),
            format!("al-{abbey}"),
            format!("album:{abbey}"),
        ] {
            let body = env.ok(endpoint, &[("id", &id)]).await;
            assert_eq!(
                body["albumInfo"],
                json!({"musicBrainzId": "2a2c6d57-2f46-4b5e-9d0a-7e2f4a1d5c11"}),
                "{endpoint} {id}"
            );
        }
        // No MBID: the element is empty (`notes` has no source).
        let body = env.ok(endpoint, &[("id", &s(album_id(2)))]).await;
        assert_eq!(body["albumInfo"], json!({}));
        for id in [
            uuid::Uuid::now_v7().to_string(),
            "nope".to_owned(),
            s(album_id(6)),
            s(artist_id(1)),
        ] {
            env.assert_error(endpoint, &[("id", &id)], 70, "Album not found")
                .await;
        }
        env.assert_error(endpoint, &[], 10, "Required parameter is missing: id")
            .await;
    }
    let xml = env.xml("getAlbumInfo2", &[("id", &abbey)]).await;
    assert!(
        xml.contains(
            "<albumInfo><musicBrainzId>2a2c6d57-2f46-4b5e-9d0a-7e2f4a1d5c11</musicBrainzId></albumInfo>"
        ),
        "{xml}"
    );
}

// ---- getLyrics ----

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_lyrics_matches_title_contains_and_artist(pool: PgPool) {
    let env = env(pool);
    let plain = "Here come old flat top\nHe come groovin up slowly";
    let come_together = json!({
        "artist": "The Beatles",
        "title": "Come Together",
        "value": plain,
    });
    for params in [
        vec![("title", "come tog"), ("artist", "the beatles")],
        vec![("title", "TOGETHER"), ("artist", "  THE   BEATLES ")],
        vec![("title", "Together")],
        // Artist alone: the artist's first track.
        vec![("artist", "The Beatles")],
    ] {
        let body = env.ok("getLyrics", &params).await;
        assert_eq!(body["lyrics"], come_together, "{params:?}");
    }
    // The wrong artist, or a title nobody has, finds nothing.
    for params in [
        vec![("title", "Come Together"), ("artist", "La Roux")],
        vec![("title", "no such song")],
        vec![("artist", "Nobody")],
        // LIKE wildcards are plain text.
        vec![("title", "%")],
        vec![("title", "_")],
        vec![("title", "Come%Together")],
    ] {
        let body = env.ok("getLyrics", &params).await;
        assert_eq!(body["lyrics"], json!({}), "{params:?}");
    }
    // A track with a cached miss, or no row, has no lyrics.
    for title in ["Something", "Oh! Darling"] {
        let body = env.ok("getLyrics", &[("title", title)]).await;
        assert_eq!(body["lyrics"], json!({}), "{title}");
    }
    // Synced only: the synced text.
    env.sql(&format!(
        "INSERT INTO track_lyrics (track_id, synced_lyrics, source)
         VALUES ('{}', '[00:01.00]Bang bang <hammer> & co', 'embedded')",
        track_id(3)
    ))
    .await;
    let body = env.ok("getLyrics", &[("title", "maxwell")]).await;
    assert_eq!(body["lyrics"]["value"], "[00:01.00]Bang bang <hammer> & co");
    let xml = env.xml("getLyrics", &[("title", "maxwell")]).await;
    assert!(
        xml.contains(
            "<lyrics artist=\"The Beatles\" title=\"Maxwell&apos;s Silver Hammer\">[00:01.00]Bang bang &lt;hammer&gt; &amp; co</lyrics>"
        ),
        "{xml}"
    );
    // A missing track is not found.
    env.sql(&format!(
        "INSERT INTO track_lyrics (track_id, plain_lyrics, source)
         VALUES ('{}', 'Curtain call', 'embedded')",
        track_id(20)
    ))
    .await;
    let body = env.ok("getLyrics", &[("title", "Curtain")]).await;
    assert_eq!(body["lyrics"], json!({}));
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_lyrics_empty_when_both_missing(pool: PgPool) {
    let env = env(pool);
    for params in [
        vec![],
        vec![("title", ""), ("artist", "")],
        vec![("title", "   "), ("artist", " ")],
    ] {
        let body = env.ok("getLyrics", &params).await;
        assert_eq!(body["lyrics"], json!({}), "{params:?}");
    }
    let xml = env.xml("getLyrics", &[]).await;
    assert!(xml.contains("<lyrics/>"), "{xml}");
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn get_lyrics_never_fetches(pool: PgPool) {
    let recording = Recording::load(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../kima-ident/tests/fixtures/lrclib/lyrics.json"),
    );
    let replay = Replay::start(recording).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    let state = fixture_state(pool.clone())
        .with_identify(IdentifySwitch::new(false, true))
        .with_net(Arc::new(net));
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let env = Env {
        server: TestServer::new(kima_subsonic::router(state).layer(MockConnectInfo(peer))).unwrap(),
        pool,
    };
    // Track 4 has no lyrics row; track 2 a cached miss. Neither is looked up.
    let body = env.ok("getLyrics", &[("title", "Oh! Darling")]).await;
    assert_eq!(body["lyrics"], json!({}));
    for id in [track_id(4), track_id(2), track_id(1)] {
        env.ok("getLyricsBySongId", &[("id", &s(id))]).await;
    }
    env.ok(
        "getLyrics",
        &[("artist", "The Beatles"), ("title", "Something")],
    )
    .await;
    assert!(replay.asked().is_empty(), "asked {:?}", replay.asked());
}

// ---- getLyricsBySongId ----

fn entries(body: &Value) -> Vec<Value> {
    body["lyricsList"]["structuredLyrics"]
        .as_array()
        .unwrap_or_else(|| panic!("no structuredLyrics list: {body}"))
        .clone()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn lyrics_by_song_id_synced_lines_ms(pool: PgPool) {
    let env = env(pool);
    let t1 = s(track_id(1));
    env.sql(&format!(
        "UPDATE track_lyrics SET plain_lyrics = NULL, synced_lyrics = E'{}' WHERE track_id = '{t1}'",
        [
            "[1:02.5]one",
            "[01:02.05]two",
            "[01:02.005]three",
            "  [00:03]   four  ",
            "not a line",
            "[99999999999:99]huge",
            "[100:00.00]three-digit minutes",
            "[00:5.00]one-digit seconds",
            "[00:05.0000]four-digit fraction",
            "[]",
            "[:]",
            "",
            "[00:06.00]<b>&amp;</b>",
        ]
        .join("\\n")
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t1)]).await;
    let list = entries(&body);
    assert_eq!(list.len(), 1, "{body}");
    assert_eq!(
        list[0],
        json!({
            "displayArtist": "The Beatles",
            "displayTitle": "Come Together",
            "lang": "und",
            "synced": true,
            "line": [
                {"start": 62500, "value": "one"},
                {"start": 62050, "value": "two"},
                {"start": 62005, "value": "three"},
                {"start": 3000, "value": "four"},
                {"start": 6000, "value": "<b>&amp;</b>"},
            ],
        })
    );
    let xml = env.xml("getLyricsBySongId", &[("id", &t1)]).await;
    assert!(
        xml.contains("<line start=\"6000\">&lt;b&gt;&amp;amp;&lt;/b&gt;</line>"),
        "{xml}"
    );

    // The fixture's row: synced then plain, both from the same row.
    env.sql(&format!(
        "UPDATE track_lyrics SET plain_lyrics = E'  Here come old flat top \\n\\n  He come groovin  ',
                                 synced_lyrics = E'[00:01.50]Here come old flat top\\n[00:05.00]He come'
         WHERE track_id = '{t1}'"
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t1)]).await;
    let list = entries(&body);
    assert_eq!(list.len(), 2, "{body}");
    assert_eq!(list[0]["synced"], true);
    assert_eq!(
        list[0]["line"],
        json!([{"start": 1500, "value": "Here come old flat top"}, {"start": 5000, "value": "He come"}])
    );
    assert_eq!(list[1]["synced"], false);

    // Hostile input: 256 KiB of timestamps never panics, and the lines are bounded.
    let many = "[59:59.999]x\\n".repeat(256 * 1024 / 13);
    env.sql(&format!(
        "UPDATE track_lyrics SET plain_lyrics = NULL, synced_lyrics = E'{many}' WHERE track_id = '{t1}'"
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t1)]).await;
    let list = entries(&body);
    let lines = list[0]["line"].as_array().unwrap();
    assert_eq!(lines.len(), 5000);
    assert_eq!(lines[0], json!({"start": 3_599_999, "value": "x"}));
    // A long run of brackets on one line is one unmatched line.
    let brackets = "[".repeat(256 * 1024);
    env.sql(&format!(
        "UPDATE track_lyrics SET synced_lyrics = '{brackets}' WHERE track_id = '{t1}'"
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t1)]).await;
    assert_eq!(entries(&body), Vec::<Value>::new());
}

/// Final review F2: a missing track's lyrics are not served (an empty list, as `getSong`
/// answers it not found).
#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn lyrics_by_song_id_missing_track_is_empty(pool: PgPool) {
    let env = env(pool);
    let t1 = s(track_id(1));
    let body = env.ok("getLyricsBySongId", &[("id", &t1)]).await;
    assert!(!entries(&body).is_empty(), "{body}");
    env.sql(&format!(
        "UPDATE tracks SET missing_since = now() WHERE id = '{t1}'"
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t1)]).await;
    assert_eq!(entries(&body), Vec::<Value>::new());
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn lyrics_by_song_id_plain_lines(pool: PgPool) {
    let env = env(pool);
    let t10 = s(track_id(10));
    env.sql(&format!(
        "INSERT INTO track_lyrics (track_id, plain_lyrics, source)
         VALUES ('{t10}', E'  Going in for the kill  \\r\\n\\n\\t\\nDoing it for a thrill\\n', 'embedded')"
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t10)]).await;
    assert_eq!(
        entries(&body),
        vec![json!({
            "displayArtist": "La Roux",
            "displayTitle": "In for the Kill",
            "lang": "und",
            "synced": false,
            "line": [{"value": "Going in for the kill"}, {"value": "Doing it for a thrill"}],
        })]
    );
    // Synced text with no line that parses gives no synced entry.
    env.sql(&format!(
        "UPDATE track_lyrics SET synced_lyrics = 'just words' WHERE track_id = '{t10}'"
    ))
    .await;
    let body = env.ok("getLyricsBySongId", &[("id", &t10)]).await;
    let list = entries(&body);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["synced"], false);
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn lyrics_by_song_id_none_row_empty_list(pool: PgPool) {
    let env = env(pool);
    // A cached miss, no row, an unknown track and a non-id are all an empty list.
    for id in [
        s(track_id(2)),
        s(track_id(4)),
        uuid::Uuid::now_v7().to_string(),
        "banana".to_owned(),
    ] {
        let body = env.ok("getLyricsBySongId", &[("id", &id)]).await;
        assert_eq!(body["lyricsList"], json!({"structuredLyrics": []}), "{id}");
    }
    let xml = env
        .xml("getLyricsBySongId", &[("id", &s(track_id(2)))])
        .await;
    assert!(xml.contains("<lyricsList/>"), "{xml}");
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn lyrics_by_song_id_missing_10(pool: PgPool) {
    let env = env(pool);
    env.assert_error(
        "getLyricsBySongId",
        &[],
        10,
        "Required parameter is missing: id",
    )
    .await;
    env.assert_error(
        "getLyricsBySongId",
        &[("id", "")],
        10,
        "Required parameter is missing: id",
    )
    .await;
}
