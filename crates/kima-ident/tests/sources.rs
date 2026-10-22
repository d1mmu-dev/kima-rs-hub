//! The enrichment source clients against the replay server: Wikidata (`wikidata.outcome.test.ts`),
//! Deezer (`deezerVerifiedMatch.test.ts`, `deezerRelatedArtists.test.ts`), ListenBrainz
//! (`listenBrainz.similar.test.ts`), fanart.tv and the Cover Art Archive, plus kima-rs's own
//! secret, cache and untrusted-answer cases. Nothing here reaches a real service: every answer is
//! a hand-written entry (`tests/fixtures/README.md`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use kima_core::identify::IdentifySwitch;
use kima_db::settings_kv::KvSettings;
use kima_ident::Mbid;
use kima_ident::{coverartarchive, deezer, fanart, listenbrainz, wikidata};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;

const BEATLES: &str = "b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d";
const ABBEY_ROAD: &str = "9162580e-5df4-32de-80cc-f45a8d8a9b1d";
/// Upstream's Wikidata and ListenBrainz test MBID (Pink Floyd).
const SEED: &str = "83d91898-7763-47d7-b03b-b92132375c47";
const ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_3_threshold_10_limit_100_filter_True_skip_30";

fn fixture(dir: &str) -> Vec<Entry> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir)
        .join("beatles.json");
    Recording::load(path).0
}

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

async fn net_for(pool: &PgPool, entries: Vec<Entry>) -> (Replay, Net) {
    let replay = Replay::start(Recording(entries)).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    (replay, net)
}

fn mbid(s: &str) -> Mbid {
    Mbid::parse(s).unwrap()
}

fn answered<T: std::fmt::Debug>(o: Outcome<T>) -> T {
    match o {
        Outcome::Answered(t) => t,
        other => panic!("expected an answer, got {other:?}"),
    }
}

async fn cache_rows(pool: &PgPool) -> Vec<(String, String, String, String, String)> {
    sqlx::query_as(
        "SELECT key, source, request, outcome, body::text FROM http_cache ORDER BY fetched_at, key",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

// ---- Wikidata ----

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

fn entity_url(q: &str) -> String {
    format!("/wiki/Special:EntityData/{q}.json")
}

/// Port 1: "never puts a value that is not a MusicBrainz id into the query". In kima-rs the
/// lookup takes a typed [`Mbid`], so the values upstream feeds it cannot be built; a real MBID
/// goes into the query as the spec's text.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_only_mbids_reach_the_query(pool: PgPool) {
    for bad in ["temp-1a2b", "12345", "83d91898\" . } ?x ?y ?z { \"", ""] {
        assert!(Mbid::parse(bad).is_none(), "{bad}");
    }
    let (replay, net) = net_for(&pool, vec![entry(&sparql_url(SEED), 200, sparql(None))]).await;
    answered(wikidata::lookup(&net, mbid(SEED)).await);
    let asked = replay.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0], format!("GET {}", sparql_url(SEED)));
}

/// Port 2: "answers null, uncached, when a source fails".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_failure_is_not_no_bio(pool: PgPool) {
    let (_replay, net) = net_for(
        &pool,
        vec![
            entry(&sparql_url(SEED), 200, sparql(Some("Q2306"))),
            entry(&entity_url("Q2306"), 503, json!({"error": "unavailable"})),
        ],
    )
    .await;
    let out = wikidata::lookup(&net, mbid(SEED)).await;
    assert_eq!(out.code(), Some("HTTP_503"), "{out:?}");
    // Only the SPARQL answer is kept; the failure is not, and no "{}" stands in for it.
    let rows = cache_rows(&pool).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].2.contains("/sparql"), "{:?}", rows[0]);
}

/// Port 3: "caches an artist Wikidata has no entry for".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_no_entry_is_answered_empty_and_cached(pool: PgPool) {
    let (replay, net) = net_for(&pool, vec![entry(&sparql_url(SEED), 200, sparql(None))]).await;
    let info = answered(wikidata::lookup(&net, mbid(SEED)).await);
    assert_eq!(info, wikidata::WikiInfo::default());
    let rows = cache_rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].1.as_str(), rows[0].3.as_str()),
        ("wikidata", "absent")
    );
    // Asked again: the cache answers.
    assert_eq!(
        answered(wikidata::lookup(&net, mbid(SEED)).await),
        wikidata::WikiInfo::default()
    );
    assert_eq!(replay.asked().len(), 1);
}

/// Port 4: "treats a missing English article as no summary, not a failure".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_missing_article_is_no_summary(pool: PgPool) {
    let (_replay, net) = net_for(
        &pool,
        vec![
            entry(&sparql_url(SEED), 200, sparql(Some("Q1"))),
            entry(
                &entity_url("Q1"),
                200,
                json!({"entities": {"Q1": {"sitelinks": {"enwiki": {"title": "X"}}, "claims": {}}}}),
            ),
            entry("/api/rest_v1/page/summary/X", 404, json!({"type": "not_found"})),
        ],
    )
    .await;
    let info = answered(wikidata::lookup(&net, mbid(SEED)).await);
    assert_eq!(
        info,
        wikidata::WikiInfo {
            bio: None,
            image_url: None
        }
    );
}

