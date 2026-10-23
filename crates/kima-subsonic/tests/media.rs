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
                "transcodeParams": "",
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
            "transcodeParams": "format=mp3",
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
    let env = env(pool);
    let data = env.audio(FLAC_REL);
    let t1 = id(1);
    let song = env
        .get(
            "getTranscodeStream",
            &[
                ("mediaId", &t1),
                ("mediaType", "song"),
                ("transcodeParams", "x"),
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
                ("transcodeParams", "x"),
                ("maxBitRate", "128"),
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
            ("transcodeParams", "x"),
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
