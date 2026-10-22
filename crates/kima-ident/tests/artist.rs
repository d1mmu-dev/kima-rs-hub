//! Artist enrichment (spec §10.5, §12.1-§12.5): MBID resolution and the guard, bio, image,
//! genres and similar artists, the failure cap and the claim. Ports `resolveArtistMbid.test.ts`
//! (the `getArtistCorrection` cases dropped), `artistFailureCap.test.ts`,
//! `artistEnrichmentArtwork.test.ts` (the album-cover sweep is Task 22's), selected
//! `artwork.test.ts` cases and `artistSimilarity.tiers.test.ts` (the library re-rank is M5's).
//! Replay only: every answer is a hand-written entry.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use kima_db::identity::{self, ArtistForEnrichment};
use kima_ident::artist::{self, ArtistResult, MAX_CATALOGUE_CHECKS};
use kima_ident::listenbrainz::Similar;
use kima_ident::similar::{self, Entry as SimilarEntry};
use kima_ident::{IdentCtx, Mbid};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Outcome};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_3_threshold_10_limit_100_filter_True_skip_30";

const M: &str = "11111111-2222-3333-4444-555555555555";
const LOW: &str = "11111111-0000-4000-8000-000000000001";
const LOW_UK: &str = "11111111-0000-4000-8000-000000000002";
const LOW_ROAR: &str = "11111111-0000-4000-8000-000000000003";
const YE: &str = "11111111-0000-4000-8000-000000000004";
const FANART_KEY: &str = "fanart-test-key-1234";

fn mbid(s: &str) -> Mbid {
    Mbid::parse(s).unwrap()
}

fn answer<T>(o: Outcome<T>) -> Option<T> {
    match o {
        Outcome::Answered(t) => Some(t),
        _ => None,
    }
}

// ---- fixtures ----

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

fn mb_artist_url(id: &str, inc: &str) -> String {
    format!("/artist/{id}?inc={inc}&fmt=json")
}

fn mb_search_url(q: &str) -> String {
    format!("/artist?query={q}&limit=10&fmt=json")
}

fn rg_url(id: &str, offset: u32) -> String {
    format!("/release-group?artist={id}&type=album|ep|single&limit=100&offset={offset}&fmt=json")
}

fn sparql_url(id: &str) -> String {
    let q = format!("SELECT ?item WHERE {{ ?item wdt:P434 \"{id}\" }} LIMIT 1");
    let encoded: String = q
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("/sparql?query={encoded}&format=json")
}

fn sparql(q: Option<&str>) -> Value {
    let bindings = match q {
        Some(q) => {
            json!([{ "item": { "type": "uri", "value": format!("http://www.wikidata.org/entity/{q}") } }])
        }
        None => json!([]),
    };
    json!({ "head": { "vars": ["item"] }, "results": { "bindings": bindings } })
}

fn lb_url(id: &str) -> String {
    format!("/similar-artists/json?artist_mbids={id}&algorithm={ALGORITHM}")
}

fn deezer_search_url(name: &str) -> String {
    format!("/search/artist?q={name}&limit=1")
}

fn fanart_url(id: &str) -> String {
    format!("/music/{id}?api_key={FANART_KEY}")
}

/// What MusicBrainz says of `id`: `name`, its sort name and aliases.
fn held(id: &str, name: &str, sort: &str, aliases: &[&str]) -> Value {
    json!({
        "id": id, "name": name, "sort-name": sort,
        "aliases": aliases.iter().map(|a| json!({"name": a, "sort-name": a})).collect::<Vec<_>>(),
    })
}