/// Port 5: "takes the preferred image and never a deprecated one".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_takes_the_preferred_image(pool: PgPool) {
    let claim =
        |rank: &str, file: &str| json!({"rank": rank, "mainsnak": {"datavalue": {"value": file}}});
    let (_replay, net) = net_for(
        &pool,
        vec![
            entry(&sparql_url(SEED), 200, sparql(Some("Q1"))),
            entry(
                &entity_url("Q1"),
                200,
                json!({"entities": {"Q1": {"claims": {"P18": [
                    claim("deprecated", "Wrong.jpg"),
                    claim("normal", "Ok.jpg"),
                    claim("preferred", "Best.jpg"),
                ]}}}}),
            ),
        ],
    )
    .await;
    let info = answered(wikidata::lookup(&net, mbid(SEED)).await);
    assert!(
        info.image_url.as_deref().unwrap().ends_with("/Best.jpg"),
        "{info:?}"
    );
    assert_eq!(info.bio, None, "no enwiki sitelink: Wikipedia is not asked");
}

#[test]
fn p18_prefers_preferred_then_normal_never_deprecated() {
    let claim = |rank: &str, file: &str| json!({"rank": rank, "mainsnak": {"datavalue": {"value": file, "type": "string"}}});
    let entity = |claims: Vec<Value>| json!({"claims": {"P18": claims}});
    let file = |e: &Value| {
        wikidata::image_of(e, kima_net::COMMONS_BASE)
            .map(|u| u.rsplit('/').next().unwrap().to_string())
    };
    assert_eq!(
        file(&entity(vec![
            claim("normal", "A.jpg"),
            claim("preferred", "B.jpg"),
            claim("preferred", "C.jpg"),
        ])),
        Some("B.jpg".into())
    );
    assert_eq!(
        file(&entity(vec![
            claim("deprecated", "A.jpg"),
            claim("normal", "B.jpg"),
            claim("normal", "C.jpg"),
        ])),
        Some("B.jpg".into())
    );
    assert_eq!(file(&entity(vec![claim("deprecated", "A.jpg")])), None);
    // Untrusted shapes: nothing panics, nothing is made up.
    for e in [
        json!({}),
        json!({"claims": null}),
        json!({"claims": {"P18": "x"}}),
        json!({"claims": {"P18": [1, null, {"rank": "preferred"}]}}),
        json!({"claims": {"P18": [{"rank": "preferred", "mainsnak": {"datavalue": {"value": 7}}}]}}),
        json!({"claims": {"P18": [{"rank": "normal", "mainsnak": {"datavalue": {"value": ""}}}]}}),
        json!({"claims": {"P18": [{"rank": "normal", "mainsnak": {"datavalue": {"value": "x".repeat(300)}}}]}}),
    ] {
        assert_eq!(wikidata::image_of(&e, kima_net::COMMONS_BASE), None, "{e}");
    }
}

