//! Media endpoints on the SQL fixture (spec §4.4, §10.2): stream, download, hls, transcode
//! decision and stream, cover art and avatar. The audio is made by ffmpeg under the fixture's
//! temporary library root.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::extract::connect_info::MockConnectInfo;
use axum::http::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HOST,
    IF_NONE_MATCH, RANGE,
};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum_test::{TestResponse, TestServer};
use common::audio::{require_ffmpeg, sine};
use common::*;
use kima_db::PgPool;
use kima_library::transcode::Transcoder;
use serde_json::{Value, json};

/// Track 1: Abbey Road, flac, bitrate unknown.
const FLAC_REL: &str = "The Beatles/Abbey Road/01 Come Together.flac";

struct Env {
    server: TestServer,
    root: PathBuf,
    data: PathBuf,
    // Kept alive for the data and transcode directories.
    _dirs: Vec<tempfile::TempDir>,
}

fn env_with(pool: PgPool, public_url: Option<&str>) -> Env {
    let data = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let state = fixture_state(pool)
        .with_data_dir(data.path().to_path_buf())
        .with_transcoder(Transcoder::new(cache.path(), 100 * 1024 * 1024))
        .with_public_url(public_url.map(str::to_owned));
    let root = state.library_root().unwrap();
    let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
    let router = kima_subsonic::router(state.clone()).layer(MockConnectInfo(peer));
    Env {
        server: TestServer::new(router).unwrap(),
        root,
        data: data.path().to_path_buf(),
        _dirs: vec![data, cache],
    }
}

fn env(pool: PgPool) -> Env {
    env_with(pool, None)
}

impl Env {
    /// Makes a real audio file for a fixture track and returns its bytes.
    fn audio(&self, rel: &str) -> Vec<u8> {
        let path = self.root.join(rel);
        sine(&path);
        std::fs::read(path).unwrap()
    }

    /// GETs `/{endpoint}.view` as alice (app token) with the given parameters.
    async fn get(&self, endpoint: &str, params: &[(&str, &str)]) -> TestResponse {
        self.server
            .get(&format!("/{endpoint}.view"))
            .add_query_param("apiKey", FIXTURE_TOKEN)
            .add_query_params(params)
            .await
    }

    /// The `subsonic-response` object of an answer asked for as JSON.
    async fn json(&self, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let mut all = vec![("f", "json")];
        all.extend_from_slice(params);
        let res = self.get(endpoint, &all).await;
        res.assert_status_ok();
        res.json::<Value>()["subsonic-response"].clone()
    }

    /// Asserts a Subsonic error (HTTP 200, JSON envelope) with the code and message.
    async fn assert_error(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        code: u32,
        message: &str,
    ) {
        let body = self.json(endpoint, params).await;
        assert_eq!(body["status"], "failed", "{endpoint} {params:?}: {body}");
        assert_eq!(body["error"]["code"], code, "{endpoint} {params:?}: {body}");
        assert_eq!(body["error"]["message"], message, "{endpoint} {params:?}");
    }
}

