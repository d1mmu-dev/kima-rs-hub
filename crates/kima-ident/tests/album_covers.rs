//! Album cover fallback (spec §12.7; upstream `enrichAlbumCovers`, `artistEnrichment.ts:393-479`):
//! Deezer, then the Cover Art Archive, then fanart.tv, for the artist's albums that have no
//! embedded or folder cover. Replay only: every answer is a hand-written entry.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use kima_db::identity;
use kima_ident::IdentCtx;
use kima_ident::album_covers;
use kima_ident::artist::{self, ArtistResult};
use kima_library::artwork;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const FANART_KEY: &str = "fanart-test-key-1234";
const RG_A: &str = "22222222-0000-4000-8000-00000000000a";
const RG_B: &str = "22222222-0000-4000-8000-00000000000b";
const RG_C: &str = "22222222-0000-4000-8000-00000000000c";
const M: &str = "11111111-2222-3333-4444-555555555555";

fn entry(url: &str, status: u16, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: url.into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

/// Answers keyed by URL; a later one for the same URL replaces the earlier.
#[derive(Default, Clone)]
struct World(BTreeMap<String, Entry>);

impl World {
    fn get(mut self, url: &str, body: Value) -> Self {
        self.0.insert(url.into(), entry(url, 200, body));
        self
    }
    fn status(mut self, url: &str, code: u16) -> Self {
        self.0
            .insert(url.into(), entry(url, code, json!({"error": "boom"})));
        self
    }
    fn image(mut self, path: &str, bytes: &[u8]) -> Self {
        let mut e = entry(path, 200, Value::Null);
        e.headers.insert("content-type".into(), "image/jpeg".into());
        e.body_base64 = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        self.0.insert(path.into(), e);
        self
    }
    fn entries(self) -> Vec<Entry> {
        self.0.into_values().collect()
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Deezer's structured album search and its plain fallback.
fn deezer_urls(artist: &str, title: &str) -> (String, String) {
    (
        format!(
            "/search/album?q={}&limit=5",
            enc(&format!("artist:\"{artist}\" album:\"{title}\""))
        ),
        format!(
            "/search/album?q={}&limit=5",
            enc(&format!("{artist} {title}"))
        ),
    )
}

fn caa_url(rg: &str) -> String {
    format!("/release-group/{rg}")
}

fn fanart_album_url(rg: &str) -> String {
    format!("/music/albums/{rg}?api_key={FANART_KEY}")
}

impl World {
    /// Deezer finds nothing for the album, either way it is asked.
    fn deezer_none(self, artist: &str, title: &str) -> Self {
        let (s, p) = deezer_urls(artist, title);
        self.get(&s, json!({"data": []}))
            .get(&p, json!({"data": []}))
    }
    fn deezer_hit(self, artist: &str, title: &str, cover: &str) -> Self {
        let (s, _) = deezer_urls(artist, title);
        self.get(
            &s,
            json!({"data": [{"title": title, "artist": {"name": artist}, "cover_xl": cover}]}),
        )
    }
    fn caa_front(self, rg: &str, image: &str) -> Self {
        self.get(
            &caa_url(rg),
            json!({"images": [{"front": true, "image": image, "thumbnails": {}}]}),
        )
    }
    fn fanart_cover(self, rg: &str, url: &str) -> Self {
        self.get(
            &fanart_album_url(rg),
            json!({"albums": {rg: {"albumcover": [{"url": url}]}}}),
        )
    }
}

/// A noisy JPEG: well over the 1000 bytes an image must have.
fn jpeg(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let v = (x ^ seed)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503));
        image::Rgb([(v >> 3) as u8, (v >> 11) as u8, (v >> 19) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

struct Fx {
    api: Replay,
    images: Replay,
    ctx: IdentCtx,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn asked(&self) -> Vec<String> {
        self.api.asked()
    }
    fn data_dir(&self) -> &Path {
        &self.ctx.library.data_dir
    }
}

async fn fx(pool: &PgPool, api: World, images: World) -> Fx {
    let images = Replay::start(Recording(images.entries())).await;
    let base = images.url();
    let entries = api
        .entries()
        .into_iter()
        .map(|mut e| {
            let text = serde_json::to_string(&e.body).unwrap();
            e.body = serde_json::from_str(&text.replace("{images}", &base)).unwrap();
            e
        })
        .collect();
    let api = Replay::start(Recording(entries)).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut ctx = common::ident_ctx(pool, &root, Bases::all(&api.url()));
    ctx.config.fanart_env_key = Some(FANART_KEY.into());
    Fx {
        api,
        images,
        ctx,
        _dir: dir,
        root,
    }
}

async fn artist_row(pool: &PgPool, name: &str) -> Uuid {
    let artist = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, lower($2))")
        .bind(artist)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    artist
}

/// An album of `artist` holding one present track at `<name>/<title>/01.flac`.
async fn album(pool: &PgPool, artist: Uuid, name: &str, title: &str, rg: Option<&str>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, release_group_mbid) \
         VALUES ($1, $2, $3, lower($3), $4)",
    )
    .bind(id)
    .bind(artist)
    .bind(title)
    .bind(rg.map(|r| Uuid::parse_str(r).unwrap()))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, duration_secs) \
         VALUES ($1, $2, $3, 't', 'flac', $4, 1, now(), '\\x00', 30)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(name)
    .bind(format!("{name}/{title}/01.flac"))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn image_of(pool: &PgPool, album: Uuid) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT image_path, image_source FROM albums WHERE id = $1")
        .bind(album)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn stored(f: &Fx, album: Uuid) -> Option<Vec<u8>> {
    std::fs::read(artwork::album_image_path(f.data_dir(), album)).ok()
}

/// The files in `<data_dir>/album-images`, temp files included.
fn album_image_files(f: &Fx) -> Vec<String> {
    std::fs::read_dir(f.data_dir().join("album-images"))
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn position(asked: &[String], needle: &str) -> usize {
    asked
        .iter()
        .position(|a| a.contains(needle))
        .unwrap_or_else(|| panic!("{needle} was not asked: {asked:?}"))
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_then_caa_then_fanart_order(pool: PgPool) {
    let a = artist_row(&pool, "Low").await;
    let alpha = album(&pool, a, "Low", "Alpha", Some(RG_A)).await;
    let beta = album(&pool, a, "Low", "Beta", Some(RG_B)).await;
    let gamma = album(&pool, a, "Low", "Gamma", Some(RG_C)).await;
    let delta = album(&pool, a, "Low", "Delta", None).await;
    let w = World::default()
        .deezer_hit("Low", "Alpha", "{images}/alpha.jpg")
        .caa_front(RG_A, "{images}/never.jpg")
        .deezer_none("Low", "Beta")
        .caa_front(RG_B, "{images}/beta.jpg")
        .deezer_none("Low", "Gamma")
        .status(&caa_url(RG_C), 404)
        .fanart_cover(RG_C, "{images}/gamma.jpg")
        .deezer_none("Low", "Delta");
    let images = World::default()
        .image("/alpha.jpg", &jpeg(600, 600, 1))
        .image("/beta.jpg", &jpeg(2000, 1000, 2))
        .image("/gamma.jpg", &jpeg(300, 300, 3));
    let f = fx(&pool, w, images).await;

    let out = album_covers::fill_for_artist(&f.ctx, a).await;
    assert!(matches!(out, Outcome::Answered(3)), "{out:?}");

    let name = |id: Uuid| Some(format!("{id}.jpg"));
    assert_eq!(
        image_of(&pool, alpha).await,
        (name(alpha), Some("deezer".into()))
    );
    assert_eq!(
        image_of(&pool, beta).await,
        (name(beta), Some("coverartarchive".into()))
    );
    assert_eq!(
        image_of(&pool, gamma).await,
        (name(gamma), Some("fanart".into()))
    );
    assert_eq!(image_of(&pool, delta).await, (None, None));

    // Stored re-encoded, at most 1500 px on the long side.
    let img = image::load_from_memory(&stored(&f, beta).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (1500, 750));
    assert!(stored(&f, delta).is_none());

    let asked = f.asked();
    // Deezer answered for Alpha: the Cover Art Archive was never asked about it.
    assert!(asked.iter().all(|q| !q.contains(RG_A)), "{asked:?}");
    // Gamma: Deezer, then the Cover Art Archive, then fanart.tv.
    let dz = position(&asked, "Gamma");
    let caa = position(&asked, &caa_url(RG_C));
    let fa = position(&asked, &format!("/music/albums/{RG_C}"));
    assert!(dz < caa && caa < fa, "{asked:?}");
    // Without a release group only Deezer can be asked.
    assert_eq!(
        asked.iter().filter(|q| q.contains("Delta")).count(),
        2,
        "{asked:?}"
    );
    assert!(f.images.asked().iter().all(|q| !q.contains("never")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_with_embedded_cover_untouched(pool: PgPool) {
    let a = artist_row(&pool, "Low").await;
    let covered = album(&pool, a, "Low", "Covered", Some(RG_A)).await;
    let w = World::default()
        .deezer_hit("Low", "Covered", "{images}/c.jpg")
        .caa_front(RG_A, "{images}/c.jpg");
    let images = World::default().image("/c.jpg", &jpeg(300, 300, 1));
    let f = fx(&pool, w, images).await;
    // Art the M1 lookup finds (a folder image next to the track; embedded art is the same
    // `find_cover` call) keeps the album out of the fallback.
    let dir = f.root.join("Low/Covered");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cover.jpg"), jpeg(200, 200, 9)).unwrap();

    let out = album_covers::fill_for_artist(&f.ctx, a).await;
    assert!(matches!(out, Outcome::Answered(0)), "{out:?}");
    assert_eq!(image_of(&pool, covered).await, (None, None));
    assert!(f.asked().is_empty(), "{:?}", f.asked());
    assert!(stored(&f, covered).is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_source_records_nothing(pool: PgPool) {
    let a = artist_row(&pool, "Low").await;
    // Deezer fails outright; no release group, so nothing else can be asked.
    let alpha = album(&pool, a, "Low", "Alpha", None).await;
    // Deezer names an image that does not decode; the archive fails; fanart.tv has none.
    let beta = album(&pool, a, "Low", "Beta", Some(RG_B)).await;
    // Deezer names an image that is not there; the archive's is used instead.
    let gamma = album(&pool, a, "Low", "Gamma", Some(RG_C)).await;
    let (alpha_q, _) = deezer_urls("Low", "Alpha");
    let w = World::default()
        .status(&alpha_q, 500)
        .deezer_hit("Low", "Beta", "{images}/garbage.jpg")
        .status(&caa_url(RG_B), 500)
        .status(&fanart_album_url(RG_B), 404)
        .deezer_hit("Low", "Gamma", "{images}/missing.jpg")
        .caa_front(RG_C, "{images}/gamma.jpg");
    let images = World::default()
        .image("/garbage.jpg", &[0x5a; 4000])
        .image("/gamma.jpg", &jpeg(300, 300, 3));
    let f = fx(&pool, w, images).await;

    let out = album_covers::fill_for_artist(&f.ctx, a).await;
    assert!(matches!(out, Outcome::Answered(1)), "{out:?}");

    assert_eq!(image_of(&pool, alpha).await, (None, None));
    assert_eq!(image_of(&pool, beta).await, (None, None));
    assert_eq!(
        image_of(&pool, gamma).await,
        (Some(format!("{gamma}.jpg")), Some("coverartarchive".into()))
    );
    // Only Gamma's file; no temp file is left behind.
    assert_eq!(album_image_files(&f), [format!("{gamma}.jpg")]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rate_limited_source_leaves_the_album_for_later(pool: PgPool) {
    let a = artist_row(&pool, "Low").await;
    let alpha = album(&pool, a, "Low", "Alpha", None).await;
    let f = fx(&pool, World::default(), World::default()).await;
    sqlx::query(
        "INSERT INTO source_pauses (source, paused_until, reason)
         VALUES ('deezer', now() + interval '10 minutes', 'HTTP 429')",
    )
    .execute(&pool)
    .await
    .unwrap();
    f.ctx.net.load_pauses().await.unwrap();

    let out = album_covers::fill_for_artist(&f.ctx, a).await;
    assert!(matches!(out, Outcome::RateLimited { .. }), "{out:?}");
    assert_eq!(image_of(&pool, alpha).await, (None, None));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_user_cover_is_never_replaced(pool: PgPool) {
    let a = artist_row(&pool, "Low").await;
    let alpha = album(&pool, a, "Low", "Alpha", None).await;
    let f = fx(&pool, World::default(), World::default()).await;
    sqlx::query(
        "UPDATE albums SET image_path = id::text || '.jpg', image_source = 'user' WHERE id = $1",
    )
    .bind(alpha)
    .execute(&pool)
    .await
    .unwrap();

    // Not offered to the fallback at all...
    assert!(
        identity::albums_without_cover(&pool, a)
            .await
            .unwrap()
            .is_empty()
    );
    // ...and the write itself refuses to replace it.
    let mut conn = pool.acquire().await.unwrap();
    let applied = identity::set_album_image(&mut conn, alpha, "x.jpg", "deezer")
        .await
        .unwrap();
    assert!(!applied);
    assert_eq!(
        image_of(&pool, alpha).await,
        (Some(format!("{alpha}.jpg")), Some("user".into()))
    );
    let out = album_covers::fill_for_artist(&f.ctx, a).await;
    assert!(matches!(out, Outcome::Answered(0)), "{out:?}");
    assert!(f.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn albums_without_cover_lists_present_albums_without_an_image(pool: PgPool) {
    let a = artist_row(&pool, "Low").await;
    let alpha = album(&pool, a, "Low", "Alpha", Some(RG_A)).await;
    let beta = album(&pool, a, "Low", "Beta", None).await;
    let gone = album(&pool, a, "Low", "Gone", None).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE album_id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE albums SET image_path = 'b.jpg', image_source = 'deezer' WHERE id = $1")
        .bind(beta)
        .execute(&pool)
        .await
        .unwrap();
    let found = identity::albums_without_cover(&pool, a).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let first = &found[0];
    assert_eq!(first.id, alpha);
    assert_eq!(first.title, "Alpha");
    assert_eq!(first.artist_name, "Low");
    assert_eq!(
        first.release_group_mbid,
        Some(Uuid::parse_str(RG_A).unwrap())
    );
    assert_eq!(first.track_path.as_deref(), Some("Low/Alpha/01.flac"));
}

/// `enrich_one` fills the artist's album covers after its own image.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enrichment_fills_album_covers(pool: PgPool) {
    let a = artist_row(&pool, "Perturbator").await;
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = 'user' WHERE id = $1")
        .bind(a)
        .bind(Uuid::parse_str(M).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let days = album(&pool, a, "Perturbator", "Dangerous Days", None).await;
    let algorithm = "session_based_days_7500_session_300_contribution_3_threshold_10_limit_100_filter_True_skip_30";
    let sparql = enc(&format!(
        "SELECT ?item WHERE {{ ?item wdt:P434 \"{M}\" }} LIMIT 1"
    ));
    let w = World::default()
        .get(
            &format!("/artist/{M}?inc=genres&fmt=json"),
            json!({"id": M, "name": "Perturbator", "genres": []}),
        )
        .get(
            &format!("/sparql?query={sparql}&format=json"),
            json!({"head": {"vars": ["item"]}, "results": {"bindings": []}}),
        )
        .get(
            &format!("/similar-artists/json?artist_mbids={M}&algorithm={algorithm}"),
            json!([]),
        )
        .get("/search/artist?q=Perturbator&limit=1", json!({"data": []}))
        .get(&format!("/music/{M}?api_key={FANART_KEY}"), json!({}))
        .deezer_hit("Perturbator", "Dangerous Days", "{images}/days.jpg");
    let images = World::default().image("/days.jpg", &jpeg(500, 500, 4));
    let f = fx(&pool, w, images).await;

    let claimed = identity::claim_artists(&pool, 10).await.unwrap();
    let mine = claimed.into_iter().find(|c| c.id == a).unwrap();
    assert_eq!(
        artist::enrich_one(&f.ctx, &mine).await.unwrap(),
        ArtistResult::Completed
    );
    assert_eq!(
        image_of(&pool, days).await,
        (Some(format!("{days}.jpg")), Some("deezer".into()))
    );
    assert!(stored(&f, days).is_some());
}