/// An artist MusicBrainz knows as `name` under `id`, with nothing anywhere else: no genres, no
/// Wikidata item, no similar artists, no Deezer hit.
fn known(name: &str, id: &str) -> World {
    World::default()
        .get(&mb_artist_url(id, "aliases"), held(id, name, name, &[]))
        .get(
            &mb_artist_url(id, "genres"),
            json!({"id": id, "name": name, "genres": []}),
        )
        .get(&sparql_url(id), sparql(None))
        .get(&lb_url(id), json!([]))
        .get(&deezer_search_url(name), json!({"data": []}))
        .get(&fanart_url(id), json!({}))
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

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let v = x
            .wrapping_mul(2_654_435_761)
            .wrapping_add(y.wrapping_mul(40_503));
        image::Rgb([(v >> 3) as u8, (v >> 11) as u8, (v >> 19) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// The replay servers and an [`IdentCtx`] over them. Images are served by their own server,
/// whose URL the API answers name (`{images}` in a body is replaced by it).
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
    fn images_asked(&self) -> Vec<String> {
        self.images.asked()
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

// ---- the database side ----

/// An artist with one album holding one present track at `<name>/<album>/01.flac`.
async fn seed(pool: &PgPool, name: &str, album: &str) -> Uuid {
    let artist = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, lower($2))")
        .bind(artist)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    add_album(pool, artist, name, album).await;
    artist
}

async fn add_album(pool: &PgPool, artist: Uuid, name: &str, album: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, $3, lower($3))",
    )
    .bind(id)
    .bind(artist)
    .bind(album)
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
    .bind(format!("{name}/{album}/01.flac"))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn set(pool: &PgPool, artist: Uuid, sql: &str) {
    sqlx::query(&format!("UPDATE artists SET {sql} WHERE id = $1"))
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
}

async fn set_mbid(pool: &PgPool, artist: Uuid, id: &str, source: &str) {
    sqlx::query("UPDATE artists SET mbid = $2, mbid_source = $3 WHERE id = $1")
        .bind(artist)
        .bind(Uuid::parse_str(id).unwrap())
        .bind(source)
        .execute(pool)
        .await
        .unwrap();
}

/// Claims `artist` (and nothing else) and enriches it.
async fn enrich(f: &Fx, artist: Uuid) -> ArtistResult {
    let claimed = identity::claim_artists(&f.ctx.pool, 50).await.unwrap();
    let a: ArtistForEnrichment = claimed
        .into_iter()
        .find(|a| a.id == artist)
        .expect("the artist is claimable");
    artist::enrich_one(&f.ctx, &a).await.unwrap()
}

struct Row_ {
    status: String,
    failures: i16,
    mbid: Option<Uuid>,
    mbid_source: Option<String>,
    bio: Option<String>,
    image_path: Option<String>,
    image_source: Option<String>,
    similar: Option<Value>,
    last_error: Option<String>,
    last_error_code: Option<String>,
    enriched: bool,
}

async fn row(pool: &PgPool, artist: Uuid) -> Row_ {
    let r = sqlx::query(
        r#"SELECT enrichment_status, enrichment_failures, mbid, mbid_source, bio, image_path,
                  image_source, "similar", last_error, last_error_code,
                  last_enriched_at IS NOT NULL AS enriched
           FROM artists WHERE id = $1"#,
    )
    .bind(artist)
    .fetch_one(pool)
    .await
    .unwrap();
    Row_ {
        status: r.get("enrichment_status"),
        failures: r.get("enrichment_failures"),
        mbid: r.get("mbid"),
        mbid_source: r.get("mbid_source"),
        bio: r.get("bio"),
        image_path: r.get("image_path"),
        image_source: r.get("image_source"),
        similar: r.get("similar"),
        last_error: r.get("last_error"),
        last_error_code: r.get("last_error_code"),
        enriched: r.get("enriched"),
    }
}

async fn mb_genres(pool: &PgPool, artist: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT g.name FROM artist_genres ag JOIN genres g ON g.id = ag.genre_id \
         WHERE ag.artist_id = $1 AND ag.source = 'musicbrainz' ORDER BY ag.rank",
    )
    .bind(artist)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn pause_musicbrainz(f: &Fx) {
    sqlx::query(
        "INSERT INTO source_pauses (source, paused_until, reason)
         VALUES ('musicbrainz', now() + interval '10 minutes', 'HTTP 503')",
    )
    .execute(&f.ctx.pool)
    .await
    .unwrap();
    f.ctx.net.load_pauses().await.unwrap();
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn stored(f: &Fx, artist: Uuid) -> Option<Vec<u8>> {
    std::fs::read(kima_library::artwork::artist_image_path(
        f.data_dir(),
        artist,
    ))
    .ok()
}

// ---- resolveArtistMbid.test.ts ----

async fn resolve(pool: &PgPool, world: World, name: &str, titles: &[&str]) -> (Outcome<Mbid>, Fx) {
    let f = fx(pool, world, World::default()).await;
    let titles: Vec<String> = titles.iter().map(|s| s.to_string()).collect();
    let out = artist::resolve_mbid(&f.ctx.net, name, &titles).await;
    (out, f)
}

fn search(hits: Value) -> Value {
    json!({ "artists": hits })
}

fn groups(titles: &[&str]) -> Value {
    let gs: Vec<Value> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| json!({"id": format!("22222222-0000-4000-8000-{i:012}"), "title": t}))
        .collect();
    json!({ "release-groups": gs, "release-group-count": gs.len() })
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_passes_an_outage_through(pool: PgPool) {
    let w = World::default().status(&mb_search_url("Low"), 500);
    let (out, _f) = resolve(&pool, w, "Low", &["Things We Lost in the Fire"]).await;
    assert!(matches!(out, Outcome::Transient(_)), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_rejects_a_hit_whose_name_only_contains_the_one_asked_for(pool: PgPool) {
    let w = World::default().get(
        &mb_search_url("Low"),
        search(json!([{"id": LOW_ROAR, "name": "Low Roar"}])),
    );
    let (out, _f) = resolve(&pool, w, "Low", &[]).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_accepts_a_hit_by_alias(pool: PgPool) {
    let w = World::default().get(
        &mb_search_url("Kanye West"),
        search(json!([{"id": YE, "name": "Ye", "aliases": [{"name": "Kanye West"}]}])),
    );
    let (out, _f) = resolve(&pool, w, "Kanye West", &[]).await;
    assert_eq!(answer(out), Some(mbid(YE)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_picks_the_namesake_whose_catalogue_holds_the_album(pool: PgPool) {
    let w = World::default()
        .get(
            &mb_search_url("Low"),
            search(json!([{"id": LOW_UK, "name": "Low"}, {"id": LOW, "name": "Low"}])),
        )
        .get(&rg_url(LOW_UK, 0), groups(&["Something Else"]))
        .get(&rg_url(LOW, 0), groups(&["Things We Lost in the Fire"]));
    let (out, _f) = resolve(&pool, w, "Low", &["Things We Lost In The Fire"]).await;
    assert_eq!(answer(out), Some(mbid(LOW)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_stores_nothing_when_no_catalogue_holds_the_album(pool: PgPool) {
    let w = World::default()
        .get(
            &mb_search_url("Low"),
            search(json!([{"id": LOW, "name": "Low"}])),
        )
        .get(&rg_url(LOW, 0), groups(&["Other"]));
    let (out, _f) = resolve(&pool, w, "Low", &["Things We Lost in the Fire"]).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_refuses_to_guess_between_namesakes_with_only_generic_titles(pool: PgPool) {
    let w = World::default().get(
        &mb_search_url("Low"),
        search(json!([{"id": LOW_UK, "name": "Low"}, {"id": LOW, "name": "Low"}])),
    );
    let (out, f) = resolve(&pool, w, "Low", &["Unknown Album"]).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert!(
        f.asked().iter().all(|a| !a.contains("/release-group")),
        "{:?}",
        f.asked()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_refuses_a_shared_name_when_the_artist_has_no_albums(pool: PgPool) {
    let w = World::default().get(
        &mb_search_url("Low"),
        search(json!([{"id": LOW_UK, "name": "Low"}, {"id": LOW, "name": "Low"}])),
    );
    let (out, _f) = resolve(&pool, w, "Low", &[]).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_takes_a_unique_name_when_the_artist_has_no_albums(pool: PgPool) {
    let w = World::default().get(
        &mb_search_url("Low"),
        search(json!([{"id": LOW, "name": "Low"}, {"id": LOW_ROAR, "name": "Low Roar"}])),
    );
    let (out, _f) = resolve(&pool, w, "Low", &[]).await;
    assert_eq!(answer(out), Some(mbid(LOW)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_passes_a_catalogue_outage_through(pool: PgPool) {
    let w = World::default()
        .get(
            &mb_search_url("Low"),
            search(json!([{"id": LOW, "name": "Low"}])),
        )
        .status(&rg_url(LOW, 0), 503);
    let (out, _f) = resolve(&pool, w, "Low", &["Things We Lost in the Fire"]).await;
    assert!(matches!(out, Outcome::RateLimited { .. }), "{out:?}");
}

/// kima-rs: at most [`MAX_CATALOGUE_CHECKS`] namesakes' catalogues are read, and a name with
/// collaboration noise is searched without it.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolve_reads_at_most_three_catalogues(pool: PgPool) {
    assert_eq!(MAX_CATALOGUE_CHECKS, 3);
    let ids: Vec<String> = (1..=4)
        .map(|i| format!("33333333-0000-4000-8000-{i:012}"))
        .collect();
    let mut w = World::default().get(
        &mb_search_url("Low"),
        search(json!(
            ids.iter()
                .map(|id| json!({"id": id, "name": "Low"}))
                .collect::<Vec<_>>()
        )),
    );
    for (i, id) in ids.iter().enumerate() {
        let title = if i == 3 { "The Album" } else { "Other" };
        w = w.get(&rg_url(id, 0), groups(&[title]));
    }
    let (out, f) = resolve(&pool, w, "Low (feat. Someone)", &["The Album"]).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    let catalogues = f
        .asked()
        .iter()
        .filter(|a| a.contains("/release-group"))
        .count();
    assert_eq!(catalogues, 3);
}

// ---- the guard (`dropMbidOfAnotherArtist`) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn guard_keeps_a_right_id(pool: PgPool) {
    let cases = [
        ("Deadmau5", held(M, "deadmau5", "deadmau5", &["Deadmau5"])),
        ("Sigur Ros", held(M, "Sigur Rós", "Sigur Rós", &[])),
        (
            "Glass, Philip",
            held(M, "Philip Glass", "Glass, Philip", &[]),
        ),
        (
            "Mumiy Troll",
            held(M, "Мумий Тролль", "Mumiy Troll", &["Mumiy Troll"]),
        ),
        ("Beatles", held(M, "The Beatles", "Beatles, The", &[])),
    ];
    for (local, says) in cases {
        let w = World::default().get(&mb_artist_url(M, "aliases"), says);
        let f = fx(&pool, w, World::default()).await;
        let out = artist::mbid_belongs(&f.ctx.net, mbid(M), &[local.to_string()]).await;
        assert_eq!(answer(out), Some(true), "{local}");
        sqlx::query("DELETE FROM http_cache")
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn guard_refuses_another_artists_id_or_an_unknown_one(pool: PgPool) {
    let w = World::default().get(
        &mb_artist_url(M, "aliases"),
        held(M, "Sam Cooke", "Cooke, Sam", &[]),
    );
    let f = fx(&pool, w, World::default()).await;
    let out = artist::mbid_belongs(&f.ctx.net, mbid(M), &["Alexis Ffrench".into()]).await;
    assert_eq!(answer(out), Some(false));

    let w = World::default().status(&mb_artist_url(LOW, "aliases"), 404);
    let f = fx(&pool, w, World::default()).await;
    let out = artist::mbid_belongs(&f.ctx.net, mbid(LOW), &["Low".into()]).await;
    assert_eq!(answer(out), Some(false));
}

// ---- artistFailureCap.test.ts ----

#[test]
fn failures_count_and_stay_retryable_below_the_cap() {
    assert_eq!(
        artist::failed_status(0),
        (identity::ArtistStatus::Failed, 1)
    );
    assert_eq!(
        artist::failed_status(2),
        (identity::ArtistStatus::Failed, 3)
    );
}

#[test]
fn the_failure_that_reaches_the_cap_is_unresolvable() {
    assert_eq!(
        artist::failed_status(3),
        (identity::ArtistStatus::Unresolvable, 4)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fourth_failure_is_unresolvable(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    set(
        &pool,
        a,
        "enrichment_status = 'failed', enrichment_failures = 3",
    )
    .await;
    let w = known("Perturbator", M).status(&sparql_url(M), 500);
    let f = fx(&pool, w, World::default()).await;

    let out = enrich(&f, a).await;

    assert_eq!(out, ArtistResult::Unresolvable);
    let r = row(&pool, a).await;
    assert_eq!((r.status.as_str(), r.failures), ("unresolvable", 4));
    assert_eq!(r.last_error_code.as_deref(), Some("HTTP_500"));
    assert!(r.last_error.is_some());
}

// ---- artistEnrichmentArtwork.test.ts ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn records_no_image_when_no_source_had_one(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let w = known("Perturbator", M).get(&fanart_url(M), json!({"name": "Perturbator"}));
    let f = fx(&pool, w, World::default()).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.status, "completed");
    assert_eq!((r.image_path, r.image_source), (None, None));
    assert!(stored(&f, a).is_none());
    assert!(r.enriched);
    assert_eq!(r.similar, Some(json!([])));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stores_the_image_a_source_found(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let w = known("Perturbator", M).get(
        &fanart_url(M),
        json!({"artistthumb": [{"url": "{images}/fanart/thumb.jpg"}]}),
    );
    let images = World::default().image("/fanart/thumb.jpg", &jpeg(2000, 1000, 1));
    let f = fx(&pool, w, images).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.image_source.as_deref(), Some("fanart"));
    assert_eq!(r.image_path, Some(format!("{a}.jpg")));
    let bytes = stored(&f, a).expect("the image is stored");
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (1500, 750));
    // The key never reaches an error, a log column or the cache's request text.
    let cache: Vec<String> = sqlx::query_scalar("SELECT request FROM http_cache")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(cache.iter().all(|c| !c.contains(FANART_KEY)), "{cache:?}");
}

/// Port of "does not hand a placeholder MBID to the sources": with no MBID, fanart.tv (keyed by
/// MBID) is not asked; Deezer (by name) is.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn without_an_mbid_fanart_is_not_asked(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    let w = World::default()
        .status(&mb_search_url("Perturbator"), 500)
        .get(
            &deezer_search_url("Perturbator"),
            json!({"data": [{"id": 77, "name": "Perturbator", "picture_xl": "{images}/dz.jpg"}]}),
        )
        .get("/artist/77/related?limit=30", json!({"data": []}));
    let images = World::default().image("/dz.jpg", &jpeg(300, 300, 2));
    let f = fx(&pool, w, images).await;

    let out = enrich(&f, a).await;

    assert!(matches!(out, ArtistResult::Failed { .. }), "{out:?}");
    assert!(
        f.asked().iter().all(|q| !q.contains("/music/")),
        "{:?}",
        f.asked()
    );
    let r = row(&pool, a).await;
    assert_eq!(r.image_source.as_deref(), Some("deezer"));
    assert_eq!((r.status.as_str(), r.failures), ("failed", 1));
    assert_eq!(r.mbid, None);
}

/// "drops the id and what it brought when MusicBrainz names someone else (Alexis Ffrench
/// holding Sam Cooke's)": MBID, bio, MusicBrainz genres and similar go; a local image stays.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn drops_another_artists_mbid(pool: PgPool) {
    let a = seed(&pool, "Alexis Ffrench", "Evolution").await;
    set_mbid(&pool, a, M, "tag").await;
    sqlx::query(
        r#"UPDATE artists SET bio = 'Sam Cooke was…', "similar" = '[{"name":"Otis Redding","mbid":null,"match":0.9}]',
                  image_path = $2, image_source = 'local'
           WHERE id = $1"#,
    )
    .bind(a)
    .bind(format!("{a}.jpg"))
    .execute(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    kima_db::genres::replace_artist_genres(
        &mut conn,
        a,
        kima_db::genres::GenreSource::MusicBrainz,
        &["soul".into()],
    )
    .await
    .unwrap();
    drop(conn);
    let w = World::default()
        .get(
            &mb_artist_url(M, "aliases"),
            held(M, "Sam Cooke", "Cooke, Sam", &[]),
        )
        .get(&mb_search_url("Alexis Ffrench"), search(json!([])));
    let f = fx(&pool, w, World::default()).await;
    put(&f.root, "Alexis Ffrench/artist.jpg", &png(200, 200));

    let out = enrich(&f, a).await;

    assert_eq!(out, ArtistResult::Unresolvable);
    let r = row(&pool, a).await;
    assert_eq!((r.mbid, r.mbid_source), (None, None));
    assert_eq!(r.bio, None);
    assert_eq!(r.similar, None);
    assert!(mb_genres(&pool, a).await.is_empty());
    assert_eq!(r.image_source.as_deref(), Some("local"));
    assert_eq!(r.image_path, Some(format!("{a}.jpg")));
    assert_eq!(r.status, "unresolvable");
}

/// A user's (or the autotagger's) MBID is never questioned or replaced.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_user_mbid_is_never_checked_or_replaced(pool: PgPool) {
    let a = seed(&pool, "Alexis Ffrench", "Evolution").await;
    set_mbid(&pool, a, M, "user").await;
    // MusicBrainz would name someone else; the guard is not asked.
    let w = known("Alexis Ffrench", M).get(
        &mb_artist_url(M, "aliases"),
        held(M, "Sam Cooke", "Cooke, Sam", &[]),
    );
    let f = fx(&pool, w, World::default()).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.mbid, Some(Uuid::parse_str(M).unwrap()));
    assert_eq!(r.mbid_source.as_deref(), Some("user"));
    assert!(
        f.asked().iter().all(|q| !q.contains("inc=aliases")),
        "{:?}",
        f.asked()
    );
}

/// "leaves the row alone while MusicBrainz is unreachable": the guard's outage stops the
/// enrichment, keeps the id and counts a failed try.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn leaves_the_row_alone_while_musicbrainz_is_unreachable(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "search").await;
    set(&pool, a, "bio = 'kept'").await;
    let w = World::default().status(&mb_artist_url(M, "aliases"), 500);
    let f = fx(&pool, w, World::default()).await;

    let out = enrich(&f, a).await;

    assert!(
        matches!(&out, ArtistResult::Failed { code, .. } if code == "HTTP_500"),
        "{out:?}"
    );
    let r = row(&pool, a).await;
    assert_eq!(r.mbid, Some(Uuid::parse_str(M).unwrap()));
    assert_eq!(r.bio.as_deref(), Some("kept"));
    assert_eq!((r.status.as_str(), r.failures), ("failed", 1));
    assert_eq!(f.asked().len(), 1, "{:?}", f.asked());
}

/// "waits for MusicBrainz's rate limit without counting a failure or calling the artist done".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn waits_for_a_rate_limit_without_counting_a_failure(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    set(
        &pool,
        a,
        "enrichment_status = 'failed', enrichment_failures = 2",
    )
    .await;
    let f = fx(&pool, known("Perturbator", M), World::default()).await;
    pause_musicbrainz(&f).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Pending);

    let r = row(&pool, a).await;
    assert_eq!((r.status.as_str(), r.failures), ("pending", 2));
    assert!(r.enriched);
}

/// "clears the artist's earlier failures from the failures list once it is answered".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_answer_clears_earlier_failures(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    set(
        &pool,
        a,
        "enrichment_status = 'failed', enrichment_failures = 2, last_error = 'x', last_error_code = 'HTTP_500'",
    )
    .await;
    let f = fx(&pool, known("Perturbator", M), World::default()).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!((r.status.as_str(), r.failures), ("completed", 0));
    assert_eq!((r.last_error, r.last_error_code), (None, None));
}

/// The whole happy path: a name resolved through its catalogue, Wikipedia's bio, MusicBrainz's
/// genres, ListenBrainz and Deezer's similar artists.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn resolves_and_fills_everything(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    let lb_id = "44444444-0000-4000-8000-000000000001";
    let w = known("Perturbator", M)
        .get(
            &mb_search_url("Perturbator"),
            search(json!([{"id": M, "name": "Perturbator"}])),
        )
        .get(&rg_url(M, 0), groups(&["Dangerous Days"]))
        .get(&sparql_url(M), sparql(Some("Q42")))
        .get(
            "/wiki/Special:EntityData/Q42.json",
            json!({"entities": {"Q42": {"sitelinks": {"enwiki": {"title": "Perturbator"}}}}}),
        )
        .get(
            "/api/rest_v1/page/summary/Perturbator",
            json!({"extract": "French synthwave musician."}),
        )
        .get(&lb_url(M), json!([{"artist_mbid": lb_id, "name": "Carpenter Brut", "score": 10}]))
        .get(
            &deezer_search_url("Perturbator"),
            json!({"data": [{"id": 9, "name": "Perturbator"}]}),
        )
        .get(
            "/artist/9/related?limit=30",
            json!({"data": [{"id": 1, "name": "carpenter brut"}, {"id": 2, "name": "Dance With The Dead"}]}),
        )
        .get(&fanart_url(M), json!({}));
    let f = fx(&pool, w, World::default()).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.mbid, Some(Uuid::parse_str(M).unwrap()));
    assert_eq!(r.mbid_source.as_deref(), Some("search"));
    assert_eq!(r.bio.as_deref(), Some("French synthwave musician."));
    let similar = r.similar.unwrap();
    let similar = similar.as_array().unwrap();
    assert_eq!(similar.len(), 2, "{similar:?}");
    assert_eq!(similar[0]["name"], "Carpenter Brut");
    assert_eq!(similar[0]["mbid"], lb_id);
    assert!((similar[0]["match"].as_f64().unwrap() - 0.95).abs() < 1e-9);
    assert_eq!(similar[1]["name"], "Dance With The Dead");
    assert_eq!(similar[1]["mbid"], Value::Null);
    assert!((similar[1]["match"].as_f64().unwrap() - 0.5).abs() < 1e-9);
}

// ---- artwork.test.ts (selected) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn local_image_beats_wikidata(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let w = known("Perturbator", M)
        .get(&sparql_url(M), sparql(Some("Q42")))
        .get(
            "/wiki/Special:EntityData/Q42.json",
            json!({"entities": {"Q42": {"claims": {"P18": [
                {"rank": "normal", "mainsnak": {"datavalue": {"value": "Perturbator live.jpg"}}}
            ]}}}}),
        )
        .get(
            &fanart_url(M),
            json!({"artistthumb": [{"url": "{images}/fanart.jpg"}]}),
        );
    let images = World::default().image("/fanart.jpg", &jpeg(300, 300, 3));
    let f = fx(&pool, w, images).await;
    let local = png(400, 300);
    put(&f.root, "Perturbator/artist.png", &local);

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.image_source.as_deref(), Some("local"));
    // Nothing remote was downloaded (a Commons URL would have tripped the network guard).
    assert!(f.images_asked().is_empty(), "{:?}", f.images_asked());
    let img = image::load_from_memory(&stored(&f, a).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (400, 300));
}

/// A failed source keeps what was stored: the bio (Wikidata failed), the similar list (Deezer
/// failed) and the image (fanart failed, Deezer could not be asked).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_source_never_records_none(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let old_similar = json!([{"name": "Old", "mbid": null, "match": 0.9}]);
    sqlx::query(
        r#"UPDATE artists SET bio = 'old bio', "similar" = $2, image_path = $3,
                  image_source = 'deezer' WHERE id = $1"#,
    )
    .bind(a)
    .bind(&old_similar)
    .bind(format!("{a}.jpg"))
    .execute(&pool)
    .await
    .unwrap();
    let w = known("Perturbator", M)
        .status(&sparql_url(M), 500)
        .status(&fanart_url(M), 500)
        .status(&deezer_search_url("Perturbator"), 500);
    let f = fx(&pool, w, World::default()).await;
    kima_library::artwork::store_artist_image(f.data_dir(), a, b"old image").unwrap();

    let out = enrich(&f, a).await;

    assert!(matches!(out, ArtistResult::Failed { .. }), "{out:?}");
    let r = row(&pool, a).await;
    assert_eq!(r.bio.as_deref(), Some("old bio"));
    assert_eq!(r.similar, Some(old_similar));
    assert_eq!(r.image_source.as_deref(), Some("deezer"));
    assert_eq!(r.image_path, Some(format!("{a}.jpg")));
    assert_eq!(stored(&f, a).as_deref(), Some(&b"old image"[..]));
    assert_eq!((r.status.as_str(), r.failures), ("failed", 1));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn higher_source_replaces_lower(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    set(
        &pool,
        a,
        &format!("image_path = '{a}.jpg', image_source = 'deezer'"),
    )
    .await;
    let new = jpeg(300, 300, 4);
    let w = known("Perturbator", M).get(
        &fanart_url(M),
        json!({"artistbackground": [{"url": "{images}/bg.jpg"}]}),
    );
    let f = fx(&pool, w, World::default().image("/bg.jpg", &new)).await;
    kima_library::artwork::store_artist_image(f.data_dir(), a, b"deezer image").unwrap();

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.image_source.as_deref(), Some("fanart"));
    assert_ne!(stored(&f, a).as_deref(), Some(&b"deezer image"[..]));

    // A lower source does not replace a higher one: Deezer's picture leaves fanart's alone.
    let b = seed(&pool, "Carpenter Brut", "Trilogy").await;
    let cb = "44444444-0000-4000-8000-000000000009";
    set_mbid(&pool, b, cb, "tag").await;
    set(
        &pool,
        b,
        &format!("image_path = '{b}.jpg', image_source = 'fanart'"),
    )
    .await;
    let w = known("Carpenter Brut", cb)
        .get(
            &deezer_search_url("Carpenter Brut"),
            json!({"data": [{"id": 5, "name": "Carpenter Brut", "picture_xl": "{images}/dz.jpg"}]}),
        )
        .get("/artist/5/related?limit=30", json!({"data": []}));
    let f = fx(
        &pool,
        w,
        World::default().image("/dz.jpg", &jpeg(300, 300, 5)),
    )
    .await;
    kima_library::artwork::store_artist_image(f.data_dir(), b, b"fanart image").unwrap();

    assert_eq!(enrich(&f, b).await, ArtistResult::Completed);

    let r = row(&pool, b).await;
    assert_eq!(r.image_source.as_deref(), Some("fanart"));
    assert_eq!(stored(&f, b).as_deref(), Some(&b"fanart image"[..]));
    assert!(f.images_asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_image_never_replaced(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    set(
        &pool,
        a,
        &format!("image_path = '{a}.jpg', image_source = 'user'"),
    )
    .await;
    let w = known("Perturbator", M).get(
        &fanart_url(M),
        json!({"artistthumb": [{"url": "{images}/fanart.jpg"}]}),
    );
    let f = fx(
        &pool,
        w,
        World::default().image("/fanart.jpg", &jpeg(300, 300, 6)),
    )
    .await;
    kima_library::artwork::store_artist_image(f.data_dir(), a, b"the user's image").unwrap();
    put(&f.root, "Perturbator/artist.jpg", &png(200, 200));

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.image_source.as_deref(), Some("user"));
    assert_eq!(stored(&f, a).as_deref(), Some(&b"the user's image"[..]));
    assert!(f.images_asked().is_empty());
}

// ---- artistSimilarity.tiers.test.ts ----

fn lb(n: usize) -> Vec<Similar> {
    (0..n)
        .map(|i| Similar {
            name: format!("LB {i}"),
            mbid: Some(mbid(&format!("55555555-0000-4000-8000-{i:012}"))),
            score: (100 - i) as f64,
        })
        .collect()
}

fn names(es: &[SimilarEntry]) -> Vec<&str> {
    es.iter().map(|e| e.name.as_str()).collect()
}

#[test]
fn listenbrainz_alone_when_it_fills_the_list() {
    let got = similar::merge(&lb(30), &["Dz One".into()]);
    assert_eq!(got.len(), 30);
    assert!(got.iter().all(|e| e.name.starts_with("LB ")));
    assert!((got[0].r#match - 0.95).abs() < 1e-9);
    assert!((got[29].r#match - (0.5 + 0.45 * 71.0 / 100.0)).abs() < 1e-9);
}

#[test]
fn deezer_when_listenbrainz_is_empty() {
    let got = similar::merge(&[], &["Dz One".into(), "Dz Two".into(), "Dz Three".into()]);
    assert_eq!(names(&got), ["Dz One", "Dz Two", "Dz Three"]);
    for (e, want) in got.iter().zip([0.9, 0.7, 0.5]) {
        assert!((e.r#match - want).abs() < 1e-9, "{e:?}");
    }
    assert!(got.iter().all(|e| e.mbid.is_none()));
}

#[test]
fn merge_dedupes_by_name_listenbrainz_first() {
    let shared = Similar {
        name: "Shared Artist".into(),
        mbid: Some(mbid(LOW)),
        score: 2.0,
    };
    let got = similar::merge(&[shared], &["shared artist".into(), "Dz Only".into()]);
    assert_eq!(names(&got), ["Shared Artist", "Dz Only"]);
    assert_eq!(got[0].mbid, Some(mbid(LOW)));
    // Cut at 30 overall.
    let dz: Vec<String> = (0..40).map(|i| format!("Dz {i}")).collect();
    assert_eq!(similar::merge(&lb(5), &dz).len(), 30);
}

#[test]
fn both_empty_is_empty() {
    assert!(similar::merge(&[], &[]).is_empty());
}

#[test]
fn entries_serialise_as_name_mbid_match() {
    let e = SimilarEntry {
        name: "X".into(),
        mbid: None,
        r#match: 0.5,
    };
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"name": "X", "mbid": null, "match": 0.5})
    );
}

/// "a failed source keeps the old value and fails the step".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_failed_similar_source_keeps_the_old_list(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let old = json!([{"name": "Old", "mbid": null, "match": 0.9}]);
    sqlx::query(r#"UPDATE artists SET "similar" = $2 WHERE id = $1"#)
        .bind(a)
        .bind(&old)
        .execute(&pool)
        .await
        .unwrap();
    let w = known("Perturbator", M)
        .status(&lb_url(M), 500)
        .get(&fanart_url(M), json!({}));
    let f = fx(&pool, w, World::default()).await;

    let out = enrich(&f, a).await;

    assert!(matches!(out, ArtistResult::Failed { .. }), "{out:?}");
    let r = row(&pool, a).await;
    assert_eq!(r.similar, Some(old));
    assert_eq!((r.status.as_str(), r.failures), ("failed", 1));
}

// ---- genres, Various Artists, the claim ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_genres_top5_only_when_non_empty(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let genres: Vec<Value> = [
        ("synthwave", 9),
        ("darksynth", 8),
        ("electronic", 7),
        ("outrun", 6),
        ("cyberpunk", 5),
        ("industrial", 4),
        ("metal", 3),
    ]
    .iter()
    .map(|(n, c)| json!({"name": n, "count": c}))
    .collect();
    let w = known("Perturbator", M)
        .get(
            &mb_artist_url(M, "genres"),
            json!({"id": M, "name": "Perturbator", "genres": genres}),
        )
        .get(&fanart_url(M), json!({}));
    let f = fx(&pool, w, World::default()).await;
    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);
    assert_eq!(
        mb_genres(&pool, a).await,
        [
            "synthwave",
            "darksynth",
            "electronic",
            "outrun",
            "cyberpunk"
        ]
    );

    // MusicBrainz with no genres leaves the stored ones alone.
    sqlx::query("DELETE FROM http_cache")
        .execute(&pool)
        .await
        .unwrap();
    set(&pool, a, "enrichment_status = 'pending'").await;
    let w = known("Perturbator", M).get(&fanart_url(M), json!({}));
    let f = fx(&pool, w, World::default()).await;
    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);
    assert_eq!(mb_genres(&pool, a).await.len(), 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn va_never_looked_up(pool: PgPool) {
    let va = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, enrichment_status) \
         VALUES ($1, 'Various Artists', 'various artists', 'pending')",
    )
    .bind(va)
    .execute(&pool)
    .await
    .unwrap();
    add_album(&pool, va, "Various Artists", "Hits").await;
    assert!(identity::claim_artists(&pool, 10).await.unwrap().is_empty());

    let f = fx(&pool, World::default(), World::default()).await;
    let report = artist::run_batch(&f.ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.claimed, 0);
    assert!(f.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_order_and_unresolvable_retry_after_7_days(pool: PgPool) {
    let fresh = seed(&pool, "Fresh", "A").await;
    let failed = seed(&pool, "Failed", "A").await;
    set(
        &pool,
        failed,
        "enrichment_status = 'failed', last_enriched_at = now() - interval '1 hour'",
    )
    .await;
    let old = seed(&pool, "Old", "A").await;
    set(
        &pool,
        old,
        "enrichment_status = 'unresolvable', last_enriched_at = now() - interval '8 days'",
    )
    .await;
    let recent = seed(&pool, "Recent", "A").await;
    set(
        &pool,
        recent,
        "enrichment_status = 'unresolvable', last_enriched_at = now() - interval '1 day'",
    )
    .await;
    let done = seed(&pool, "Done", "A").await;
    set(&pool, done, "enrichment_status = 'completed'").await;
    let gone = seed(&pool, "Gone", "A").await;
    sqlx::query(
        "UPDATE tracks SET missing_since = now() WHERE album_id IN \
         (SELECT id FROM albums WHERE artist_id = $1)",
    )
    .bind(gone)
    .execute(&pool)
    .await
    .unwrap();

    let mut order = Vec::new();
    loop {
        let got = identity::claim_artists(&pool, 1).await.unwrap();
        let Some(a) = got.into_iter().next() else {
            break;
        };
        order.push(a.id);
    }
    assert_eq!(order, [fresh, old, failed]);
    for id in [fresh, old, failed] {
        assert_eq!(row(&pool, id).await.status, "enriching");
    }

    // What a claim carries.
    identity::release_artists(&pool, &[fresh]).await.unwrap();
    assert_eq!(row(&pool, fresh).await.status, "pending");
    add_album(&pool, fresh, "Fresh", "Unknown Album").await;
    let a = identity::claim_artists(&pool, 1).await.unwrap().remove(0);
    assert_eq!(a.id, fresh);
    assert_eq!(a.name, "Fresh");
    let mut titles = a.album_titles.clone();
    titles.sort();
    assert_eq!(titles, ["A", "Unknown Album"]);
    let mut dirs = a.album_dirs.clone();
    dirs.sort();
    assert_eq!(dirs, ["Fresh/A", "Fresh/Unknown Album"]);
}

/// A batch runs up to `enrichmentConcurrency` artists and closes each; cancelled before it
/// starts, it claims nothing.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn run_batch_enriches_the_claimed_artists(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let b = seed(&pool, "Nobody", "Nothing").await;
    let w = known("Perturbator", M)
        .get(&fanart_url(M), json!({}))
        .get(&mb_search_url("Nobody"), search(json!([])));
    let f = fx(&pool, w, World::default()).await;

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let report = artist::run_batch(&f.ctx, &cancelled).await.unwrap();
    assert_eq!(report.claimed, 0);

    let report = artist::run_batch(&f.ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((report.claimed, report.done, report.failed), (2, 2, 0));
    assert_eq!(row(&pool, a).await.status, "completed");
    assert_eq!(row(&pool, b).await.status, "unresolvable");
}

// ---- review fixes (Task 21 round 1) ----

/// The Wikidata image end to end: the `P18` file becomes a Commons URL under the `commons` base
/// (the replay server here), which is downloaded, re-encoded and stored.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_image_is_downloaded_and_stored(pool: PgPool) {
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set_mbid(&pool, a, M, "tag").await;
    let commons_path = kima_ident::wikidata::commons_url("", "Perturbator live.jpg");
    let w = known("Perturbator", M)
        .get(&sparql_url(M), sparql(Some("Q42")))
        .get(
            "/wiki/Special:EntityData/Q42.json",
            json!({"entities": {"Q42": {"claims": {"P18": [
                {"rank": "normal", "mainsnak": {"datavalue": {"value": "Perturbator live.jpg"}}}
            ]}}}}),
        )
        .image(&commons_path, &jpeg(1800, 600, 7));
    let f = fx(&pool, w, World::default()).await;

    assert_eq!(enrich(&f, a).await, ArtistResult::Completed);

    let r = row(&pool, a).await;
    assert_eq!(r.image_source.as_deref(), Some("wikidata"));
    assert_eq!(r.image_path, Some(format!("{a}.jpg")));
    let img = image::load_from_memory(&stored(&f, a).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (1500, 500));
    assert!(
        f.asked().iter().any(|q| q.contains("/wikipedia/commons/")),
        "{:?}",
        f.asked()
    );
    // fanart.tv ranks below Wikidata: not asked once Wikidata's image is stored.
    assert!(
        f.asked().iter().all(|q| !q.contains("/music/")),
        "{:?}",
        f.asked()
    );
}

/// A `failed` artist waits out the retry grace (15 min, database clock) before it is claimed
/// again, so a short outage cannot run it to the failure cap.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_failed_artist_waits_out_the_retry_grace(pool: PgPool) {
    assert_eq!(
        identity::FAILED_RETRY_GRACE,
        std::time::Duration::from_secs(15 * 60)
    );
    let a = seed(&pool, "Perturbator", "Dangerous Days").await;
    set(&pool, a, "enrichment_status = 'failed', enrichment_failures = 1, last_enriched_at = now() - interval '14 minutes'").await;
    assert!(identity::claim_artists(&pool, 10).await.unwrap().is_empty());
    set(&pool, a, "last_enriched_at = now() - interval '16 minutes'").await;
    let claimed = identity::claim_artists(&pool, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, a);
}