#[test]
fn commons_url_uses_md5_path() {
    // md5("Example.jpg") = a91fe217...: Commons' own example file.
    assert_eq!(
        wikidata::commons_url(kima_net::COMMONS_BASE, "Example.jpg"),
        "https://upload.wikimedia.org/wikipedia/commons/a/a9/Example.jpg"
    );
    // Spaces become underscores before hashing; the name is encoded as `encodeURIComponent`
    // encodes it (parentheses kept). md5("Paul_McCartney_(1964).jpg") = abbfb9f3...
    assert_eq!(
        wikidata::commons_url(kima_net::COMMONS_BASE, "Paul McCartney (1964).jpg"),
        "https://upload.wikimedia.org/wikipedia/commons/a/ab/Paul_McCartney_(1964).jpg"
    );
    // md5("Björk_Cannes_2000.jpg") = 73f10278...
    assert_eq!(
        wikidata::commons_url(kima_net::COMMONS_BASE, "Björk Cannes 2000.jpg"),
        "https://upload.wikimedia.org/wikipedia/commons/7/73/Bj%C3%B6rk_Cannes_2000.jpg"
    );
    // A name that would break the URL is encoded.
    assert!(
        wikidata::commons_url(kima_net::COMMONS_BASE, "a/b?c#d&e.jpg")
            .ends_with("/a%2Fb%3Fc%23d%26e.jpg")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn summary_404_is_no_bio_not_failure(pool: PgPool) {
    let mut entries = fixture("wikidata");
    // The article is gone; the image still comes from the entity.
    entries[2].status = 404;
    entries[2].body = json!({"type": "https://mediawiki.org/wiki/HyperSwitch/errors/not_found"});
    let (replay, net) = net_for(&pool, entries).await;
    let info = answered(wikidata::lookup(&net, mbid(BEATLES)).await);
    assert_eq!(info.bio, None);
    assert_eq!(
        info.image_url,
        Some(format!(
            "{}/wikipedia/commons/d/df/The_Fabs.JPG",
            replay.url()
        ))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_beatles_bio_and_image(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture("wikidata")).await;
    let info = answered(wikidata::lookup(&net, mbid(BEATLES)).await);
    assert!(
        info.bio
            .as_deref()
            .unwrap()
            .starts_with("The Beatles were an English rock band"),
        "{info:?}"
    );
    // The Commons host is the test base (production: `COMMONS_BASE`).
    assert_eq!(
        info.image_url,
        Some(format!(
            "{}/wikipedia/commons/d/df/The_Fabs.JPG",
            replay.url()
        ))
    );
    let asked = replay.asked();
    assert_eq!(asked.len(), 3, "{asked:?}");
    assert_eq!(asked[2], "GET /api/rest_v1/page/summary/The%20Beatles");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_untrusted_answers(pool: PgPool) {
    let long: String = "é".repeat(wikidata::MAX_BIO_CHARS + 100);
    let title = "AC/DC & Friends?";
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(&sparql_url(SEED), 200, sparql(Some("Q5"))),
            // A redirected item answers under its new id: the one entity is used.
            entry(
                &entity_url("Q5"),
                200,
                json!({"entities": {"Q6": {"sitelinks": {"enwiki": {"title": title}}, "claims": {"P18": 3}}}}),
            ),
            entry(
                "/api/rest_v1/page/summary/AC%2FDC%20%26%20Friends%3F",
                200,
                json!({"extract": long}),
            ),
        ],
    )
    .await;
    let info = answered(wikidata::lookup(&net, mbid(SEED)).await);
    let bio = info.bio.unwrap();
    assert_eq!(bio.chars().count(), wikidata::MAX_BIO_CHARS);
    assert_eq!(info.image_url, None);
    assert_eq!(
        replay.asked()[2],
        "GET /api/rest_v1/page/summary/AC%2FDC%20%26%20Friends%3F"
    );

    // An item id that is not `Q<digits>` never reaches a path.
    let other = "00000000-0000-4000-8000-000000000001";
    let (replay, net) = net_for(
        &pool,
        vec![entry(
            &sparql_url(other),
            200,
            json!({"results": {"bindings": [{"item": {"value": "http://www.wikidata.org/entity/../../w/api.php"}}]}}),
        )],
    )
    .await;
    assert_eq!(
        answered(wikidata::lookup(&net, mbid(other)).await),
        wikidata::WikiInfo::default()
    );
    assert_eq!(replay.asked().len(), 1);

    // An answer without `results` is a bad response, not "no entry", and is not cached.
    let third = "00000000-0000-4000-8000-000000000002";
    let (_replay, net) = net_for(
        &pool,
        vec![entry(&sparql_url(third), 200, json!({"oops": 1}))],
    )
    .await;
    assert_eq!(
        wikidata::lookup(&net, mbid(third)).await.code(),
        Some("BAD_RESPONSE")
    );
}

// ---- Deezer ----

fn deezer_search_artist(name: &str) -> String {
    format!("/search/artist?q={}&limit=1", enc(name))
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

fn deezer_album_urls(artist: &str, title: &str) -> (String, String) {
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

async fn album_cover_with(pool: &PgPool, hits: Value) -> Outcome<Option<String>> {
    let (structured, _) = deezer_album_urls("Radiohead", "OK Computer");
    let (_replay, net) =
        net_for(pool, vec![entry(&structured, 200, json!({ "data": hits }))]).await;
    deezer::album_cover(&net, "Radiohead", "OK Computer").await
}

/// `deezerVerifiedMatch.test.ts`, `getAlbumCover` (4 cases).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_album_cover_verified_match_only(pool: PgPool) {
    let hit = |title: &str, artist: &str, cover: &str| json!({"title": title, "artist": {"name": artist}, "cover_xl": cover});
    assert_eq!(
        answered(
            album_cover_with(
                &pool,
                json!([hit("OK Computer", "Radiohead", "https://deezer/ok.jpg")])
            )
            .await
        ),
        Some("https://deezer/ok.jpg".into())
    );
    assert_eq!(
        answered(
            album_cover_with(
                &pool,
                json!([hit("Pablo Honey", "Radiohead", "https://deezer/pablo.jpg")])
            )
            .await
        ),
        None,
        "no first-hit fallback"
    );
    assert_eq!(
        answered(
            album_cover_with(
                &pool,
                json!([hit(
                    "OK Computer",
                    "Radiohead Tribute Band",
                    "https://deezer/fake.jpg"
                )])
            )
            .await
        ),
        None
    );
    assert_eq!(
        answered(
            album_cover_with(
                &pool,
                json!([
                    hit("Amnesiac", "Radiohead", "https://deezer/amnesiac.jpg"),
                    hit("OK Computer", "Radiohead", "https://deezer/ok.jpg"),
                ])
            )
            .await
        ),
        Some("https://deezer/ok.jpg".into())
    );
}

/// `deezerVerifiedMatch.test.ts`, `getArtistImage` (2 cases).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_artist_image_verified_match_only(pool: PgPool) {
    let body = json!({"data": [{"id": 5, "name": "The Only Ones", "picture_xl": "https://deezer/right.jpg"}]});
    let (_replay, net) = net_for(
        &pool,
        vec![
            entry(&deezer_search_artist("Someone Else"), 200, body.clone()),
            entry(&deezer_search_artist("The Only Ones"), 200, body),
        ],
    )
    .await;
    assert_eq!(
        answered(deezer::find_artist(&net, "Someone Else").await),
        None
    );
    assert_eq!(
        answered(deezer::find_artist(&net, "The Only Ones").await),
        Some(deezer::DeezerArtist {
            id: 5,
            name: "The Only Ones".into(),
            picture: Some("https://deezer/right.jpg".into()),
        })
    );
}

/// `deezerVerifiedMatch.test.ts`, "does not serve a mis-resolved seed's related list", and
/// `deezerRelatedArtists.test.ts` "returns [] when the artist search finds nothing": the seed is
/// `find_artist`, which answers `None`, so `related` is never asked.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_unverified_seed_asks_no_related_list(pool: PgPool) {
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(
                &deezer_search_artist("Seed Artist"),
                200,
                json!({"data": [{"id": 999, "name": "Someone Else"}]}),
            ),
            entry(
                &deezer_search_artist("Nobody"),
                200,
                json!({"data": [], "total": 0}),
            ),
            entry(
                "/artist/999/related?limit=30",
                200,
                json!({"data": [{"id": 1, "name": "Wrong Neighbour"}]}),
            ),
        ],
    )
    .await;
    assert_eq!(
        answered(deezer::find_artist(&net, "Seed Artist").await),
        None
    );
    assert_eq!(answered(deezer::find_artist(&net, "Nobody").await), None);
    assert_eq!(replay.asked().len(), 2, "{:?}", replay.asked());
    // An empty search is an absence in the cache (spec §7.4).
    let rows = cache_rows(&pool).await;
    let outcomes: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    assert_eq!(outcomes, ["answered", "absent"]);
}