fn id(n: u128) -> String {
    track_id(n).to_string()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn stream_original_ranges_and_etag(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    let data = env.audio(FLAC_REL);
    let t1 = id(1);

    let res = env.get("stream", &[("id", &t1)]).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(res.header(CONTENT_TYPE), "audio/flac");
    assert_eq!(res.header(ACCEPT_RANGES), "bytes");
    let etag = res.header(ETAG).to_str().unwrap().to_owned();

    // `format=raw` and a `maxBitRate` at or above the unknown source are the original too.
    let raw = env
        .get(
            "stream",
            &[("id", &t1), ("format", "raw"), ("maxBitRate", "64")],
        )
        .await;
    assert_eq!(raw.as_bytes().as_ref(), &data[..]);
    let wide = env.get("stream", &[("id", &t1), ("maxBitRate", "0")]).await;
    assert_eq!(wide.as_bytes().as_ref(), &data[..]);

    let part = env
        .server
        .get("/stream.view")
        .add_query_params([("apiKey", FIXTURE_TOKEN), ("id", t1.as_str())])
        .add_header(RANGE, "bytes=10-19")
        .await;
    part.assert_status(StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.as_bytes().as_ref(), &data[10..20]);
    assert_eq!(
        part.header(CONTENT_RANGE).to_str().unwrap(),
        format!("bytes 10-19/{}", data.len())
    );

    let past = env
        .server
        .get("/stream.view")
        .add_query_params([("apiKey", FIXTURE_TOKEN), ("id", t1.as_str())])
        .add_header(RANGE, format!("bytes={}-", data.len() + 5))
        .await;
    past.assert_status(StatusCode::RANGE_NOT_SATISFIABLE);

    let cached = env
        .server
        .get("/stream.view")
        .add_query_params([("apiKey", FIXTURE_TOKEN), ("id", t1.as_str())])
        .add_header(IF_NONE_MATCH, etag)
        .await;
    cached.assert_status(StatusCode::NOT_MODIFIED);
    assert!(cached.as_bytes().is_empty());
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn stream_max_bit_rate_128_is_mpeg(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    let original = env.audio(FLAC_REL);
    let res = env
        .get("stream", &[("id", &id(1)), ("maxBitRate", "128")])
        .await;
    res.assert_status_ok();
    assert_eq!(res.header(CONTENT_TYPE), "audio/mpeg");
    let body = res.as_bytes();
    assert!(body.len() > 1000, "got {} bytes", body.len());
    assert_ne!(body.as_ref(), &original[..]);
    // An MP3 starts with an ID3 tag or a frame sync.
    assert!(
        body.starts_with(b"ID3") || (body[0] == 0xff && body[1] & 0xe0 == 0xe0),
        "not mpeg: {:02x?}",
        &body[..4]
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn stream_unknown_or_missing_track_70_in_envelope_http_200(pool: PgPool) {
    let env = env(pool);
    let unknown = uuid::Uuid::now_v7().to_string();
    // Track 20 is flagged missing; track 2 is present in the database but has no file.
    for bad in [
        unknown.as_str(),
        id(20).as_str(),
        id(2).as_str(),
        "not-a-uuid",
    ] {
        env.assert_error("stream", &[("id", bad)], 70, "Song not found")
            .await;
    }
    env.assert_error("stream", &[], 10, "Required parameter is missing: id")
        .await;

    // The same error in XML, still HTTP 200.
    let res = env.get("stream", &[("id", &unknown)]).await;
    res.assert_status_ok();
    assert!(
        res.text()
            .contains("<error code=\"70\" message=\"Song not found\"/>")
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn stream_logs_no_play(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool.clone());
    env.audio(FLAC_REL);
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM plays")
        .fetch_one(&pool)
        .await
        .unwrap();
    env.get("stream", &[("id", &id(1))])
        .await
        .assert_status_ok();
    env.get("download", &[("id", &id(1))])
        .await
        .assert_status_ok();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM plays")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn head_stream_has_length_no_body(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    let data = env.audio(FLAC_REL);
    let res = env
        .server
        .method(Method::HEAD, "/stream.view")
        .add_query_params([("apiKey", FIXTURE_TOKEN), ("id", id(1).as_str())])
        .await;
    res.assert_status_ok();
    assert_eq!(
        res.header(CONTENT_LENGTH).to_str().unwrap(),
        data.len().to_string()
    );
    assert!(res.as_bytes().is_empty());
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn download_is_original_with_disposition(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    let data = env.audio(FLAC_REL);
    // Quality parameters do not apply to a download.
    let res = env
        .get(
            "download",
            &[("id", &id(1)), ("maxBitRate", "64"), ("format", "mp3")],
        )
        .await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    assert_eq!(
        res.header(CONTENT_DISPOSITION),
        "attachment; filename*=UTF-8''01%20Come%20Together.flac"
    );
    env.assert_error("download", &[("id", &id(20))], 70, "Song not found")
        .await;
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn download_non_ascii_name(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let name = "Bj\u{f6}rk \u{2013} J\u{f3}ga \"live\"; take 2.flac";
    let rel = format!("Bj\u{f6}rk/{name}");
    sqlx::query("UPDATE tracks SET path = $1 WHERE id = $2")
        .bind(&rel)
        .bind(track_id(1))
        .execute(&pool)
        .await
        .unwrap();
    let env = env(pool);
    let data = env.audio(&rel);
    let res = env.get("download", &[("id", &id(1))]).await;
    res.assert_status_ok();
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    let header = res.header(CONTENT_DISPOSITION);
    let header = header.to_str().expect("ASCII header");
    assert!(header.is_ascii());
    let tail = header
        .strip_prefix("attachment; filename*=UTF-8''")
        .expect("prefix");
    assert!(
        !tail.contains('"') && !tail.contains(';') && !tail.contains(' '),
        "{tail}"
    );
    assert_eq!(percent_decode(tail), name);
}

/// The stream URL line of an `hls` playlist, after checking the other lines.
fn playlist_url(res: &TestResponse, id: &str) -> String {
    assert_eq!(res.header(CONTENT_TYPE), "application/vnd.apple.mpegurl");
    let text = res.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(lines[0], "#EXTM3U");
    assert_eq!(lines[1], "#EXT-X-VERSION:3");
    assert_eq!(lines[2], format!("#EXTINF:-1,{id}"));
    lines[3].to_owned()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn hls_points_at_stream_view_with_auth_and_bitrate(pool: PgPool) {
    let env = env(pool);
    let t1 = id(1);
    // The first `bitRate` wins, without its `@size`; the host comes from the request and
    // `X-Forwarded-Proto` is not trusted.
    let res = env
        .server
        .get("/hls.view")
        .add_query_params([
            ("id", t1.as_str()),
            ("bitRate", "128@640x480"),
            ("bitRate", "64"),
            ("u", ALICE_NAME),
            ("p", ALICE_PASSWORD),
            ("v", "1.16.1"),
            ("c", "tests"),
            ("f", "json"),
        ])
        .add_header(HOST, "kima.test:8080")
        .add_header(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        )
        .await;
    res.assert_status_ok();
    assert_eq!(
        playlist_url(&res, &t1),
        format!(
            "http://kima.test:8080/rest/stream.view?id={t1}&maxBitRate=128\
             &u=alice&p=alicepass1&v=1.16.1&c=tests&f=json"
        )
    );

    // `hls.m3u8` is the same answer; an app token travels as `apiKey`; no bitrate, no `maxBitRate`.
    let alias = env
        .server
        .get("/hls.m3u8")
        .add_query_params([("id", t1.as_str()), ("apiKey", FIXTURE_TOKEN)])
        .add_header(HOST, "kima.test")
        .await;
    alias.assert_status_ok();
    assert_eq!(
        playlist_url(&alias, &t1),
        format!("http://kima.test/rest/stream.view?id={t1}&apiKey={FIXTURE_TOKEN}")
    );

    env.assert_error("hls", &[], 10, "Required parameter is missing: id")
        .await;
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn hls_uses_public_url_when_set(pool: PgPool) {
    let env = env_with(pool, Some("https://kima.example"));
    let t1 = id(1);
    let res = env
        .server
        .get("/hls.view")
        .add_query_params([("id", t1.as_str()), ("apiKey", FIXTURE_TOKEN)])
        .add_header(HOST, "ignored.test")
        .await;
    assert_eq!(
        playlist_url(&res, &t1),
        format!("https://kima.example/rest/stream.view?id={t1}&apiKey={FIXTURE_TOKEN}")
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn transcode_decision_direct_play_and_format_change(pool: PgPool) {
    let env = env(pool);
    let t1 = id(1);
    for params in [
        vec![("mediaId", t1.as_str()), ("mediaType", "song")],
        vec![
            ("mediaId", t1.as_str()),
            ("mediaType", "song"),
            ("format", "flac"),
        ],
        vec![
            ("mediaId", t1.as_str()),
            ("mediaType", "song"),
            ("format", "raw"),
        ],
    ] {
        let body = env.json("getTranscodeDecision", &params).await;
        assert_eq!(body["status"], "ok", "{body}");
        assert_eq!(
            body["transcodeDecision"],
            json!({
                "canDirectPlay": true,
                "canTranscode": true,
                "errorReason": "",
                "transcodeParams": "q-original",
                "transcodeReason": [],
            }),
            "{params:?}"
        );
    }
    let body = env
        .json(
            "getTranscodeDecision",
            &[("mediaId", &t1), ("mediaType", "song"), ("format", "mp3")],
        )
        .await;
    assert_eq!(
        body["transcodeDecision"],
        json!({
            "canDirectPlay": false,
            "canTranscode": true,
            "errorReason": "",
            "transcodeParams": "mp3-high",
            "transcodeReason": ["source is flac, client asked for mp3"],
        })
    );
    env.assert_error(
        "getTranscodeDecision",
        &[("mediaType", "song")],
        10,
        "Required parameter is missing: mediaId",
    )
    .await;
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn transcode_decision_non_song_70(pool: PgPool) {
    let env = env(pool);
    let t1 = id(1);
    for kind in ["podcast", "album", "x"] {
        env.assert_error(
            "getTranscodeDecision",
            &[("mediaId", &t1), ("mediaType", kind)],
            70,
            "Media not found",
        )
        .await;
    }
    env.assert_error(
        "getTranscodeDecision",
        &[("mediaId", &id(20)), ("mediaType", "song")],
        70,
        "Media not found",
    )
    .await;
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn transcode_stream_song_podcast_other(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool.clone());
    let data = env.audio(FLAC_REL);
    let t1 = id(1);
    let song = env
        .get(
            "getTranscodeStream",
            &[
                ("mediaId", &t1),
                ("mediaType", "song"),
                ("transcodeParams", "q-original"),
            ],
        )
        .await;
    song.assert_status_ok();
    assert_eq!(song.as_bytes().as_ref(), &data[..]);

    let low = env
        .get(
            "getTranscodeStream",
            &[
                ("mediaId", &t1),
                ("mediaType", "SONG"),
                ("transcodeParams", "q-low-128"),
            ],
        )
        .await;
    assert_eq!(low.header(CONTENT_TYPE), "audio/mpeg");

    env.assert_error(
        "getTranscodeStream",
        &[
            ("mediaId", &t1),
            ("mediaType", "podcast"),
            ("transcodeParams", "x"),
        ],
        70,
        "Podcast episode not found",
    )
    .await;

    // A real episode of a show alice follows: a redirect to its own audio URL, regardless of
    // `transcodeParams` (D-101/D-167 amended: podcasts are always direct play of the feed).
    let show = kima_db::podcasts::insert_show(
        &pool,
        uuid::Uuid::now_v7(),
        &kima_db::podcasts::NewShow {
            feed_url: "https://feed.example/show.xml",
            title: "A Show",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 1,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    kima_db::podcasts::insert_episodes(
        &pool,
        show.id,
        &[kima_db::podcasts::NewEpisode {
            guid: "ep-1".into(),
            title: "Episode 1".into(),
            description: None,
            audio_url: "https://feed.example/ep1.mp3".into(),
            duration: 120,
            published_at: time::OffsetDateTime::now_utc(),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        }],
    )
    .await
    .unwrap();
    kima_db::podcasts::subscribe(&pool, ALICE_ID, show.id)
        .await
        .unwrap();
    let episode = kima_db::podcasts::episodes_for_channel(&pool, show.id, 1, false)
        .await
        .unwrap()
        .remove(0);
    let redirect = env
        .get(
            "getTranscodeStream",
            &[
                ("mediaId", &episode.id.to_string()),
                ("mediaType", "podcast"),
                ("transcodeParams", "anything"),
            ],
        )
        .await;
    redirect.assert_status(StatusCode::FOUND);
    assert_eq!(
        redirect.header(axum::http::header::LOCATION),
        "https://feed.example/ep1.mp3"
    );
    env.assert_error(
        "getTranscodeStream",
        &[
            ("mediaId", &t1),
            ("mediaType", "album"),
            ("transcodeParams", "x"),
        ],
        0,
        "Unsupported mediaType",
    )
    .await;
    env.assert_error(
        "getTranscodeStream",
        &[
            ("mediaId", &id(20)),
            ("mediaType", "song"),
            ("transcodeParams", "q-original"),
        ],
        70,
        "Song not found",
    )
    .await;
    env.assert_error(
        "getTranscodeStream",
        &[("mediaId", &t1), ("mediaType", "song")],
        10,
        "Required parameter is missing: transcodeParams",
    )
    .await;
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Abbey Road (album 1, tracks 1-5) gets a 400x200 folder image.
fn abbey_road_cover(env: &Env) {
    let dir = env.root.join("The Beatles/Abbey Road");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cover.png"), png(400, 200)).unwrap();
}

fn dims(res: &TestResponse) -> (u32, u32) {
    let img = image::load_from_memory(res.as_bytes()).unwrap();
    (img.width(), img.height())
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn cover_art_album_track_and_prefixes(pool: PgPool) {
    let env = env(pool);
    abbey_road_cover(&env);
    let album = album_id(1).to_string();
    let track = id(3);
    for raw in [
        album.clone(),
        format!("al-{album}"),
        format!("album:{album}"),
        track.clone(),
        format!("tr-{track}"),
    ] {
        let res = env.get("getCoverArt", &[("id", &raw)]).await;
        res.assert_status_ok();
        assert_eq!(res.header(CONTENT_TYPE), "image/jpeg", "{raw}");
        assert_eq!(dims(&res), (400, 200), "{raw}");
    }
    // The prefix says what the id is: a track id behind `al-` is no album.
    env.assert_error(
        "getCoverArt",
        &[("id", &format!("al-{track}"))],
        70,
        "Cover art not found",
    )
    .await;
    env.assert_error(
        "getCoverArt",
        &[("id", &format!("tr-{album}"))],
        70,
        "Cover art not found",
    )
    .await;
    env.assert_error("getCoverArt", &[], 10, "Required parameter is missing: id")
        .await;
    // A junk size is ignored like a paging parameter: the id still decides the answer.
    env.assert_error(
        "getCoverArt",
        &[("id", "x"), ("size", "abc")],
        70,
        "Cover art not found",
    )
    .await;
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn cover_art_size_snaps(pool: PgPool) {
    let env = env(pool);
    abbey_road_cover(&env);
    let album = album_id(1).to_string();
    let res = env
        .get("getCoverArt", &[("id", &album), ("size", "100")])
        .await;
    res.assert_status_ok();
    assert_eq!(dims(&res), (128, 64));
    let big = env
        .get("getCoverArt", &[("id", &album), ("size", "5000")])
        .await;
    assert_eq!(dims(&big), (400, 200));
    let zero = env
        .get("getCoverArt", &[("id", &album), ("size", "0")])
        .await;
    assert_eq!(dims(&zero), (400, 200));
    // Junk falls back to the original size, as upstream (which ignores `size`) and Navidrome.
    for junk in ["abc", "10.5", "-5"] {
        let res = env
            .get("getCoverArt", &[("id", &album), ("size", junk)])
            .await;
        res.assert_status_ok();
        assert_eq!(dims(&res), (400, 200), "{junk}");
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn cover_art_artist_without_image_70(pool: PgPool) {
    let env = env(pool);
    abbey_road_cover(&env);
    // Artist 1 has an `image_path` but no file under the data folder; artist 2 has none at all.
    for artist in [artist_id(1).to_string(), artist_id(2).to_string()] {
        for raw in [
            format!("ar-{artist}"),
            format!("artist:{artist}"),
            artist.clone(),
        ] {
            env.assert_error("getCoverArt", &[("id", &raw)], 70, "Cover art not found")
                .await;
        }
    }
    // An artist prefix never serves an album's cover.
    let album = album_id(1).to_string();
    env.assert_error(
        "getCoverArt",
        &[("id", &format!("ar-{album}"))],
        70,
        "Cover art not found",
    )
    .await;
    // An artist cover is an error in the envelope, not an HTTP failure or an image.
    let res = env
        .get("getCoverArt", &[("id", &format!("ar-{}", artist_id(2)))])
        .await;
    res.assert_status_ok();
    assert!(res.text().contains("<error code=\"70\""));
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn cover_art_artist_prefix_and_bare_id(pool: PgPool) {
    let env = env(pool);
    let artist = artist_id(1);
    let path = kima_library::artwork::artist_image_path(&env.data, artist);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let img = image::RgbImage::from_pixel(600, 300, image::Rgb([10, 200, 30]));
    img.save_with_format(&path, image::ImageFormat::Jpeg)
        .unwrap();
    let artist = artist.to_string();
    for raw in [
        format!("ar-{artist}"),
        format!("artist:{artist}"),
        artist.clone(),
    ] {
        let res = env.get("getCoverArt", &[("id", &raw)]).await;
        res.assert_status_ok();
        assert_eq!(res.header(CONTENT_TYPE), "image/jpeg", "{raw}");
        assert_eq!(res.header("cache-control"), "private, max-age=86400");
        assert_eq!(dims(&res), (600, 300), "{raw}");
        // `size` snaps as for albums: the long side fits the bucket.
        let small = env
            .get("getCoverArt", &[("id", &raw), ("size", "100")])
            .await;
        small.assert_status_ok();
        let (w, h) = dims(&small);
        assert!(w <= 150 && h <= 150 && w > h, "{raw}: {w}x{h}");
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn cover_art_none_70(pool: PgPool) {
    let env = env(pool);
    // Album 3 has no image; neither does an unknown id, or a non-id.
    for raw in [
        album_id(3).to_string(),
        uuid::Uuid::now_v7().to_string(),
        "nonsense".to_owned(),
    ] {
        env.assert_error("getCoverArt", &[("id", &raw)], 70, "Cover art not found")
            .await;
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn avatar_known_unknown_missing(pool: PgPool) {
    let env = env(pool);
    let res = env.get("getAvatar", &[("username", "alice")]).await;
    res.assert_status_ok();
    assert_eq!(res.header(CONTENT_TYPE), "image/png");
    assert_eq!(res.header("cache-control"), "private, max-age=3600");
    let img = image::load_from_memory(res.as_bytes()).unwrap().to_rgba8();
    assert_eq!((img.width(), img.height()), (1, 1));
    assert_eq!(img.get_pixel(0, 0).0[3], 0);
    // Names match without regard to case, as sign-in does.
    env.get("getAvatar", &[("username", "ADMIN")])
        .await
        .assert_status_ok();
    env.assert_error("getAvatar", &[("username", "nobody")], 70, "User not found")
        .await;
    env.assert_error(
        "getAvatar",
        &[],
        10,
        "Required parameter is missing: username",
    )
    .await;
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn media_needs_auth(pool: PgPool) {
    let env = env(pool);
    let res = env
        .server
        .get("/stream.view")
        .add_query_params([("f", "json"), ("id", id(1).as_str())])
        .await;
    res.assert_status_ok();
    let body = res.json::<Value>();
    assert_eq!(body["subsonic-response"]["error"]["code"], 10);
}

// M-SYNC Task 10 (upstream `78dc8f1d`): track 2 has a row and no file, not flagged missing. It
// answers error 70 with the fixed message at every bit rate, never a path or a server error.
#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn stream_file_gone_before_the_scan_flags_it_is_70_without_a_path(pool: PgPool) {
    let env = env(pool);
    let t2 = id(2);
    for extra in [
        vec![],
        vec![("format", "raw")],
        vec![("maxBitRate", "64")],
        vec![("maxBitRate", "128"), ("format", "mp3")],
    ] {
        let mut params = vec![("id", t2.as_str())];
        params.extend(extra.iter().copied());
        env.assert_error("stream", &params, 70, "Song not found")
            .await;
    }
}

// ---- Task 16: one stream plan (upstream `transcodeDecisionStream.route.test.ts`) ----

const M4A_REL: &str = "The Beatles/Abbey Road/02 Something.m4a";

impl Env {
    /// Gives a fixture track a stored bitrate (kbps, `None` = never probed).
    async fn set_bitrate(&self, pool: &PgPool, track: u128, kbps: Option<i32>) {
        sqlx::query("UPDATE tracks SET bitrate = $2 WHERE id = $1")
            .bind(track_id(track))
            .bind(kbps)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Track 2 becomes a real `.m4a` of the given stored bitrate.
    async fn make_m4a(&self, pool: &PgPool, kbps: Option<i32>) {
        sqlx::query("UPDATE tracks SET path = $2, bitrate = $3, codec = 'aac' WHERE id = $1")
            .bind(track_id(2))
            .bind(M4A_REL)
            .bind(kbps)
            .execute(pool)
            .await
            .unwrap();
        self.audio(M4A_REL);
    }

    async fn decision(&self, song: u128, params: &[(&str, &str)]) -> Value {
        let song = id(song);
        let mut all = vec![("mediaId", song.as_str()), ("mediaType", "song")];
        all.extend_from_slice(params);
        let body = self.json("getTranscodeDecision", &all).await;
        assert_eq!(body["status"], "ok", "{body}");
        body["transcodeDecision"].clone()
    }

    async fn content_type(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        let res = self.get(endpoint, params).await;
        res.assert_status_ok();
        res.header(CONTENT_TYPE).to_str().unwrap().to_owned()
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn direct_play_gets_a_transcode_params_value_and_the_stream_serves_the_file_as_it_is(
    pool: PgPool,
) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    let data = env.audio(FLAC_REL);
    let t1 = id(1);
    for q in [
        vec![],
        vec![("format", "flac")],
        vec![("format", "raw"), ("maxBitRate", "128")],
    ] {
        let d = env.decision(1, &q).await;
        assert_eq!(d["canDirectPlay"], true, "{q:?}");
        assert_eq!(d["canTranscode"], true, "{q:?}");
        assert_eq!(d["transcodeParams"], "q-original", "{q:?}");
        assert_eq!(d["transcodeReason"], json!([]), "{q:?}");
        let mut stream = vec![("id", t1.as_str())];
        stream.extend_from_slice(&q);
        let res = env.get("stream", &stream).await;
        assert_eq!(res.header(CONTENT_TYPE), "audio/flac", "{q:?}");
        assert_eq!(res.as_bytes().as_ref(), &data[..], "{q:?}");
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn a_bitrate_cap_is_never_exceeded_a_file_over_it_gets_the_highest_mp3_tier_within_it(
    pool: PgPool,
) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool.clone());
    env.audio(FLAC_REL);
    env.set_bitrate(&pool, 1, Some(900)).await;
    let t1 = id(1);
    for (cap, params) in [
        ("320", "q-high-320"),
        ("256", "q-medium-256"),
        ("128", "q-low-128"),
    ] {
        let d = env.decision(1, &[("maxBitRate", cap)]).await;
        assert_eq!(d["canDirectPlay"], false, "{cap}");
        assert_eq!(d["canTranscode"], true, "{cap}");
        assert_eq!(d["transcodeParams"], params, "{cap}");
        assert_eq!(
            d["transcodeReason"],
            json!([format!("client capped the bitrate at {cap}")])
        );
        assert_eq!(
            env.content_type("stream", &[("id", &t1), ("maxBitRate", cap)])
                .await,
            "audio/mpeg",
            "{cap}"
        );
        assert_eq!(
            env.content_type(
                "getTranscodeStream",
                &[
                    ("mediaId", &t1),
                    ("mediaType", "song"),
                    ("transcodeParams", params)
                ]
            )
            .await,
            "audio/mpeg",
            "{params}"
        );
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn a_cap_below_the_smallest_mp3_is_refused_unless_the_file_already_fits(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool.clone());
    env.audio(FLAC_REL);
    env.set_bitrate(&pool, 1, Some(900)).await;
    let d = env.decision(1, &[("maxBitRate", "64")]).await;
    assert_eq!(d["canDirectPlay"], false);
    assert_eq!(d["canTranscode"], false);
    assert!(d.get("transcodeParams").is_none(), "{d}");
    assert!(
        d["errorReason"].as_str().unwrap().contains("smallest mp3"),
        "{d}"
    );
    // The stream is not an error: the smallest mp3.
    assert_eq!(
        env.content_type("stream", &[("id", &id(1)), ("maxBitRate", "64")])
            .await,
        "audio/mpeg"
    );
    // A file already within a tiny cap plays as it is.
    env.set_bitrate(&pool, 1, Some(48)).await;
    let d = env.decision(1, &[("maxBitRate", "64")]).await;
    assert_eq!(d["canDirectPlay"], true);
    assert_eq!(d["transcodeParams"], "q-original");
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn a_file_already_within_the_cap_plays_as_it_is(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool.clone());
    env.audio(FLAC_REL);
    env.set_bitrate(&pool, 1, Some(900)).await;
    let d = env.decision(1, &[("maxBitRate", "1000")]).await;
    assert_eq!(d["canDirectPlay"], true);
    assert_eq!(d["transcodeParams"], "q-original");
    let d = env.decision(1, &[("maxBitRate", "500")]).await;
    assert_eq!(d["canDirectPlay"], false);
    assert_eq!(d["transcodeParams"], "q-high-500");
    assert_eq!(
        env.content_type("stream", &[("id", &id(1)), ("maxBitRate", "500")])
            .await,
        "audio/mpeg"
    );

    env.make_m4a(&pool, Some(64)).await;
    let t2 = id(2);
    for q in [
        vec![("maxBitRate", "128")],
        vec![("format", "m4a"), ("maxBitRate", "320")],
    ] {
        let d = env.decision(2, &q).await;
        assert_eq!(d["canDirectPlay"], true, "{q:?}");
        assert_eq!(d["transcodeParams"], "q-original", "{q:?}");
        let mut stream = vec![("id", t2.as_str())];
        stream.extend_from_slice(&q);
        assert_eq!(
            env.content_type("stream", &stream).await,
            "audio/mp4",
            "{q:?}"
        );
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn a_file_whose_bitrate_cannot_be_read_is_made_into_an_mp3_under_a_cap(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    env.audio(FLAC_REL);
    // Track 1's bitrate is unknown (NULL) in the fixture.
    let d = env.decision(1, &[("maxBitRate", "1000")]).await;
    assert_eq!(d["canDirectPlay"], false);
    assert_eq!(d["transcodeParams"], "q-high-1000");
    assert_eq!(
        env.content_type("stream", &[("id", &id(1)), ("maxBitRate", "1000")])
            .await,
        "audio/mpeg"
    );
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn an_asked_for_mp3_is_an_mp3_even_when_the_file_is_already_under_the_cap(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool.clone());
    let flac = env.audio(FLAC_REL);
    env.set_bitrate(&pool, 1, Some(900)).await;
    let d = env.decision(1, &[("format", "mp3")]).await;
    assert_eq!(d["transcodeParams"], "mp3-high");

    env.make_m4a(&pool, Some(64)).await;
    let d = env
        .decision(2, &[("format", "mp3"), ("maxBitRate", "128")])
        .await;
    assert_eq!(d["canDirectPlay"], false);
    assert_eq!(d["transcodeParams"], "mp3-low-128");
    assert_eq!(
        env.content_type(
            "stream",
            &[("id", &id(2)), ("format", "mp3"), ("maxBitRate", "128")]
        )
        .await,
        "audio/mpeg"
    );

    // A cached mp3 does not change a later no-format decision, nor what it delivers.
    env.content_type("stream", &[("id", &id(1)), ("format", "mp3")])
        .await;
    let d = env.decision(1, &[]).await;
    assert_eq!(d["transcodeParams"], "q-original");
    let res = env.get("stream", &[("id", &id(1))]).await;
    assert_eq!(res.as_bytes().as_ref(), &flac[..]);

    // An mp3 file asked as mp3 over the cap is remade at the cap; without a cap it is the file.
    env.audio("The Beatles/White Album/1-01 Back in the U.S.S.R..mp3");
    let d = env
        .decision(6, &[("format", "mp3"), ("maxBitRate", "128")])
        .await;
    assert_eq!(d["transcodeParams"], "mp3-low-128");
    let d = env.decision(6, &[("format", "mp3")]).await;
    assert_eq!(d["canDirectPlay"], true);
    assert_eq!(d["transcodeParams"], "q-original");
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn a_format_kima_cannot_make_is_refused_in_the_decision_with_no_params(pool: PgPool) {
    let env = env(pool.clone());
    env.make_m4a(&pool, Some(64)).await;
    for (song, q, text) in [
        (1, vec![("format", "opus")], "only to mp3"),
        (
            1,
            vec![("format", "flac"), ("maxBitRate", "128")],
            "cannot make flac smaller",
        ),
        (2, vec![("format", "aac")], "cannot make aac"),
    ] {
        let d = env.decision(song, &q).await;
        assert_eq!(d["canDirectPlay"], false, "{q:?}");
        assert_eq!(d["canTranscode"], false, "{q:?}");
        assert!(d.get("transcodeParams").is_none(), "{d}");
        assert!(d["errorReason"].as_str().unwrap().contains(text), "{d}");
        assert_eq!(d["transcodeReason"].as_array().unwrap().len(), 1, "{d}");
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn stream_download_and_hls_follow_the_same_rule(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let env = env(pool);
    let data = env.audio(FLAC_REL);
    let t1 = id(1);
    // A refused format is not an error for `stream`: the file as it is.
    let res = env.get("stream", &[("id", &t1), ("format", "opus")]).await;
    assert_eq!(res.header(CONTENT_TYPE), "audio/flac");
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    // `download` is always the file, whatever the rate.
    let res = env
        .get("download", &[("id", &t1), ("maxBitRate", "64")])
        .await;
    assert_eq!(res.as_bytes().as_ref(), &data[..]);
    // `hls` links `stream.view` with the bitrate as `maxBitRate`.
    let res = env.get("hls", &[("id", &t1), ("bitRate", "192")]).await;
    assert!(res.text().contains("maxBitRate=192"), "{}", res.text());
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn transcode_params_kima_would_not_hand_out_are_an_error_not_a_stream(pool: PgPool) {
    let env = env(pool);
    let t1 = id(1);
    for bad in [
        "format=mp3",
        "q-bogus",
        "q-",
        "mp3-",
        "x-high",
        "q-high-0",
        "q-high-x",
        "q-high-123456",
        "q-original-128",
        "mp3-original",
        "q-high-100",
        "q-high",
        "mp3-low-100",
        "q-low-64",
        "mp3-medium-320",
    ] {
        env.assert_error(
            "getTranscodeStream",
            &[
                ("mediaId", &t1),
                ("mediaType", "song"),
                ("transcodeParams", bad),
            ],
            0,
            "Invalid transcodeParams: ask getTranscodeDecision again",
        )
        .await;
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn a_decision_for_anything_but_a_song_is_an_error(pool: PgPool) {
    let env = env(pool);
    // `id(1)` is a song id, not a real podcast episode id, so the podcast branch (M6 Task 18)
    // also answers "Media not found"; `decision_podcast_direct_only` (`tests/podcasts.rs`)
    // covers a real one.
    for kind in ["podcast", "album"] {
        env.assert_error(
            "getTranscodeDecision",
            &[("mediaId", &id(1)), ("mediaType", kind)],
            70,
            "Media not found",
        )
        .await;
    }
}
