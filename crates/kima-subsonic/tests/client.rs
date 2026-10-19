//! A real token+salt Subsonic client against a real socket (spec §10.3): the `kima` router is
//! served by `axum::serve` on an ephemeral port, Postgres is real, and the library is three
//! ffmpeg tones that the real scanner has indexed.

mod common;

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;

use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH, RANGE};
use common::audio::require_ffmpeg;
use kima_core::events::EventBus;
use kima_db::PgPool;
use kima_db::users;
use kima_library::scan::{LibraryCtx, reconcile};
use kima_library::transcode::Transcoder;
use md5::{Digest, Md5};
use rand::Rng;
use serde_json::{Value, json};
use uuid::Uuid;

const PASSWORD: &str = "clientpass1";
const NEW_PASSWORD: &str = "clientpass2";

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Json,
    Xml,
}

/// A token+salt Subsonic client: `u`, `t = md5(token + salt)` and a random 12-character `s` on
/// every call.
struct Client {
    http: reqwest::Client,
    base: String,
    user: String,
    token: String,
}

impl Client {
    fn new(http: &reqwest::Client, base: &str, user: &str, token: &str) -> Self {
        Self {
            http: http.clone(),
            base: base.to_owned(),
            user: user.to_owned(),
            token: token.to_owned(),
        }
    }

    /// The `u`, `t` and `s` parameters for one request, with a fresh salt.
    fn ping_ts(&self) -> Vec<(String, String)> {
        let salt: String = rand::thread_rng()
            .sample_iter(rand::distributions::Alphanumeric)
            .take(12)
            .map(char::from)
            .collect();
        let t = hex::encode(Md5::digest(format!("{}{salt}", self.token)));
        vec![
            ("u".into(), self.user.clone()),
            ("t".into(), t),
            ("s".into(), salt),
        ]
    }

    fn request(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        format: Format,
    ) -> reqwest::RequestBuilder {
        let mut query = self.ping_ts();
        query.push(("v".into(), "1.16.1".into()));
        query.push(("c".into(), "kima-test".into()));
        if format == Format::Json {
            query.push(("f".into(), "json".into()));
        }
        query.extend(params.iter().map(|(k, v)| ((*k).into(), (*v).into())));
        self.http
            .get(format!("{}/rest/{endpoint}.view", self.base))
            .query(&query)
    }

    async fn call(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        format: Format,
    ) -> reqwest::Response {
        let res = self.request(endpoint, params, format).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{endpoint}");
        res
    }

    /// The `subsonic-response` object of a JSON answer.
    async fn json(&self, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let body: Value = serde_json::from_slice(
            &self
                .call(endpoint, params, Format::Json)
                .await
                .bytes()
                .await
                .unwrap(),
        )
        .unwrap();
        body["subsonic-response"].clone()
    }

    async fn xml(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        self.call(endpoint, params, Format::Xml)
            .await
            .text()
            .await
            .unwrap()
    }
}

fn ok(res: &Value) {
    assert_eq!(res["status"], "ok", "{res}");
}

fn failed(res: &Value, code: u64) {
    assert_eq!(res["status"], "failed", "{res}");
    assert_eq!(res["error"]["code"], code, "{res}");
}

/// A `/api` call: JSON in, JSON out, with an optional bearer token.
async fn api(
    http: &reqwest::Client,
    base: &str,
    method: reqwest::Method,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = http.request(method, format!("{base}/api{path}"));
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string());
    }
    let res = req.send().await.unwrap();
    let status = res.status();
    let bytes = res.bytes().await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// A 35 second FLAC clip with tags: a play needs a track longer than 30 seconds. White noise keeps