/// `deezerVerifiedMatch.test.ts` "a failed request is not a 'no'".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_failure_is_not_a_no(pool: PgPool) {
    let (structured, _) = deezer_album_urls("Some Artist", "Some Album");
    let (_replay, net) = net_for(
        &pool,
        vec![
            entry(&structured, 500, json!({})),
            entry(&deezer_search_artist("Some Artist"), 502, json!({})),
        ],
    )
    .await;
    assert!(matches!(
        deezer::album_cover(&net, "Some Artist", "Some Album").await,
        Outcome::Transient(_)
    ));
    assert!(matches!(
        deezer::find_artist(&net, "Some Artist").await,
        Outcome::Transient(_)
    ));
    assert!(cache_rows(&pool).await.is_empty());
}

/// `deezerRelatedArtists.test.ts`: the list in Deezer's order, the seed left out, an empty list,
/// a failed request. The position match (0.9 to 0.5) and the link are Task 21's `similar::merge`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_related_artists(pool: PgPool) {
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(
                "/artist/123/related?limit=30",
                200,
                json!({"data": [
                    {"id": 1, "name": "Rel One", "nb_fan": 90000, "link": "https://www.deezer.com/artist/1"},
                    {"id": 2, "name": "Rel Two", "nb_fan": 50000},
                    {"id": 3, "name": "Rel Three", "nb_fan": 10000},
                ]}),
            ),
            entry(
                "/artist/124/related?limit=30",
                200,
                json!({"data": [{"id": 124, "name": "Seed Artist"}, {"id": 2, "name": "Rel Two"}]}),
            ),
            entry("/artist/125/related?limit=30", 200, json!({"data": []})),
            entry("/artist/126/related?limit=30", 503, json!({})),
        ],
    )
    .await;
    assert_eq!(
        answered(deezer::related(&net, 123).await),
        ["Rel One", "Rel Two", "Rel Three"]
    );
    assert_eq!(answered(deezer::related(&net, 124).await), ["Rel Two"]);
    assert_eq!(
        answered(deezer::related(&net, 125).await),
        Vec::<String>::new()
    );
    assert!(matches!(
        deezer::related(&net, 126).await,
        Outcome::Transient(_)
    ));
    assert_eq!(replay.asked().len(), 4);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_album_cover_needs_artist_and_title_agree(pool: PgPool) {
    // The Beatles fixture: the first hit is a different edition, the second agrees.
    let (replay, net) = net_for(&pool, fixture("deezer")).await;
    let cover = answered(deezer::album_cover(&net, "The Beatles", "Abbey Road").await).unwrap();
    assert!(cover.ends_with("/1000x1000-000000-80-0-0.jpg"), "{cover}");
    assert_eq!(replay.asked().len(), 1);

    // No structured hit: the plain query is asked; its hit must still agree on both.
    let (structured, plain) = deezer_album_urls("Björk", "Debut");
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(&structured, 200, json!({"data": []})),
            entry(
                &plain,
                200,
                json!({"data": [
                    {"title": "Debut", "artist": {"name": "Bjork Tribute"}, "cover_xl": "https://x/1.jpg"},
                    {"title": "Debut (Live)", "artist": {"name": "Björk"}, "cover_xl": "https://x/2.jpg"},
                    {"title": "Debut", "artist": {"name": "Björk"}, "cover_big": "https://x/3.jpg", "cover_medium": "https://x/4.jpg"},
                ]}),
            ),
        ],
    )
    .await;
    assert_eq!(
        answered(deezer::album_cover(&net, "Björk", "Debut").await),
        Some("https://x/3.jpg".into())
    );
    assert_eq!(replay.asked().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_beatles_fixture(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture("deezer")).await;
    let artist = answered(deezer::find_artist(&net, "The Beatles").await).unwrap();
    assert_eq!(artist.id, 1);
    assert_eq!(artist.name, "The Beatles");
    assert!(
        artist
            .picture
            .unwrap()
            .ends_with("/1000x1000-000000-80-0-0.jpg")
    );
    assert_eq!(
        answered(deezer::related(&net, artist.id).await),
        ["John Lennon", "Paul McCartney", "George Harrison", "Wings"]
    );
    assert_eq!(
        replay.asked()[0],
        "GET /search/artist?q=The%20Beatles&limit=1"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deezer_untrusted_answers(pool: PgPool) {
    let many: Vec<Value> = (0..80)
        .map(|i| json!({"id": 1000 + i, "name": format!("Artist {i}")}))
        .collect();
    let (replay, net) = net_for(
        &pool,
        vec![
            entry("/artist/7/related?limit=30", 200, json!({ "data": many })),
            entry(
                "/artist/8/related?limit=30",
                200,
                json!({"data": [null, 3, {"id": "x"}, {"name": 5}, {"name": ""}, {"name": "x".repeat(2000)}, {"name": "Kept"}]}),
            ),
            // Deezer answers its errors with a 200: never an answer, never cached.
            entry(
                &deezer_search_artist("Quota"),
                200,
                json!({"error": {"type": "Exception", "message": "Quota limit exceeded", "code": 4}}),
            ),
            entry(
                &deezer_search_artist("AC/DC & Co"),
                200,
                json!({"data": [{"id": "not a number", "name": "AC/DC & Co", "picture_xl": 7, "picture_big": "https://x/big.jpg"}]}),
            ),
        ],
    )
    .await;
    assert_eq!(answered(deezer::related(&net, 7).await).len(), 30);
    let names = answered(deezer::related(&net, 8).await);
    assert_eq!(names.len(), 2);
    assert_eq!(names[0].chars().count(), 500);
    assert_eq!(names[1], "Kept");
    assert_eq!(
        deezer::find_artist(&net, "Quota").await.code(),
        Some("BAD_RESPONSE")
    );
    // An id that is not a number: no usable artist.
    assert_eq!(
        answered(deezer::find_artist(&net, "AC/DC & Co").await),
        None
    );
    assert!(
        replay
            .asked()
            .contains(&"GET /search/artist?q=AC%2FDC%20%26%20Co&limit=1".to_string())
    );
    let rows = cache_rows(&pool).await;
    assert!(rows.iter().all(|r| !r.2.contains("Quota")), "{rows:?}");
}

// ---- ListenBrainz ----

fn lb_url(seed: &str) -> String {
    format!("/similar-artists/json?artist_mbids={seed}&algorithm={ALGORITHM}")
}

fn lb_row(m: &str, name: &str, score: Value) -> Value {
    json!({"artist_mbid": m, "name": name, "score": score, "reference_mbid": SEED})
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listenbrainz_filters_sorts_and_excludes_self(pool: PgPool) {
    let a = "00000000-0000-4000-8000-00000000000a";
    let b = "00000000-0000-4000-8000-00000000000b";
    let c = "00000000-0000-4000-8000-00000000000c";
    let (replay, net) = net_for(
        &pool,
        vec![entry(
            &lb_url(SEED),
            200,
            json!([
                lb_row(b, "Led Zeppelin", json!(1500)),
                lb_row(&SEED.to_uppercase(), "Pink Floyd", json!(9999)),
                lb_row(a, "The Beatles", json!(3000)),
                lb_row(c, "", json!(5000)),
                lb_row("not-an-mbid", "Nobody", json!(4000)),
                lb_row(c, "Zero", json!(0)),
                lb_row(c, "Negative", json!(-3)),
                lb_row(c, "Strings", json!("2000")),
                {"name": "No Mbid", "score": 100},
                null,
            ]),
        )],
    )
    .await;
    let out = answered(listenbrainz::similar(&net, mbid(SEED)).await);
    let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["The Beatles", "Strings", "Led Zeppelin"]);
    assert_eq!(out[0].mbid, Some(mbid(a)));
    assert_eq!(out[0].score, 3000.0);
    assert_eq!(out[1].score, 2000.0);
    // Port of "asks the similarity model with the artist page's algorithm".
    assert_eq!(replay.asked(), [format!("GET {}", lb_url(SEED))]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listenbrainz_empty_is_absent(pool: PgPool) {
    let other = "00000000-0000-4000-8000-000000000003";
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(&lb_url(SEED), 200, json!([])),
            entry(&lb_url(other), 503, json!({"error": "down"})),
        ],
    )
    .await;
    assert!(matches!(
        listenbrainz::similar(&net, mbid(SEED)).await,
        Outcome::Absent(_)
    ));
    let rows = cache_rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].1.as_str(), rows[0].3.as_str()),
        ("listenbrainz", "absent")
    );
    // An error is passed on, never remembered.
    assert!(matches!(
        listenbrainz::similar(&net, mbid(other)).await,
        Outcome::Transient(_)
    ));
    assert_eq!(cache_rows(&pool).await.len(), 1);
    // Only the seed itself: nothing similar either.
    let third = "00000000-0000-4000-8000-000000000004";
    let (_r, net) = net_for(
        &pool,
        vec![entry(
            &lb_url(third),
            200,
            json!([lb_row(third, "Self", json!(10))]),
        )],
    )
    .await;
    assert!(matches!(
        listenbrainz::similar(&net, mbid(third)).await,
        Outcome::Absent(_)
    ));
    drop(replay);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn listenbrainz_top_30_and_beatles_fixture(pool: PgPool) {
    let rows: Vec<Value> = (1..=100)
        .map(|i| {
            lb_row(
                &format!("00000000-0000-4000-8000-{i:012}"),
                &format!("A{i}"),
                json!(i),
            )
        })
        .collect();
    let (_r, net) = net_for(&pool, vec![entry(&lb_url(SEED), 200, Value::Array(rows))]).await;
    let out = answered(listenbrainz::similar(&net, mbid(SEED)).await);
    assert_eq!(out.len(), 30);
    assert_eq!(out[0].name, "A100");
    assert_eq!(out[29].name, "A71");

    let (_r, net) = net_for(&pool, fixture("listenbrainz")).await;
    let out = answered(listenbrainz::similar(&net, mbid(BEATLES)).await);
    let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "John Lennon",
            "Paul McCartney",
            "George Harrison",
            "The Rolling Stones",
            "The Beach Boys"
        ]
    );

    // Not a list: a bad response, not "nothing similar".
    let other = "00000000-0000-4000-8000-000000000005";
    let (_r, net) = net_for(&pool, vec![entry(&lb_url(other), 200, json!({"x": 1}))]).await;
    assert_eq!(
        listenbrainz::similar(&net, mbid(other)).await.code(),
        Some("BAD_RESPONSE")
    );
}

