//! The MusicBrainz client against the replay server: ports of upstream's
//! `musicbrainz.identification.test.ts` and `musicbrainz.albumCatalogue.test.ts`, plus the
//! tagged-release path and the query escaping of Review Focus 3. Nothing here reaches
//! musicbrainz.org: every answer is a hand-written or copied entry.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kima_core::identify::IdentifySwitch;
use kima_ident::Mbid;
use kima_ident::musicbrainz::{self, Credit, Genre, ReleaseSummary};
use kima_ident::text::Expected;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Evidence, Net, NetConfig, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;

const RG_MBID: &str = "48117b90-a16e-34ca-a514-19c702df1158";
const EARLIEST_RELEASE: &str = "bd3bb36e-16c8-438f-850e-dfbf4d1478f0";
/// Another official release of the same group, listed second by MusicBrainz.
const OTHER_RELEASE: &str = "d073287b-e5b0-4b9a-a9e7-46e3f6b8d1a5";

const PARAMORE: &str = "728ea90d-279b-4201-a8c4-e44ee1e4b4b6";
const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

const RG_PATH: &str =
    "/release-group/48117b90-a16e-34ca-a514-19c702df1158?inc=releases%2Bgenres&fmt=json";
const RELEASE_INC: &str = "inc=recordings%2Bisrcs%2Blabels%2Bgenres%2Bartist-credits&fmt=json";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/musicbrainz")
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

