//! The autotagger's MusicBrainz lookups against the replay server. The fixtures in
//! `fixtures/musicbrainz/autotag.json` are hand-written (see the README); nothing here reaches
//! musicbrainz.org.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kima_core::identify::IdentifySwitch;
use kima_ident::Mbid;
use kima_ident::musicbrainz::{self, ArtistClause};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;

const R1: &str = "c0000001-0000-4000-8000-000000000001";
const R2: &str = "c0000002-0000-4000-8000-000000000001";
const R3: &str = "c0000003-0000-4000-8000-000000000001";
const RG1: &str = "d0000001-0000-4000-8000-000000000001";
const REC1: &str = "8e010100-0000-4000-8000-000000000001";
const MISSING: &str = "c0000099-0000-4000-8000-000000000001";
const VA: &str = "89ad4ac3-39f7-470e-963a-56509c546377";
const RELEASE_INC: &str =
    "inc=recordings%2Bartist-credits%2Brelease-groups%2Blabels%2Bisrcs%2Bmedia%2Bgenres&fmt=json";

fn mbid(s: &str) -> Mbid {
    Mbid::parse(s).unwrap()
}

trait Answer<T> {
    fn answer(self) -> Option<T>;
}

impl<T> Answer<T> for Outcome<T> {
    fn answer(self) -> Option<T> {
        match self {
            Outcome::Answered(v) => Some(v),
            _ => None,
        }
    }
}

fn fixture() -> Recording {
    Recording::load(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/musicbrainz/autotag.json"),
    )
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

async fn net_for(pool: &PgPool, rec: Recording) -> (Replay, Net) {
    let replay = Replay::start(rec).await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    (replay, net)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_parses_new_fields(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture()).await;
    let Outcome::Answered(r) = musicbrainz::release(&net, mbid(R1)).await else {
        panic!("expected an answer");
    };
    assert_eq!(r.title, "Fixture Album");
    assert_eq!(r.status.as_deref(), Some("Official"));
    assert_eq!(r.country.as_deref(), Some("XW"));
    assert_eq!(r.barcode.as_deref(), Some("5000000000017"));
    assert_eq!(r.disambiguation.as_deref(), Some(""));
    assert_eq!(r.media[0].format.as_deref(), Some("CD"));
    assert_eq!(r.media[0].title.as_deref(), Some(""));
    assert_eq!(r.release_group.as_ref().unwrap().id, mbid(RG1));
    assert_eq!(r.tracks().count(), 10);
    assert!(r.tracks().all(|t| t.length == Some(2000)));

    let Outcome::Answered(r2) = musicbrainz::release(&net, mbid(R2)).await else {
        panic!("expected an answer");
    };
    assert_eq!(r2.media.len(), 2);
    assert_eq!(r2.media[1].title.as_deref(), Some("Disc Two"));

    let Outcome::Answered(r3) = musicbrainz::release(&net, mbid(R3)).await else {
        panic!("expected an answer");
    };
    assert_eq!(r3.artist_credit[0].artist.id, mbid(VA));
    let t = r3.tracks().next().unwrap();
    assert_eq!(t.credit_names()[0], "Alpha Act");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_lookup_inc_and_request_count(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture()).await;
    assert!(matches!(
        musicbrainz::release(&net, mbid(R1)).await,
        Outcome::Answered(_)
    ));
    assert_eq!(replay.asked(), [format!("GET /release/{R1}?{RELEASE_INC}")]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_without_tracks_absent(pool: PgPool) {
    let url = format!("/release/{R1}?{RELEASE_INC}");
    let body = json!({"id": R1, "title": "Empty", "media": [{"position": 1, "tracks": []}]});
    let (_replay, net) = net_for(&pool, Recording(vec![entry(&url, 200, body)])).await;
    assert!(matches!(
        musicbrainz::release(&net, mbid(R1)).await,
        Outcome::Absent(_)
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_404_absent(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture()).await;
    assert!(matches!(
        musicbrainz::release(&net, mbid(MISSING)).await,
        Outcome::Absent(_)
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_404_is_permanent_not_absent(pool: PgPool) {
    let q = musicbrainz::release_query("Nope", ArtistClause::Name("Nobody"), Some(2));
    let url = "/release?query=release%3A%22Nope%22%20AND%20artist%3A%22Nobody%22%20AND%20tracks%3A2&limit=5&fmt=json";
    let (replay, net) = net_for(
        &pool,
        Recording(vec![entry(url, 404, json!({"error": "Not Found"}))]),
    )
    .await;
    assert!(matches!(
        musicbrainz::search_releases(&net, &q).await,
        Outcome::Permanent(_)
    ));
    assert_eq!(replay.asked(), [format!("GET {url}")]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_lists_ids_in_answer_order(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture()).await;
    let q = musicbrainz::release_query(
        "Fixture Album",
        ArtistClause::Name("Fixture Band"),
        Some(10),
    );
    assert_eq!(
        musicbrainz::search_releases(&net, &q).await.answer(),
        Some(vec![mbid(R1)])
    );
    let q = musicbrainz::release_query("Fixture Hits", ArtistClause::Various, Some(3));
    assert_eq!(
        musicbrainz::search_releases(&net, &q).await.answer(),
        Some(vec![mbid(R3)])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recording_releases_lists_ids(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture()).await;
    assert_eq!(
        musicbrainz::recording_releases(&net, mbid(REC1))
            .await
            .answer(),
        Some(vec![mbid(R1), mbid(R3)])
    );
    assert_eq!(
        replay.asked(),
        [format!("GET /recording/{REC1}?inc=releases&fmt=json")]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_group_carries_genres(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture()).await;
    let Outcome::Answered(g) = musicbrainz::release_group(&net, mbid(RG1)).await else {
        panic!("expected an answer");
    };
    assert_eq!(
        musicbrainz::top_genres(&g.genres, 2),
        ["rock", "indie rock"]
    );
}

/// The text is Lucene-escaped once, then percent-encoded once (never `%255C`).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_query_percent_encoded_once(pool: PgPool) {
    let q = musicbrainz::release_query(
        r#"Don't Stop "Live" (Remix)"#,
        ArtistClause::Name("AC/DC"),
        None,
    );
    let url = "/release?query=release%3A%22Don%27t%20Stop%20%5C%22Live%5C%22%22%20AND%20artist%3A%22AC%5C%2FDC%22&limit=5&fmt=json";
    let (replay, net) = net_for(
        &pool,
        Recording(vec![entry(url, 200, json!({"releases": []}))]),
    )
    .await;
    assert_eq!(
        musicbrainz::search_releases(&net, &q).await.answer(),
        Some(vec![])
    );
    assert_eq!(replay.asked(), [format!("GET {url}")]);
}