// ---- Cover Art Archive ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn caa_front_prefers_1200_thumbnail(pool: PgPool) {
    let (_r, net) = net_for(&pool, fixture("coverartarchive")).await;
    let url = answered(coverartarchive::front(&net, mbid(ABBEY_ROAD)).await).unwrap();
    assert!(url.ends_with("/4000000001-1200.jpg"), "{url}");

    let rg = |n: u32| format!("00000000-0000-4000-8000-{n:012}");
    let (_r, net) = net_for(
        &pool,
        vec![
            entry(
                &format!("/release-group/{}", rg(1)),
                200,
                json!({"images": [{"front": true, "image": "https://caa/full.jpg", "thumbnails": {"500": "https://caa/500.jpg"}}]}),
            ),
            entry(
                &format!("/release-group/{}", rg(2)),
                200,
                json!({"images": [{"front": false, "image": "https://caa/back.jpg", "thumbnails": {"1200": "https://caa/back-1200.jpg"}}]}),
            ),
            entry(&format!("/release-group/{}", rg(3)), 404, json!({})),
            entry(
                &format!("/release-group/{}", rg(4)),
                200,
                json!({"images": [null, {"front": "yes"}, {"front": true, "image": 5, "thumbnails": {"1200": 6}}]}),
            ),
            entry(&format!("/release-group/{}", rg(5)), 500, json!({})),
        ],
    )
    .await;
    assert_eq!(
        answered(coverartarchive::front(&net, mbid(&rg(1))).await),
        Some("https://caa/full.jpg".into())
    );
    assert_eq!(
        answered(coverartarchive::front(&net, mbid(&rg(2))).await),
        None
    );
    assert_eq!(
        answered(coverartarchive::front(&net, mbid(&rg(3))).await),
        None
    );
    assert_eq!(
        answered(coverartarchive::front(&net, mbid(&rg(4))).await),
        None
    );
    assert!(matches!(
        coverartarchive::front(&net, mbid(&rg(5))).await,
        Outcome::Transient(_)
    ));
}