/// The copied upstream bodies: the release group and its earliest release.
fn discovery() -> (Value, Value) {
    let rec = Recording::load(fixtures().join("catalogue-discovery.json"));
    let rg = rec.0[0].body.clone();
    let release = rec.0[1].body.clone();
    (rg, release)
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

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn mbid(s: &str) -> Mbid {
    Mbid::parse(s).unwrap()
}

/// Upstream's `recording(id, title, lengthMs, artist, score)`, with real-shaped ids.
fn recording(rid: &str, title: &str, length_ms: u32, artist: &str, score: Option<u32>) -> Value {
    let artist_id = if artist == "Paramore" {
        PARAMORE
    } else {
        RADIOHEAD
    };
    json!({
        "id": rid, "title": title, "length": length_ms, "score": score,
        "artist-credit": [{"name": artist, "joinphrase": "", "artist": {"id": artist_id, "name": artist}}],
    })
}

fn expected() -> Expected {
    Expected {
        title: "Daydreaming".into(),
        artists: vec!["Paramore".into()],
        length_secs: Some(271),
        isrc: None,
    }
}

const Q1: &str =
    r#"/recording?query=recording:"Daydreaming" AND artist:"Paramore"&limit=50&fmt=json"#;
const Q2: &str = "/recording?query=Daydreaming AND artist:Paramore&limit=50&fmt=json";

/// Both name queries answer `body` (upstream's `mockResolvedValue`).
fn both_queries(status: u16, body: Value) -> Vec<Entry> {
    vec![entry(Q1, status, body.clone()), entry(Q2, status, body)]
}

fn reason(out: &Outcome<impl std::fmt::Debug>) -> Value {
    match out {
        Outcome::Absent(Evidence(e)) => e["reason"].clone(),
        other => panic!("expected absent, got {other:?}"),
    }
}

// ---- musicbrainz.identification.test.ts ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn never_sends_a_code_that_is_not_an_isrc(pool: PgPool) {
    let (replay, net) = net_for(&pool, vec![]).await;
    let out = musicbrainz::isrc_recordings(&net, "PMEDIA").await;
    assert_eq!(reason(&out), "not an ISRC");
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn treats_a_code_musicbrainz_refuses_as_no_answer(pool: PgPool) {
    let (replay, net) = net_for(
        &pool,
        vec![entry(
            "/isrc/USRC17607839?inc=artist-credits&fmt=json",
            400,
            json!({"error": "Invalid isrc."}),
        )],
    )
    .await;
    let out = musicbrainz::isrc_recordings(&net, "USRC17607839").await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn returns_every_recording_with_its_length_and_credits(pool: PgPool) {
    let body = json!({"isrc": "USRC17607839", "recordings": [
        recording(&id(1), "Decode", 262000, "Paramore", None),
        recording(&id(2), "Daydreaming", 271400, "Paramore", None),
    ]});
    let (replay, net) = net_for(
        &pool,
        vec![entry(
            "/isrc/USRC17607839?inc=artist-credits&fmt=json",
            200,
            body,
        )],
    )
    .await;
    let Outcome::Answered(recs) = musicbrainz::isrc_recordings(&net, "us-rc1-76-07839").await
    else {
        panic!("expected an answer");
    };
    assert_eq!(
        replay.asked(),
        ["GET /isrc/USRC17607839?inc=artist-credits&fmt=json"]
    );
    let got: Vec<(String, Option<u32>)> =
        recs.iter().map(|r| (r.id.to_string(), r.length)).collect();
    assert_eq!(got, [(id(1), Some(262000)), (id(2), Some(271400))]);
    assert_eq!(recs[1].candidate().credit_names, ["Paramore"]);
    assert_eq!(recs[1].candidate().length_secs, Some(271));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn takes_the_candidate_that_is_the_track_not_the_top_hit(pool: PgPool) {
    let body = json!({"recordings": [
        recording(&id(10), "Daydreaming (Live)", 271000, "Paramore", Some(100)),
        recording(&id(11), "Daydreaming", 270600, "Paramore", Some(90)),
    ]});
    let (replay, net) = net_for(&pool, both_queries(200, body)).await;
    let out = musicbrainz::identify_recording(&net, &expected()).await;
    let Outcome::Answered(rec) = out else {
        panic!("expected an answer, got {out:?}");
    };
    assert_eq!(rec.id, mbid(&id(11)));
    assert_eq!(replay.asked().len(), 1, "the first query already answered");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_absent_when_no_candidate_is_the_track(pool: PgPool) {
    let body =
        json!({"recordings": [recording(&id(20), "Daydreaming", 271000, "Radiohead", Some(100))]});
    let (replay, net) = net_for(&pool, both_queries(200, body)).await;
    let out = musicbrainz::identify_recording(&net, &expected()).await;
    assert_eq!(reason(&out), "no candidate is this track");
    assert_eq!(replay.asked().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn treats_a_refused_query_as_no_answer_so_the_next_is_asked(pool: PgPool) {
    let (replay, net) = net_for(&pool, both_queries(400, json!({"error": "bad query"}))).await;
    let out = musicbrainz::identify_recording(&net, &expected()).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert_eq!(replay.asked().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_nothing_for_a_title_all_in_brackets(pool: PgPool) {
    let (replay, net) = net_for(&pool, vec![]).await;
    let e = Expected {
        title: "(untitled)".into(),
        ..expected()
    };
    let out = musicbrainz::identify_recording(&net, &e).await;
    assert_eq!(reason(&out), "nothing to search by");
    let out = musicbrainz::search_recordings(&net, "(untitled)", "Paramore").await;
    assert_eq!(reason(&out), "nothing to search by");
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn skips_another_artists_song_or_another_versions_length(pool: PgPool) {
    let body = json!({"recordings": [
        recording(&id(30), "Daydreaming", 271000, "Radiohead", Some(100)),
        recording(&id(31), "Daydreaming", 200000, "Paramore", Some(95)),
        recording(&id(32), "Daydreaming", 270600, "Paramore", Some(90)),
    ]});
    let (_replay, net) = net_for(&pool, both_queries(200, body)).await;
    let out = musicbrainz::identify_recording(&net, &expected()).await;
    let Outcome::Answered(rec) = out else {
        panic!("expected an answer, got {out:?}");
    };
    assert_eq!(rec.id, mbid(&id(32)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn passes_over_a_recording_marked_live(pool: PgPool) {
    let mut live = recording(&id(40), "Daydreaming", 271000, "Paramore", Some(100));
    live["disambiguation"] = json!("live, 2010-06-12");
    let (_replay, net) = net_for(&pool, both_queries(200, json!({"recordings": [live]}))).await;
    let out = musicbrainz::identify_recording(&net, &expected()).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert!(musicbrainz::is_other_version("2010-06-12"));
    assert!(musicbrainz::is_other_version("Acoustic"));
    assert!(!musicbrainz::is_other_version("album version"));
    assert!(!musicbrainz::is_other_version("delivered"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn pages_past_the_first_100_release_groups(pool: PgPool) {
    let artist = id(999);
    let page = |from: u32, n: u32| -> Value {
        let groups: Vec<Value> = (from..from + n)
            .map(|i| json!({"id": id(i), "title": format!("Album {i}")}))
            .collect();
        json!({"release-group-count": 230, "release-groups": groups})
    };
    let url = |offset: u32| {
        format!(
            "/release-group?artist={artist}&type=album%7Cep%7Csingle&limit=100&offset={offset}&fmt=json"
        )
    };
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(&url(0), 200, page(0, 100)),
            entry(&url(100), 200, page(100, 100)),
            entry(&url(200), 200, page(200, 30)),
        ],
    )
    .await;
    let out = musicbrainz::all_artist_release_groups(&net, mbid(&artist)).await;
    let Outcome::Answered(groups) = out else {
        panic!("expected an answer, got {out:?}");
    };
    assert_eq!(groups.len(), 230);
    let offsets: Vec<String> = replay
        .asked()
        .iter()
        .map(|a| {
            a.split("offset=")
                .nth(1)
                .unwrap()
                .split('&')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(offsets, ["0", "100", "200"]);

    let Outcome::Answered((first, count)) =
        musicbrainz::artist_release_groups(&net, mbid(&artist), 100).await
    else {
        panic!("expected a page");
    };
    assert_eq!((first.len(), count), (100, 230));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isrcs_and_genres_come_from_one_request_then_cache(pool: PgPool) {
    let rec = id(50);
    let body = json!({
        "id": rec, "title": "Daydreaming",
        "isrcs": ["USAT21000001", "USAT21000002"],
        "genres": [{"name": "pop punk", "count": 3}, {"name": "voted down", "count": 0}],
        "tags": [{"name": "pop punk", "count": 3}],
    });
    let (replay, net) = net_for(
        &pool,
        vec![entry(
            &format!("/recording/{rec}?inc=isrcs%2Bgenres%2Btags&fmt=json"),
            200,
            body,
        )],
    )
    .await;
    for _ in 0..2 {
        let Outcome::Answered(r) = musicbrainz::recording_isrcs_and_genres(&net, mbid(&rec)).await
        else {
            panic!("expected an answer");
        };
        assert_eq!(r.isrcs.first().map(String::as_str), Some("USAT21000001"));
        assert_eq!(musicbrainz::top_genres(&r.genres, 5), ["pop punk"]);
        assert_eq!(musicbrainz::top_genres(&r.tags, 5), ["pop punk"]);
    }
    assert_eq!(
        replay.asked(),
        [format!(
            "GET /recording/{rec}?inc=isrcs%2Bgenres%2Btags&fmt=json"
        )]
    );
}

// ---- musicbrainz.albumCatalogue.test.ts ----

fn catalogue_entries(rg: Value, release: Value) -> Vec<Entry> {
    vec![
        entry(RG_PATH, 200, rg),
        entry(
            &format!("/release/{EARLIEST_RELEASE}?{RELEASE_INC}"),
            200,
            release,
        ),
    ]
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn extracts_the_earliest_release_year_label_genres_and_tracklist(pool: PgPool) {
    let (rg, release) = discovery();
    let (_replay, net) = net_for(&pool, catalogue_entries(rg, release)).await;
    let out = musicbrainz::catalogue(&net, mbid(RG_MBID), None).await;
    let Outcome::Answered(c) = out else {
        panic!("expected an answer, got {out:?}");
    };
    assert_eq!(c.release.id, mbid(EARLIEST_RELEASE));
    assert!(
        c.release_group
            .first_release_date
            .as_deref()
            .unwrap()
            .starts_with("2001")
    );
    let label = c.release.label_info[0].label.as_ref().unwrap();
    assert_eq!(label.name, "Parlophone");
    assert!(c.release_group.genres.contains(&Genre {
        name: "dance".into(),
        count: 3
    }));
    let tracks: Vec<_> = c.release.tracks().collect();
    assert_eq!(tracks.len(), 14);
    let one_more_time = tracks[0];
    assert_eq!(
        one_more_time.recording.id,
        mbid("60fa767a-d85d-4991-82bc-4294e0b11ae7")
    );
    assert_eq!(one_more_time.title, "One More Time");
    assert_eq!(one_more_time.position, 1);
    assert_eq!(one_more_time.recording.length, Some(320746));
    assert!(
        one_more_time
            .recording
            .isrcs
            .iter()
            .any(|i| i == "GBDUW0000053")
    );
    assert!(one_more_time.recording.genres.contains(&Genre {
        name: "dance".into(),
        count: 4
    }));
    assert_eq!(one_more_time.credit_names(), ["Daft Punk"]);
    let cand = one_more_time.candidate();
    assert_eq!(cand.length_secs, Some(321));
    assert_eq!(cand.title, "One More Time");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keeps_every_name_a_track_is_credited_to(pool: PgPool) {
    let (rg, mut release) = discovery();
    release["media"][0]["tracks"][0]["artist-credit"] = json!([
        {"name": "Daft Punk", "joinphrase": " feat. ", "artist": {"id": "056e4f3e-d505-4dad-8ec1-d04f521cbb56", "name": "Daft Punk", "sort-name": "Daft Punk"}},
        {"name": "Romanthony", "joinphrase": "", "artist": {"id": "1fc4cb1e-6a52-4b5a-a9b5-a0d5c5d0c0f4", "name": "Romanthony", "sort-name": "Romanthony"}},
    ]);
    let (_replay, net) = net_for(&pool, catalogue_entries(rg, release)).await;
    let Outcome::Answered(c) = musicbrainz::catalogue(&net, mbid(RG_MBID), None).await else {
        panic!("expected an answer");
    };
    let first = c.release.tracks().next().unwrap();
    assert_eq!(
        first.credit_names(),
        ["Daft Punk", "Romanthony", "Daft Punk feat. Romanthony"]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_two_requests_with_exactly_the_needed_increments(pool: PgPool) {
    let (rg, release) = discovery();
    let (replay, net) = net_for(&pool, catalogue_entries(rg, release)).await;
    let out = musicbrainz::catalogue(&net, mbid(RG_MBID), None).await;
    assert!(matches!(out, Outcome::Answered(_)), "{out:?}");
    assert_eq!(
        replay.asked(),
        [
            format!("GET {RG_PATH}"),
            format!("GET /release/{EARLIEST_RELEASE}?{RELEASE_INC}"),
        ]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn picks_the_earliest_official_release_whatever_the_order(pool: PgPool) {
    let (mut rg, release) = discovery();
    let reversed: Vec<Value> = rg["releases"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .cloned()
        .collect();
    rg["releases"] = Value::Array(reversed);
    let (replay, net) = net_for(&pool, catalogue_entries(rg, release)).await;
    let out = musicbrainz::catalogue(&net, mbid(RG_MBID), None).await;
    assert!(matches!(out, Outcome::Answered(_)), "{out:?}");
    assert_eq!(
        replay.asked()[1],
        format!("GET /release/{EARLIEST_RELEASE}?{RELEASE_INC}")
    );

    // The rule itself: official first, then the padded date, then the id.
    let r = |id: &str, status: Option<&str>, date: Option<&str>| ReleaseSummary {
        id: mbid(id),
        status: status.map(Into::into),
        date: date.map(Into::into),
    };
    let rs = [
        r(&id(1), None, Some("1999-01-01")),
        r(&id(2), Some("Official"), None),
        r(&id(3), Some("Official"), Some("2001")),
        r(&id(5), Some("Official"), Some("2001-02-26")),
        r(&id(4), Some("Official"), Some("2001-02-26")),
    ];
    assert_eq!(musicbrainz::choose_release(&rs).unwrap().id, mbid(&id(4)));
    assert_eq!(
        musicbrainz::choose_release(&rs[..3]).unwrap().id,
        mbid(&id(3))
    );
    assert_eq!(
        musicbrainz::choose_release(&rs[..2]).unwrap().id,
        mbid(&id(2))
    );
    assert!(musicbrainz::choose_release(&[]).is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_absent_for_a_made_up_release_group(pool: PgPool) {
    let made_up = "6a1f0a3e-0000-4000-8000-0000000c0ffe";
    let (_replay, net) = net_for(
        &pool,
        vec![entry(
            &format!("/release-group/{made_up}?inc=releases%2Bgenres&fmt=json"),
            404,
            json!({"error": "Not Found"}),
        )],
    )
    .await;
    let out = musicbrainz::catalogue(&net, mbid(made_up), None).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn serves_a_second_call_from_cache_without_http(pool: PgPool) {
    let (rg, release) = discovery();
    let (replay, net) = net_for(&pool, catalogue_entries(rg, release)).await;
    let first = musicbrainz::catalogue(&net, mbid(RG_MBID), None).await;
    let second = musicbrainz::catalogue(&net, mbid(RG_MBID), None).await;
    assert!(matches!(first, Outcome::Answered(_)), "{first:?}");
    assert_eq!(first, second);
    assert_eq!(replay.asked().len(), 2);
}

// ---- kima-rs additions ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tagged_release_fetched_directly(pool: PgPool) {
    let (rg, mut release) = discovery();
    release["id"] = json!(OTHER_RELEASE);
    let (replay, net) = net_for(
        &pool,
        vec![
            entry(RG_PATH, 200, rg),
            entry(
                &format!("/release/{OTHER_RELEASE}?{RELEASE_INC}"),
                200,
                release,
            ),
        ],
    )
    .await;
    let out = musicbrainz::catalogue(&net, mbid(RG_MBID), Some(mbid(OTHER_RELEASE))).await;
    let Outcome::Answered(c) = out else {
        panic!("expected an answer, got {out:?}");
    };
    assert_eq!(c.release.id, mbid(OTHER_RELEASE));
    assert_eq!(
        replay.asked(),
        [
            format!("GET {RG_PATH}"),
            format!("GET /release/{OTHER_RELEASE}?{RELEASE_INC}"),
        ]
    );
}

/// Review Focus 3: titles and names that break a Lucene query or a URL. Query 1 is sent exactly
/// Lucene-escaped, then percent-encoded (expected values computed independently), and the
/// replay server, which decodes before matching, sees one well-formed request.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_query_escapes_lucene_and_url(pool: PgPool) {
    let cases = [
        (
            r#"Don't Stop "Live" (Remix)"#,
            "AC/DC",
            "recording%3A%22Don%27t%20Stop%20%5C%22Live%5C%22%22%20AND%20artist%3A%22AC%5C%2FDC%22",
        ),
        (
            "AC/DC",
            "Guns N' Roses & Friends?",
            "recording%3A%22AC%5C%2FDC%22%20AND%20artist%3A%22Guns%20N%27%20Roses%20%5C%26%20Friends%5C%3F%22",
        ),
        (
            "Guns N' Roses & Friends?",
            "#1 Crush",
            "recording%3A%22Guns%20N%27%20Roses%20%5C%26%20Friends%5C%3F%22%20AND%20artist%3A%22%231%20Crush%22",
        ),
        (
            "#1 Crush",
            "東京事変",
            "recording%3A%22%231%20Crush%22%20AND%20artist%3A%22%E6%9D%B1%E4%BA%AC%E4%BA%8B%E5%A4%89%22",
        ),
        (
            "東京事変",
            r#"Don't Stop "Live" (Remix)"#,
            "recording%3A%22%E6%9D%B1%E4%BA%AC%E4%BA%8B%E5%A4%89%22%20AND%20artist%3A%22Don%27t%20Stop%20%5C%22Live%5C%22%22",
        ),
    ];
    for (i, (title, artist, query)) in cases.iter().enumerate() {
        let hit = json!({"recordings": [recording(&id(60 + i as u32), title, 200000, "Paramore", Some(100))]});
        let url = format!("/recording?query={query}&limit=50&fmt=json");
        let (replay, net) = net_for(&pool, vec![entry(&url, 200, hit)]).await;
        let out = musicbrainz::search_recordings(&net, title, artist).await;
        let Outcome::Answered(recs) = out else {
            panic!("{title} / {artist}: expected an answer, got {out:?}");
        };
        assert_eq!(recs.len(), 1);
        assert_eq!(replay.asked(), [format!("GET {url}")], "{title} / {artist}");
    }
}

// ---- helpers and untrusted JSON ----

#[test]
fn credit_names_are_each_name_then_the_line_without_repeats() {
    let credits: Vec<Credit> = serde_json::from_value(json!([
        {"name": "Hall", "joinphrase": " & ", "artist": {"id": PARAMORE, "name": "Daryl Hall", "sort-name": "Hall, Daryl"}},
        {"name": "Oates", "joinphrase": "", "artist": {"id": RADIOHEAD, "name": "John Oates", "sort-name": "Oates, John"}},
    ]))
    .unwrap();
    assert_eq!(
        musicbrainz::credit_names(&credits),
        [
            "Hall",
            "Daryl Hall",
            "Hall, Daryl",
            "Oates",
            "John Oates",
            "Oates, John",
            "Hall & Oates"
        ]
    );
}

#[test]
fn top_genres_by_count_then_name() {
    let g = |name: &str, count| Genre {
        name: name.into(),
        count,
    };
    let gs = [
        g("rock", 2),
        g("pop", 5),
        g("indie", 2),
        g("zero", 0),
        g("jazz", 1),
    ];
    assert_eq!(musicbrainz::top_genres(&gs, 3), ["pop", "indie", "rock"]);
    assert_eq!(
        musicbrainz::top_genres(&gs, 10),
        ["pop", "indie", "rock", "jazz"]
    );
}

#[test]
fn length_ms_rounds_to_the_nearest_second() {
    let rec = |length: Value| -> musicbrainz::Recording {
        serde_json::from_value(json!({"id": PARAMORE, "title": "t", "length": length})).unwrap()
    };
    assert_eq!(rec(json!(271499)).candidate().length_secs, Some(271));
    assert_eq!(rec(json!(271500)).candidate().length_secs, Some(272));
    assert_eq!(rec(json!(499)).candidate().length_secs, Some(0));
    assert_eq!(
        rec(json!(u32::MAX)).candidate().length_secs,
        Some(4_294_967)
    );
    assert_eq!(rec(Value::Null).candidate().length_secs, None);
}

#[test]
fn untrusted_json_never_panics_and_keeps_what_parses() {
    // Missing fields, nulls, wrong types, a huge length and a bad id inside a list.
    let rec: musicbrainz::Recording = serde_json::from_value(json!({
        "id": PARAMORE, "title": null, "length": 99999999999u64, "score": "87",
        "artist-credit": [
            {"name": "ok", "artist": {"id": RADIOHEAD, "name": "Ok"}},
            {"name": "bad", "artist": {"id": "{not-an-id}", "name": "Bad"}},
            "junk",
        ],
        "isrcs": null, "genres": [{"name": "rock"}, {"count": 3}], "unknown": {"x": 1},
    }))
    .unwrap();
    assert_eq!(rec.title, "");
    assert_eq!(rec.length, Some(u32::MAX));
    assert_eq!(rec.score, Some(87));
    assert_eq!(rec.artist_credit.len(), 1);
    assert!(rec.isrcs.is_empty());
    assert_eq!(
        rec.genres,
        [Genre {
            name: "rock".into(),
            count: 0
        }]
    );
    let _ = rec.candidate();
    assert!(serde_json::from_value::<musicbrainz::Recording>(json!({"title": "no id"})).is_err());
}