/// the bitrate far above the 128 kbps cap, which a pure sine would not, at `root/rel`.
fn tone(root: &Path, rel: &str, title: &str, track: u32) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("anoisesrc=duration=35:amplitude=0.5:seed={track}"))
        .args(["-c:a", "flac"])
        .args(["-metadata", "artist=Tone Smith"])
        .args(["-metadata", "album_artist=Tone Smith"])
        .args(["-metadata", "album=Pure Tones"])
        .args(["-metadata", &format!("title={title}")])
        .args(["-metadata", &format!("track={track}")])
        .args(["-metadata", "date=2020"])
        .arg(&path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success());
}

/// The ids of a Subsonic list (`song`, `album` ...) in order.
fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap_or_else(|| panic!("not a list: {list}"))
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn real_handshake_end_to_end(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    // The library: three tones and a folder cover, indexed by the real scanner.
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let album_dir = root.path().join("Tone Smith/Pure Tones");
    let titles = ["Alpha", "Bravo", "Charlie"];
    for (i, title) in titles.iter().enumerate() {
        tone(
            root.path(),
            &format!("Tone Smith/Pure Tones/0{} {title}.flac", i + 1),
            title,
            i as u32 + 1,
        );
    }
    image::RgbImage::from_pixel(400, 200, image::Rgb([200, 30, 30]))
        .save(album_dir.join("cover.jpg"))
        .unwrap();
    let ctx = LibraryCtx {
        pool: pool.clone(),
        root: root.path().to_path_buf().into(),
        events: EventBus::new(64),
        data_dir: data.path().to_path_buf(),
        ignore: Default::default(),
    };
    let report = reconcile(&ctx).await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    users::create_by_admin(
        &pool,
        Uuid::now_v7(),
        "alice",
        &kima_core::auth::hash_password(PASSWORD),
        "user",
    )
    .await
    .unwrap()
    .unwrap();

    // The real router on a real socket.
    let state = kima_api::AppState::new(pool.clone(), "client-secret")
        .with_library_root(root.path().to_path_buf())
        .with_data_dir(data.path().to_path_buf())
        .with_transcoder(Transcoder::new(cache.path(), 100 * 1024 * 1024));
    let app = kima_api::router(state.clone()).nest("/rest", kima_subsonic::router(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let http = reqwest::Client::new();

    // 1. Sign in on the web side and mint an app token.
    let (status, login) = api(
        &http,
        &base,
        reqwest::Method::POST,
        "/auth/login",
        None,
        Some(json!({"username": "alice", "password": PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let bearer = login["token"].as_str().unwrap().to_owned();
    let (status, created) = api(
        &http,
        &base,
        reqwest::Method::POST,
        "/api-keys",
        Some(&bearer),
        Some(json!({"deviceName": "Phone"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let token = created["apiKey"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    let client = Client::new(&http, &base, "alice", &token);

    // 2. Every login path.
    ok(&client.json("ping", &[]).await);
    let xml = client.xml("ping", &[]).await;
    assert!(xml.contains("status=\"ok\""), "{xml}");

    let wrong = Client::new(&http, &base, "alice", "not-the-token");
    failed(&wrong.json("ping", &[]).await, 40);
    let by_password = Client::new(&http, &base, "alice", PASSWORD);
    let res = by_password.json("ping", &[]).await;
    failed(&res, 40);
    assert_eq!(
        res["error"]["message"],
        "Wrong username or password. Token authentication needs an app token from Settings > Native Apps; your account password cannot be used with it."
    );
    let key_only = http
        .get(format!("{base}/rest/ping.view"))
        .query(&[("apiKey", token.as_str()), ("f", "json")])
        .send()
        .await
        .unwrap();
    let key_only: Value = key_only.json_value().await;
    ok(&key_only["subsonic-response"]);
    let both = http
        .get(format!("{base}/rest/ping.view"))
        .query(&[("apiKey", token.as_str()), ("p", "x"), ("f", "json")])
        .send()
        .await
        .unwrap();
    failed(&both.json_value().await["subsonic-response"], 43);
    let enc = format!("enc:{}", hex::encode(&token));
    let by_enc = http
        .get(format!("{base}/rest/ping.view"))
        .query(&[("u", "alice"), ("p", enc.as_str()), ("f", "json")])
        .send()
        .await
        .unwrap();
    ok(&by_enc.json_value().await["subsonic-response"]);

    // 3. Browse by following ids, in both formats.
    let artists = client.json("getArtists", &[]).await;
    ok(&artists);
    let artist = &artists["artists"]["index"][0]["artist"][0];
    assert_eq!(artist["name"], "Tone Smith");
    let artist_id = artist["id"].as_str().unwrap().to_owned();
    let xml = client.xml("getArtists", &[]).await;
    assert!(xml.contains(&format!("id=\"{artist_id}\"")), "{xml}");

    let artist = client.json("getArtist", &[("id", &artist_id)]).await;
    ok(&artist);
    let album_id = ids(&artist["artist"]["album"])[0].clone();
    let xml = client.xml("getArtist", &[("id", &artist_id)]).await;
    assert!(xml.contains(&format!("id=\"{album_id}\"")), "{xml}");

    let album = client.json("getAlbum", &[("id", &album_id)]).await;
    ok(&album);
    assert_eq!(album["album"]["name"], "Pure Tones");
    let songs = ids(&album["album"]["song"]);
    assert_eq!(songs.len(), 3);
    let titles_got: Vec<&str> = album["album"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles_got, titles);
    let xml = client.xml("getAlbum", &[("id", &album_id)]).await;
    for song in &songs {
        assert!(xml.contains(&format!("id=\"{song}\"")), "{xml}");
    }

    let song = client.json("getSong", &[("id", &songs[0])]).await;
    ok(&song);
    assert_eq!(song["song"]["id"], songs[0]);
    assert_eq!(song["song"]["title"], "Alpha");
    let xml = client.xml("getSong", &[("id", &songs[0])]).await;
    assert!(xml.contains("title=\"Alpha\""), "{xml}");

    // 4. Stream: a range, the ETag and a transcode.
    let original = std::fs::read(album_dir.join("01 Alpha.flac")).unwrap();
    let res = client
        .request("stream", &[("id", &songs[0])], Format::Json)
        .header(RANGE, "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let body = res.bytes().await.unwrap();
    assert_eq!(body.len(), 100);
    assert_eq!(body.as_ref(), &original[100..=199]);

    let res = client
        .call("stream", &[("id", &songs[0])], Format::Json)
        .await;
    let etag = res.headers()[ETAG].to_str().unwrap().to_owned();
    let res = client
        .request("stream", &[("id", &songs[0])], Format::Json)
        .header(IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);

    let res = client
        .call(
            "stream",
            &[("id", &songs[0]), ("maxBitRate", "128")],
            Format::Json,
        )
        .await;
    assert_eq!(res.headers()[CONTENT_TYPE], "audio/mpeg");

    // 5. Cover art.
    let res = client
        .call(
            "getCoverArt",
            &[("id", &album_id), ("size", "128")],
            Format::Json,
        )
        .await;
    assert_eq!(res.headers()[CONTENT_TYPE], "image/jpeg");
    let img = res.bytes().await.unwrap();
    assert!(img.starts_with(&[0xff, 0xd8, 0xff]), "not a JPEG");

    // 6. A scrobble becomes a SUBSONIC play on the web side.
    ok(&client
        .json("scrobble", &[("id", &songs[0]), ("submission", "true")])
        .await);
    let (status, plays) = api(
        &http,
        &base,
        reqwest::Method::GET,
        "/plays",
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let play = plays
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["trackId"] == songs[0])
        .unwrap_or_else(|| panic!("no play in {plays}"));
    assert_eq!(play["source"], "SUBSONIC");

    // 7. A saved queue is the web player's queue.
    let queue: Vec<(&str, &str)> = songs.iter().map(|s| ("id", s.as_str())).collect();
    let mut params = queue.clone();
    params.extend([("current", songs[1].as_str()), ("position", "1500")]);
    ok(&client.json("savePlayQueue", &params).await);
    let got = client.json("getPlayQueue", &[]).await;
    ok(&got);
    assert_eq!(ids(&got["playQueue"]["entry"]), songs);
    assert_eq!(got["playQueue"]["current"], songs[1]);
    let (status, web) = api(
        &http,
        &base,
        reqwest::Method::GET,
        "/playback-state",
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&web["queue"]), songs, "{web}");
    assert_eq!(web["trackId"], songs[1]);

    // 8. A star and a rating.
    ok(&client.json("star", &[("albumId", &album_id)]).await);
    let starred = client.json("getStarred2", &[]).await;
    assert_eq!(
        ids(&starred["starred2"]["album"]),
        std::slice::from_ref(&album_id)
    );
    ok(&client
        .json("setRating", &[("id", &album_id), ("rating", "4")])
        .await);
    let album = client.json("getAlbum", &[("id", &album_id)]).await;
    assert_eq!(album["album"]["userRating"], 4, "{album}");

    // 9. Revoking the token ends its sessions.
    let (_, keys) = api(
        &http,
        &base,
        reqwest::Method::GET,
        "/api-keys",
        Some(&bearer),
        None,
    )
    .await;
    let key_id = keys["apiKeys"][0]["id"].as_str().unwrap().to_owned();
    let (status, _) = api(
        &http,
        &base,
        reqwest::Method::DELETE,
        &format!("/api-keys/{key_id}"),
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    failed(&client.json("ping", &[]).await, 40);

    // 10. A password change revokes every token.
    let (_, second) = api(
        &http,
        &base,
        reqwest::Method::POST,
        "/api-keys",
        Some(&bearer),
        Some(json!({"deviceName": "Tablet"})),
    )
    .await;
    let second = Client::new(&http, &base, "alice", second["apiKey"].as_str().unwrap());
    ok(&second.json("ping", &[]).await);
    let (status, changed) = api(
        &http,
        &base,
        reqwest::Method::POST,
        "/auth/change-password",
        Some(&bearer),
        Some(json!({"currentPassword": PASSWORD, "newPassword": NEW_PASSWORD})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let bearer = changed["token"].as_str().unwrap().to_owned();
    failed(&second.json("ping", &[]).await, 40);
    let (status, keys) = api(
        &http,
        &base,
        reqwest::Method::GET,
        "/api-keys",
        Some(&bearer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys["apiKeys"], json!([]));

    // 11. Password guessing is limited; a token is not.
    let guess = |n: u32| {
        let http = http.clone();
        let url = format!("{base}/rest/ping.view");
        async move {
            let wrong = format!("wrong-{n}");
            let res = http
                .get(url)
                .query(&[("u", "alice"), ("p", wrong.as_str()), ("f", "json")])
                .send()
                .await
                .unwrap();
            res.json_value().await["subsonic-response"].clone()
        }
    };
    for n in 1..=5 {
        failed(&guess(n).await, 40);
    }
    let blocked = guess(6).await;
    failed(&blocked, 41);
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Too many failed sign-in attempts. Try again in "),
        "{blocked}"
    );
    let (status, fresh) = api(
        &http,
        &base,
        reqwest::Method::POST,
        "/api-keys",
        Some(&bearer),
        Some(json!({"deviceName": "Fresh"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let fresh = Client::new(&http, &base, "alice", fresh["apiKey"].as_str().unwrap());
    ok(&fresh.json("ping", &[]).await);
}

/// `Response::json()` needs reqwest's `json` feature, which this crate leaves off.
trait JsonValue {
    async fn json_value(self) -> Value;
}

impl JsonValue for reqwest::Response {
    async fn json_value(self) -> Value {
        serde_json::from_slice(&self.bytes().await.unwrap()).unwrap()
    }
}