// ---- fanart.tv ----

/// The fixtures carry `api_key=REDACTED`, as `tools/record-fixtures` writes it.
const FIXTURE_KEY: &str = "REDACTED";

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fanart_first_of_three_kinds_and_bare_name_prefixed(pool: PgPool) {
    let (_r, net) = net_for(&pool, fixture("fanart")).await;
    let url = answered(fanart::artist_image(&net, FIXTURE_KEY, mbid(BEATLES)).await).unwrap();
    assert!(url.contains("/artistbackground/"), "{url}");

    let m = |n: u32| format!("00000000-0000-4000-8000-{n:012}");
    let key_url = |id: &str| format!("/music/{id}?api_key=k-12345");
    let (_r, net) = net_for(
        &pool,
        vec![
            entry(
                &key_url(&m(1)),
                200,
                json!({"artistbackground": [], "artistthumb": [{"url": "https://assets.fanart.tv/thumb.jpg"}], "hdmusiclogo": [{"url": "https://assets.fanart.tv/logo.png"}]}),
            ),
            entry(&key_url(&m(2)), 200, json!({"hdmusiclogo": [{"url": "logo-1.png"}]})),
            entry(&key_url(&m(3)), 200, json!({"name": "Nobody", "albums": {}})),
            entry(
                &key_url(&m(4)),
                200,
                json!({"artistbackground": "x", "artistthumb": [null, {"url": 3}], "hdmusiclogo": [{"url": ""}]}),
            ),
        ],
    )
    .await;
    assert_eq!(
        answered(fanart::artist_image(&net, "k-12345", mbid(&m(1))).await),
        Some("https://assets.fanart.tv/thumb.jpg".into())
    );
    assert_eq!(
        answered(fanart::artist_image(&net, "k-12345", mbid(&m(2))).await),
        Some(format!(
            "https://assets.fanart.tv/fanart/music/{}/hdmusiclogo/logo-1.png",
            m(2)
        ))
    );
    assert_eq!(
        answered(fanart::artist_image(&net, "k-12345", mbid(&m(3))).await),
        None
    );
    assert_eq!(
        answered(fanart::artist_image(&net, "k-12345", mbid(&m(4))).await),
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fanart_404_is_none(pool: PgPool) {
    let (_r, net) = net_for(&pool, fixture("fanart")).await;
    let missing = mbid("00000000-0000-4000-8000-000000000404");
    assert_eq!(
        answered(fanart::artist_image(&net, FIXTURE_KEY, missing).await),
        None
    );
    // A 404 is cached as an absence, under a key without the secret.
    let rows = cache_rows(&pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3, "absent");
    // The 429 entry: rate-limited, not "none".
    let limited = mbid("00000000-0000-4000-8000-000000000429");
    assert!(matches!(
        fanart::artist_image(&net, FIXTURE_KEY, limited).await,
        Outcome::RateLimited { .. }
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fanart_album_cover_then_cdart(pool: PgPool) {
    let (_r, net) = net_for(&pool, fixture("fanart")).await;
    let url = answered(fanart::album_cover(&net, FIXTURE_KEY, mbid(ABBEY_ROAD)).await).unwrap();
    assert!(url.contains("/albumcover/"), "{url}");

    let rg = "00000000-0000-4000-8000-000000000011";
    let rg2 = "00000000-0000-4000-8000-000000000012";
    let (_r, net) = net_for(
        &pool,
        vec![
            entry(
                &format!("/music/albums/{rg}?api_key=k-12345"),
                200,
                json!({"albums": {rg: {"albumcover": [], "cdart": [{"url": "https://assets.fanart.tv/cd.png"}]}}}),
            ),
            entry(
                &format!("/music/albums/{rg2}?api_key=k-12345"),
                200,
                json!({"albums": {"00000000-0000-4000-8000-000000000099": {"albumcover": [{"url": "https://other"}]}}}),
            ),
        ],
    )
    .await;
    assert_eq!(
        answered(fanart::album_cover(&net, "k-12345", mbid(rg)).await),
        Some("https://assets.fanart.tv/cd.png".into())
    );
    assert_eq!(
        answered(fanart::album_cover(&net, "k-12345", mbid(rg2)).await),
        None
    );
}

#[test]
fn fanart_key_order() {
    let kv = |enabled: bool, key: Option<&str>| KvSettings {
        fanart_enabled: enabled,
        fanart_api_key: key.map(str::to_string),
        ..KvSettings::default()
    };
    assert_eq!(
        fanart::key(&kv(true, Some("stored")), Some("env")).as_deref(),
        Some("stored")
    );
    // The stored key counts only while fanart is enabled.
    assert_eq!(
        fanart::key(&kv(false, Some("stored")), Some("env")).as_deref(),
        Some("env")
    );
    assert_eq!(fanart::key(&kv(false, Some("stored")), None), None);
    assert_eq!(
        fanart::key(&kv(true, None), Some(" env ")).as_deref(),
        Some("env")
    );
    assert_eq!(fanart::key(&kv(true, Some("  ")), Some("  ")), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fanart_key_never_reaches_cache_failure_or_debug(pool: PgPool) {
    let secret = "s3cret/fanart+KEY";
    let encoded = "s3cret%2Ffanart%2BKEY";
    let m = "00000000-0000-4000-8000-000000000021";
    let m2 = "00000000-0000-4000-8000-000000000022";
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(
                &format!("/music/{m}?api_key={encoded}"),
                200,
                json!({"artistthumb": [{"url": "https://assets.fanart.tv/t.jpg"}]}),
            ),
            entry(
                &format!("/music/{m2}?api_key={encoded}"),
                401,
                json!({"status": "error", "error message": format!("Invalid API key {secret} {encoded}")}),
            ),
        ],
    )
    .await;
    answered(fanart::artist_image(&net, secret, mbid(m)).await);
    for (key, source, request, _, body) in cache_rows(&pool).await {
        assert_eq!(source, "fanart");
        for text in [&key, &request, &body] {
            assert!(!text.contains(secret) && !text.contains(encoded), "{text}");
            assert!(!text.contains("api_key"), "{text}");
        }
    }
    let out = fanart::artist_image(&net, secret, mbid(m2)).await;
    let shown = format!("{out:?} {out:#?}");
    assert!(
        !shown.contains(secret) && !shown.contains(encoded),
        "{shown}"
    );
    match &out {
        Outcome::Permanent(f) => {
            assert_eq!(f.code, "HTTP_401");
            assert!(f.message.contains("REDACTED"), "{}", f.message);
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(replay.asked().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fanart_test_key_results_and_no_cache(pool: PgPool) {
    let url = |k: &str| format!("/music/{BEATLES}?api_key={k}");
    let mut beatles = fixture("fanart").remove(0);
    beatles.url = url("good-key");
    let (replay, net) = net_for(
        &pool,
        vec![
            beatles,
            entry(
                &url("bad-key"),
                401,
                json!({"status": "error", "error message": "Invalid API key"}),
            ),
            entry(&url("gone-key"), 403, json!({})),
            entry(&url("slow-key"), 500, json!({})),
            entry(&url("weird-key"), 404, json!({})),
        ],
    )
    .await;
    assert_eq!(
        fanart::test_key(&net, "good-key").await,
        fanart::TestResult::Ok
    );
    assert_eq!(
        fanart::test_key(&net, "good-key").await,
        fanart::TestResult::Ok
    );
    assert_eq!(
        fanart::test_key(&net, "bad-key").await,
        fanart::TestResult::Rejected
    );
    assert_eq!(
        fanart::test_key(&net, "gone-key").await,
        fanart::TestResult::Rejected
    );
    assert_eq!(
        fanart::test_key(&net, "slow-key").await,
        fanart::TestResult::NoAnswer
    );
    assert_eq!(
        fanart::test_key(&net, "weird-key").await,
        fanart::TestResult::NoAnswer
    );
    // Two presses ask twice; nothing is cached.
    assert_eq!(replay.asked().len(), 6);
    assert!(cache_rows(&pool).await.is_empty());
}

#[test]
fn fanart_test_request_is_uncached_with_a_short_timeout() {
    let shown = fanart::test_request("abc-secret-key");
    let shown = format!("{shown:?}");
    assert!(shown.contains("no_cache: true"), "{shown}");
    assert!(shown.contains("timeout: Some(5s)"), "{shown}");
    assert!(shown.contains(&format!("/music/{BEATLES}")), "{shown}");
    assert!(!shown.contains("abc-secret-key"), "{shown}");
}

/// Review fix (Task 20): an item deleted after the SPARQL index saw it (EntityData 404) is no
/// entity, so `{}` (spec §12.2), cached as an absence like every read of that URL.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wikidata_deleted_item_is_answered_empty(pool: PgPool) {
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(&sparql_url(SEED), 200, sparql(Some("Q404"))),
            entry(&entity_url("Q404"), 404, json!({"error": "not found"})),
        ],
    )
    .await;
    assert_eq!(
        answered(wikidata::lookup(&net, mbid(SEED)).await),
        wikidata::WikiInfo::default()
    );
    let rows = cache_rows(&pool).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .any(|r| r.2.contains("EntityData") && r.3 == "absent"),
        "{rows:?}"
    );
    // Asked again: both answers come from the cache.
    answered(wikidata::lookup(&net, mbid(SEED)).await);
    assert_eq!(replay.asked().len(), 2);
}
