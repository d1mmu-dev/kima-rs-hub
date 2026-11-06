//! The identification ladder (spec §8.3, §8.5) against the replay server: ports of upstream's
//! `acoustid-fingerprint.test.ts`, `unifiedEnrichment.acoustidOrder.test.ts` and the applicable
//! `acoustidIdentification.test.ts` cases. Nothing here reaches MusicBrainz or AcoustID: every
//! answer is a hand-written entry, and a request nobody recorded is answered 599 (a failure).

mod common;

use std::collections::BTreeMap;

use common::ident_ctx;
use kima_db::identity::{IsrcWrite, LadderTrack};
use kima_ident::matching::{
    self, AcoustidGate, Answer, LadderResult, Rung, SkipReason, acoustid_gate,
};
use kima_ident::stages::StageFailure;
use kima_ident::{IdentCtx, Mbid};
use kima_net::Bases;
use kima_net::testing::{Entry, Recording, Replay};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RG: &str = "48117b90-a16e-34ca-a514-19c702df1158";
const REL: &str = "d073287b-d1bd-4f11-a933-a4386f8cf701";
const TAGGED_REL: &str = "bd3bb36e-16c8-438f-850e-dfbf4d1478f0";
const ISRC: &str = "USRC17607839";
const KEY: &str = "test-key";
const FINGERPRINT: &str = "AQAA0pMiRZkQ";

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn mbid(s: &str) -> Mbid {
    Mbid::parse(s).unwrap()
}

fn get(url: &str, status: u16, body: Value) -> Entry {
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

fn lookup(body: Value) -> Entry {
    Entry {
        method: "POST".into(),
        url: "/lookup".into(),
        status: 200,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn rg_url() -> String {
    format!("/release-group/{RG}?inc=releases+genres&fmt=json")
}

fn release_url(release: &str) -> String {
    format!("/release/{release}?inc=recordings+isrcs+labels+genres+artist-credits&fmt=json")
}

fn recording_url(rec: &str) -> String {
    format!("/recording/{rec}?inc=isrcs+genres+tags&fmt=json")
}

fn isrc_url(code: &str) -> String {
    format!("/isrc/{code}?inc=artist-credits&fmt=json")
}

const Q1: &str = r#"/recording?query=recording:"Cheerleader Effect" AND artist:"Carpenter Brut"&limit=50&fmt=json"#;
const Q2: &str = "/recording?query=Cheerleader Effect AND artist:Carpenter Brut&limit=50&fmt=json";

fn credit(name: &str) -> Value {
    json!([{"name": name, "joinphrase": "", "artist": {"id": id(9000), "name": name}}])
}

fn recording(rec: &str, title: &str, length_ms: u32, artist: &str) -> Value {
    json!({"id": rec, "title": title, "length": length_ms, "artist-credit": credit(artist)})
}

/// A tracklist row: its recording with ISRCs and curated genres.
fn row(rec: &str, title: &str, length_ms: u32, artist: &str, isrcs: &[&str]) -> Value {
    json!({
        "id": id(7000), "position": 1, "title": title, "length": length_ms,
        "recording": {
            "id": rec, "title": title, "length": length_ms, "isrcs": isrcs,
            "genres": [{"name": "synthwave", "count": 2}],
            "artist-credit": credit(artist),
        },
    })
}

fn release_group() -> Value {
    json!({
        "id": RG, "title": "Leather Teeth", "first-release-date": "2018-02-23",
        "releases": [{"id": REL, "status": "Official", "date": "2018-02-23"}],
        "genres": [{"name": "synthwave", "count": 5}, {"name": "electronic", "count": 2}],
    })
}

fn release(id: &str, rows: Vec<Value>) -> Value {
    json!({
        "id": id, "title": "Leather Teeth", "date": "2018-02-23",
        "label-info": [{"catalog-number": "NQ1", "label": {"name": "No Quarter"}}],
        "media": [{"position": 1, "track-count": rows.len(), "tracks": rows}],
    })
}

/// The album's catalogue, both requests.
fn catalogue(rows: Vec<Value>) -> Vec<Entry> {
    vec![
        get(&rg_url(), 200, release_group()),
        get(&release_url(REL), 200, release(REL, rows)),
    ]
}

/// `/recording/{id}?inc=isrcs+genres+tags` for `rec`.
fn details(rec: &str, isrcs: &[&str]) -> Entry {
    get(
        &recording_url(rec),
        200,
        json!({
            "id": rec, "title": "Cheerleader Effect", "length": 218_000, "isrcs": isrcs,
            "genres": [{"name": "synthwave", "count": 3}, {"name": "outrun", "count": 1}],
            "tags": [{"name": "seen live", "count": 9}],
        }),
    )
}

fn no_hits() -> Vec<Entry> {
    vec![
        get(Q1, 200, json!({"recordings": []})),
        get(Q2, 200, json!({"recordings": []})),
    ]
}

/// What the track says about itself: Carpenter Brut, "Cheerleader Effect", 218 s.
fn track() -> LadderTrack {
    LadderTrack {
        id: Uuid::new_v4(),
        title: "Cheerleader Effect".into(),
        artist_name: "Carpenter Brut".into(),
        duration_secs: 218,
        isrc: None,
        isrc_source: None,
        tag_recording_mbid: None,
        album_id: Uuid::new_v4(),
        album_title: "Leather Teeth".into(),
        album_artist: "Carpenter Brut".into(),
        release_mbid: None,
        release_group_mbid: None,
        album_label: None,
        album_has_mb_genres: false,
    }
}

fn with_rg(mut t: LadderTrack) -> LadderTrack {
    t.release_group_mbid = Some(Uuid::parse_str(RG).unwrap());
    t
}

fn ask() -> AcoustidGate {
    AcoustidGate::Ask {
        key: KEY.into(),
        fingerprint: FINGERPRINT.into(),
        duration_secs: 218,
    }
}

fn no_key() -> AcoustidGate {
    AcoustidGate::Skip(SkipReason::NoKey)
}

struct Fx {
    replay: Replay,
    ctx: IdentCtx,
    _dir: tempfile::TempDir,
}

async fn fx(pool: &PgPool, entries: Vec<Entry>) -> Fx {
    let replay = Replay::start(Recording(entries)).await;
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(pool, dir.path(), Bases::all(&replay.url()));
    Fx {
        replay,
        ctx,
        _dir: dir,
    }
}

impl Fx {
    /// Every request as `METHOD /path?decoded query`.
    fn asked(&self) -> Vec<String> {
        self.replay
            .asked()
            .iter()
            .map(|a| percent_decode_str(a).decode_utf8_lossy().into_owned())
            .collect()
    }
}

fn matched(r: LadderResult) -> Answer {
    match r {
        LadderResult::Matched(a) => a,
        other => panic!("expected a match, got {other:?}"),
    }
}

// ---- rung 1: the file's recording id ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_tag_rung_decides_without_identification_requests(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, vec![details(&rec, &["GBAYE0601498"])]).await;
    let mut t = track();
    t.tag_recording_mbid = Some(Uuid::parse_str(&rec).unwrap());

    let a = matched(matching::identify(&f.ctx, &t, ask()).await);

    assert_eq!(a.rung, Rung::FileTag);
    assert_eq!(a.recording, mbid(&rec));
    // The only request collects the recording's ISRCs and genres: no search, no AcoustID.
    assert_eq!(
        f.asked(),
        vec![format!(
            "GET /recording/{rec}?inc=isrcs+genres+tags&fmt=json"
        )]
    );
    assert_eq!(a.isrc, IsrcWrite::Set("GBAYE0601498".into()));
    assert_eq!(a.track_genres, vec!["synthwave", "outrun"]);
    assert_eq!(a.album_label, None);
    assert!(a.album_genres.is_empty());
    assert_eq!(a.evidence["recordingMbid"], json!(rec));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_tag_with_release_group_reads_the_catalogue_entry(pool: PgPool) {
    let rec = id(1);
    let f = fx(
        &pool,
        catalogue(vec![
            row(&id(2), "Decode", 200_000, "Carpenter Brut", &[]),
            row(
                &rec,
                "Cheerleader Effect",
                218_400,
                "Carpenter Brut",
                &["FRX451500123"],
            ),
        ]),
    )
    .await;
    let mut t = with_rg(track());
    t.tag_recording_mbid = Some(Uuid::parse_str(&rec).unwrap());

    let a = matched(matching::identify(&f.ctx, &t, ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::FileTag, mbid(&rec)));
    // ISRC and genres from its row in the tracklist: no per-recording request.
    assert_eq!(f.asked().len(), 2);
    assert!(f.asked().iter().all(|r| !r.contains("/recording")));
    assert_eq!(a.isrc, IsrcWrite::Set("FRX451500123".into()));
    assert_eq!(a.track_genres, vec!["synthwave"]);
    assert_eq!(a.album_label.as_deref(), Some("No Quarter"));
    assert_eq!(a.album_genres, vec!["synthwave", "electronic"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_tag_still_answers_when_the_catalogue_cannot_be_fetched(pool: PgPool) {
    let rec = id(1);
    let f = fx(
        &pool,
        vec![get(&rg_url(), 500, json!({})), details(&rec, &[])],
    )
    .await;
    let mut t = with_rg(track());
    t.tag_recording_mbid = Some(Uuid::parse_str(&rec).unwrap());

    let a = matched(matching::identify(&f.ctx, &t, ask()).await);

    assert_eq!(a.rung, Rung::FileTag);
    assert!(
        f.asked()
            .iter()
            .any(|r| r.contains(&format!("/recording/{rec}")))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_tag_read_before_the_isrc(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, vec![details(&rec, &[])]).await;
    let mut t = track();
    t.tag_recording_mbid = Some(Uuid::parse_str(&rec).unwrap());
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());

    let a = matched(matching::identify(&f.ctx, &t, ask()).await);

    assert_eq!(a.rung, Rung::FileTag);
    assert!(f.asked().iter().all(|r| !r.contains("/isrc/")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tagged_release_is_fetched_directly(pool: PgPool) {
    // D-117: a tagged release stands for the group; chooseRelease is not consulted.
    let rec = id(1);
    let f = fx(
        &pool,
        vec![
            get(&rg_url(), 200, release_group()),
            get(
                &release_url(TAGGED_REL),
                200,
                release(
                    TAGGED_REL,
                    vec![row(
                        &rec,
                        "Cheerleader Effect",
                        218_000,
                        "Carpenter Brut",
                        &[],
                    )],
                ),
            ),
        ],
    )
    .await;
    let mut t = with_rg(track());
    t.release_mbid = Some(Uuid::parse_str(TAGGED_REL).unwrap());

    let a = matched(matching::identify(&f.ctx, &t, no_key()).await);

    assert_eq!((a.rung, a.recording), (Rung::Tracklist, mbid(&rec)));
    assert!(f.asked().iter().any(|r| r.contains(TAGGED_REL)));
    assert!(f.asked().iter().all(|r| !r.contains(REL)));
}

// ---- rung 2: the album's tracklist ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tracklist_rung_by_isrc_then_pick_then_vouched(pool: PgPool) {
    // By ISRC: the row carrying the file's ISRC, without looking the ISRC up.
    let f = fx(
        &pool,
        catalogue(vec![
            row(
                "00000000-0000-4000-8000-0000000000aa",
                "Decode",
                218_000,
                "Carpenter Brut",
                &["USRC17600001"],
            ),
            row(
                &id(2),
                "Cheerleader Effect",
                218_400,
                "Carpenter Brut",
                &[ISRC],
            ),
        ]),
    )
    .await;
    let mut t = with_rg(track());
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());
    let a = matched(matching::identify(&f.ctx, &t, no_key()).await);
    assert_eq!((a.rung, a.recording), (Rung::Tracklist, mbid(&id(2))));
    assert!(f.asked().iter().all(|r| !r.contains("/isrc/")));
    // The file's ISRC is kept.
    assert_eq!(a.isrc, IsrcWrite::Keep);

    // A compilation: the row credited to the track's artist wins, in either order.
    for reversed in [false, true] {
        let mut rows = vec![
            row(&id(3), "Cheerleader Effect", 217_000, "Other Band", &[]),
            row(&id(4), "Cheerleader Effect", 219_500, "Carpenter Brut", &[]),
        ];
        if reversed {
            rows.reverse();
        }
        let f = fx(&pool, catalogue(rows)).await;
        let a = matched(matching::identify(&f.ctx, &with_rg(track()), no_key()).await);
        assert_eq!((a.rung, a.recording), (Rung::Tracklist, mbid(&id(4))));
    }

    // Vouched: a credit written another way, one row alone is the track.
    let f = fx(
        &pool,
        catalogue(vec![
            row(
                &id(5),
                "Cheerleader Effect",
                218_400,
                "カーペンター・ブルート",
                &[],
            ),
            row(&id(6), "Decode", 218_000, "カーペンター・ブルート", &[]),
        ]),
    )
    .await;
    let a = matched(matching::identify(&f.ctx, &with_rg(track()), no_key()).await);
    assert_eq!((a.rung, a.recording), (Rung::Tracklist, mbid(&id(5))));
    assert_eq!(a.album_label.as_deref(), Some("No Quarter"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn vouched_pass_needs_exactly_one_row(pool: PgPool) {
    let mut entries = catalogue(vec![
        row(&id(3), "Cheerleader Effect", 217_000, "Other Band", &[]),
        row(&id(4), "Cheerleader Effect", 219_500, "Third Band", &[]),
    ]);
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;

    let r = matching::identify(&f.ctx, &with_rg(track()), no_key()).await;

    match r {
        LadderResult::NoMatch { asked, skipped } => {
            assert_eq!(asked, vec!["tracklist", "search"]);
            assert_eq!(skipped, Some(SkipReason::NoKey));
        }
        other => panic!("expected no match, got {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tracklist_row_of_another_length_is_not_taken(pool: PgPool) {
    let mut entries = catalogue(vec![row(
        &id(3),
        "Cheerleader Effect",
        160_000,
        "Carpenter Brut",
        &[],
    )]);
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;

    let r = matching::identify(&f.ctx, &with_rg(track()), no_key()).await;

    assert!(matches!(r, LadderResult::NoMatch { .. }), "{r:?}");
    assert!(f.asked().iter().any(|r| r.contains("/recording?query")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isrc_row_of_another_recording_falls_to_the_isrc_lookup(pool: PgPool) {
    let mut entries = catalogue(vec![row(
        &id(3),
        "Decode",
        300_000,
        "Carpenter Brut",
        &[ISRC],
    )]);
    entries.push(get(&isrc_url(ISRC), 404, json!({})));
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;
    let mut t = with_rg(track());
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());

    let r = matching::identify(&f.ctx, &t, no_key()).await;

    match r {
        LadderResult::NoMatch { asked, .. } => {
            assert_eq!(asked, vec!["tracklist", "isrc", "search"])
        }
        other => panic!("expected no match, got {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_catalogue_defers_without_falling_through(pool: PgPool) {
    let mut entries = vec![get(&rg_url(), 500, json!({"error": "boom"}))];
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;

    let r = matching::identify(&f.ctx, &with_rg(track()), no_key()).await;

    match r {
        LadderResult::Fail(StageFailure::Transient(failure)) => {
            assert_eq!(failure.code, "HTTP_500")
        }
        other => panic!("expected a transient failure, got {other:?}"),
    }
    assert!(f.asked().iter().all(|r| r.contains("/release-group/")));
}

// ---- rung 3: the ISRC ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isrc_rung_before_search(pool: PgPool) {
    let studio = id(3);
    let f = fx(
        &pool,
        vec![
            get(
                &isrc_url(ISRC),
                200,
                json!({"recordings": [
                    recording(&id(1), "Cheerleader Effect (Live 2014)", 240_000, "Carpenter Brut"),
                    recording(&id(2), "Decode", 218_000, "Carpenter Brut"),
                    recording(&studio, "Cheerleader Effect", 218_400, "Carpenter Brut"),
                ]}),
            ),
            details(&studio, &["FRX451500123"]),
        ],
    )
    .await;
    let mut t = track();
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());

    let a = matched(matching::identify(&f.ctx, &t, no_key()).await);

    assert_eq!((a.rung, a.recording), (Rung::Isrc, mbid(&studio)));
    assert!(f.asked().iter().all(|r| !r.contains("query=")));
    assert_eq!(a.track_genres, vec!["synthwave", "outrun"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isrc_whose_recordings_are_other_songs_goes_on_down(pool: PgPool) {
    let mut entries = vec![get(
        &isrc_url(ISRC),
        200,
        json!({"recordings": [recording(&id(2), "Decode", 218_000, "Paramore")]}),
    )];
    entries.extend(no_hits());
    entries.push(lookup(json!({"status": "ok", "results": []})));
    let f = fx(&pool, entries).await;
    let mut t = track();
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());

    let r = matching::identify(&f.ctx, &t, ask()).await;

    match r {
        LadderResult::NoMatch { asked, skipped } => {
            assert_eq!(asked, vec!["acoustid", "isrc", "search"]);
            assert_eq!(skipped, None);
        }
        other => panic!("expected no match, got {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_code_that_is_not_an_isrc_is_never_sent(pool: PgPool) {
    let f = fx(&pool, no_hits()).await;
    let mut t = track();
    t.isrc = Some("PMEDIA".into());

    let r = matching::identify(&f.ctx, &t, no_key()).await;

    assert!(matches!(r, LadderResult::NoMatch { .. }), "{r:?}");
    assert!(f.asked().iter().all(|r| !r.contains("/isrc/")));
}

// ---- rung 4: the name search ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_rung(pool: PgPool) {
    let rec = id(8);
    let f = fx(
        &pool,
        vec![
            get(
                Q1,
                200,
                json!({"recordings": [
                    {"id": rec, "title": "Cheerleader Effect", "length": 218_000, "score": 100,
                     "artist-credit": credit("Carpenter Brut")},
                ]}),
            ),
            details(&rec, &[]),
        ],
    )
    .await;

    let a = matched(matching::identify(&f.ctx, &track(), no_key()).await);

    assert_eq!((a.rung, a.recording), (Rung::Search, mbid(&rec)));
    // With no key nothing is asked of AcoustID.
    assert!(f.asked().iter().all(|r| !r.contains("/lookup")));
    assert_eq!(a.evidence["asked"], json!(["search"]));
}

// ---- rung 5: AcoustID ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn acoustid_rung_answers_before_any_other_request(pool: PgPool) {
    let rec = id(9);
    let entries = vec![
        lookup(json!({"status": "ok", "results": [
            {"id": "a1b2c3d4-0000-4000-8000-0000000000ff", "score": 0.93,
             "recordings": [{"id": rec, "title": "Cheerleader Effect", "duration": 218}]},
        ]})),
        details(&rec, &["FRX451500123"]),
    ];
    let f = fx(&pool, entries).await;

    let a = matched(matching::identify(&f.ctx, &track(), ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::AcoustIdTitle, mbid(&rec)));
    let asked = f.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], "POST /lookup");
    assert!(asked[1].contains(&format!("/recording/{rec}")));
    assert_eq!(a.evidence["score"], json!(0.93));
    assert_eq!(a.evidence["fingerprintFrom"], json!("stored"));
    assert_eq!(a.evidence["asked"], json!(["acoustid"]));
    assert_eq!(a.rung.as_str(), "acoustid");
    // The key never reaches the evidence.
    assert!(!a.evidence.to_string().contains(KEY));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn acoustid_id_tag_is_never_a_recording_id(pool: PgPool) {
    // An AcoustID id is UUID-shaped; it names AcoustID's own cluster, never a recording.
    let acoustid_id = "a1b2c3d4-0000-4000-8000-0000000000ff";
    let rec = id(9);
    let mut entries = no_hits();
    entries.push(lookup(json!({"status": "ok", "results": [
        {"id": acoustid_id, "score": 0.99},
        {"id": "a1b2c3d4-0000-4000-8000-0000000000fe", "score": 0.9,
         "recordings": [{"id": rec, "title": "Cheerleader Effect", "duration": 218}]},
    ]})));
    entries.push(details(&rec, &[]));
    let f = fx(&pool, entries).await;

    let a = matched(matching::identify(&f.ctx, &track(), ask()).await);

    assert_eq!(a.recording, mbid(&rec));
    assert_ne!(a.recording, mbid(acoustid_id));
    assert_eq!(
        a.evidence["acoustidId"],
        json!("a1b2c3d4-0000-4000-8000-0000000000fe")
    );
}

// ---- ISRC writes (spec §8.5) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mb_isrc_replaces_only_null_or_musicbrainz(pool: PgPool) {
    let with = id(10);
    let without = id(11);
    let f = fx(
        &pool,
        vec![details(&with, &["frx451500123"]), details(&without, &[])],
    )
    .await;
    let case = |rec: &str, isrc: Option<&str>, source: Option<&str>| {
        let mut t = track();
        t.tag_recording_mbid = Some(Uuid::parse_str(rec).unwrap());
        t.isrc = isrc.map(str::to_string);
        t.isrc_source = source.map(str::to_string);
        t
    };
    let isrc_of = |t: LadderTrack| {
        let ctx = f.ctx.clone();
        async move { matched(matching::identify(&ctx, &t, no_key()).await).isrc }
    };

    // None stored: MusicBrainz's first ISRC, normalised.
    assert_eq!(
        isrc_of(case(&with, None, None)).await,
        IsrcWrite::Set("FRX451500123".into())
    );
    // An earlier MusicBrainz answer is replaced.
    assert_eq!(
        isrc_of(case(&with, Some("GBAYE0601498"), Some("musicbrainz"))).await,
        IsrcWrite::Set("FRX451500123".into())
    );
    // A recording with none clears an earlier MusicBrainz one.
    assert_eq!(
        isrc_of(case(&without, Some("GBAYE0601498"), Some("musicbrainz"))).await,
        IsrcWrite::Clear
    );
    // The file's own is never touched.
    assert_eq!(
        isrc_of(case(&with, Some("GBAYE0601498"), Some("id3"))).await,
        IsrcWrite::Keep
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isrc_rung_keeps_the_file_isrc(pool: PgPool) {
    let studio = id(3);
    let f = fx(
        &pool,
        vec![
            get(
                &isrc_url(ISRC),
                200,
                json!({"recordings": [recording(&studio, "Cheerleader Effect", 218_400, "Carpenter Brut")]}),
            ),
            details(&studio, &["FRX451500123"]),
        ],
    )
    .await;
    // Even an ISRC MusicBrainz gave earlier stays: the answer came from it.
    let mut t = track();
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("musicbrainz".into());

    let a = matched(matching::identify(&f.ctx, &t, no_key()).await);

    assert_eq!(a.rung, Rung::Isrc);
    assert_eq!(a.isrc, IsrcWrite::Keep);
}

// ---- the AcoustID gate ----

/// A stored track that is "Cheerleader Effect" by Carpenter Brut, 218 s.
async fn seed_track(pool: &PgPool) -> Uuid {
    let t = common::track(pool, &format!("{}.flac", Uuid::new_v4()), 218).await;
    sqlx::query(
        "UPDATE tracks SET title = 'Cheerleader Effect', artist_name = 'Carpenter Brut' \
         WHERE id = $1",
    )
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE artists SET name = 'Carpenter Brut' WHERE id = \
         (SELECT a.artist_id FROM albums a JOIN tracks t ON t.album_id = a.id WHERE t.id = $1)",
    )
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    t
}

async fn fingerprint_stage(pool: &PgPool, t: Uuid, status: &str) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'fingerprint', $2)",
    )
    .bind(t)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

async fn store_fp(pool: &PgPool, t: Uuid) {
    let mut conn = pool.acquire().await.unwrap();
    kima_db::identity::store_fingerprint(&mut conn, t, FINGERPRINT, 218)
        .await
        .unwrap();
}

fn keyed(mut ctx: IdentCtx) -> IdentCtx {
    ctx.config.acoustid_env_key = Some(KEY.into());
    ctx
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn acoustid_skipped_without_key_has_evidence(pool: PgPool) {
    let f = fx(&pool, no_hits()).await;
    let t = seed_track(&pool).await;
    fingerprint_stage(&pool, t, "done").await;
    store_fp(&pool, t).await;

    // No setting, no env key, empty default: no key.
    let gate = acoustid_gate(&f.ctx, t).await.unwrap();
    assert!(
        matches!(gate, AcoustidGate::Skip(SkipReason::NoKey)),
        "{gate:?}"
    );

    // The stage closes `done` with the skip in its evidence.
    let report = kima_ident::stages::recording::run_batch(&f.ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((report.claimed, report.done), (1, 1));
    let ev: Value = sqlx::query_scalar(
        "SELECT evidence FROM track_stages WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        ev,
        json!({"reason": "no match", "asked": ["search"], "acoustid": "skipped",
               "acoustidReason": "no_key"})
    );

    // With a key it asks, and the key is never printed.
    let gate = acoustid_gate(&keyed(f.ctx.clone()), t).await.unwrap();
    match &gate {
        AcoustidGate::Ask {
            key,
            fingerprint,
            duration_secs,
        } => {
            assert_eq!(
                (key.as_str(), fingerprint.as_str(), *duration_secs),
                (KEY, FINGERPRINT, 218)
            )
        }
        other => panic!("expected ask, got {other:?}"),
    }
    assert!(!format!("{gate:?}").contains(KEY));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn acoustid_skipped_when_fingerprint_terminal(pool: PgPool) {
    let f = fx(&pool, vec![]).await;
    let ctx = keyed(f.ctx.clone());
    let t = seed_track(&pool).await;
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, error_class) \
         VALUES ($1, 'fingerprint', 'failed', 'PERMANENT')",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();

    let gate = acoustid_gate(&ctx, t).await.unwrap();
    assert!(
        matches!(gate, AcoustidGate::Skip(SkipReason::NoFingerprint)),
        "{gate:?}"
    );

    let mut unavailable = ctx.clone();
    unavailable.config.fingerprinting = false;
    let gate = acoustid_gate(&unavailable, t).await.unwrap();
    assert!(
        matches!(
            gate,
            AcoustidGate::Skip(SkipReason::FingerprintingUnavailable)
        ),
        "{gate:?}"
    );

    let mut disabled = ctx.clone();
    disabled.config.acoustid_disabled = true;
    store_fp(&pool, t).await;
    let gate = acoustid_gate(&disabled, t).await.unwrap();
    assert!(
        matches!(gate, AcoustidGate::Skip(SkipReason::Disabled)),
        "{gate:?}"
    );

    assert_eq!(
        [
            SkipReason::NoKey,
            SkipReason::NoFingerprint,
            SkipReason::FingerprintingUnavailable,
            SkipReason::Disabled
        ]
        .map(|r| (r.as_str(), r.code())),
        [
            ("no AcoustID key", "no_key"),
            ("no fingerprint", "no_fingerprint"),
            ("fingerprinting unavailable", "fingerprinting_unavailable"),
            ("acoustid disabled", "disabled"),
        ]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_question_waits_for_the_pause_to_end_and_uses_up_no_retry(pool: PgPool) {
    let f = fx(&pool, no_hits()).await;
    let ctx = keyed(f.ctx.clone());
    let t = seed_track(&pool).await;
    fingerprint_stage(&pool, t, "done").await;
    store_fp(&pool, t).await;
    let until: time::OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO source_pauses (source, paused_until, reason) \
         VALUES ('acoustid', date_trunc('second', now()) + interval '30 minutes', 'HTTP 429') \
         RETURNING paused_until",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    ctx.net.load_pauses().await.unwrap();

    let gate = acoustid_gate(&ctx, t).await.unwrap();
    assert!(matches!(gate, AcoustidGate::Paused { .. }), "{gate:?}");

    let report = kima_ident::stages::recording::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.claimed, 1);
    let row = sqlx::query(
        "SELECT status, attempts, error_code, next_attempt_at FROM track_stages \
         WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "pending");
    assert_eq!(row.get::<i16, _>("attempts"), 0);
    assert_eq!(row.get::<String, _>("error_code"), "RATE_LIMITED");
    assert_eq!(row.get::<time::OffsetDateTime, _>("next_attempt_at"), until);
    // Not a request of any kind: no lower source names the recording in AcoustID's place.
    assert!(f.asked().is_empty(), "{:?}", f.asked());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_rung_answers_closes_done_no_match(pool: PgPool) {
    let mut entries = no_hits();
    // The recording stage asks its songs ahead in one batch; the ladder reads the saved reply.
    entries.push(lookup(
        json!({"status": "ok", "fingerprints": [{"index": 0, "results": []}]}),
    ));
    let f = fx(&pool, entries).await;
    let ctx = keyed(f.ctx.clone());
    let t = seed_track(&pool).await;
    fingerprint_stage(&pool, t, "done").await;
    store_fp(&pool, t).await;

    let report = kima_ident::stages::recording::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((report.claimed, report.done, report.failed), (1, 1, 0));

    let row = sqlx::query(
        "SELECT s.status, s.attempts, s.source, s.evidence, s.error_class, s.next_attempt_at, \
                t.recording_mbid \
         FROM track_stages s JOIN tracks t ON t.id = s.track_id \
         WHERE s.track_id = $1 AND s.stage = 'recording'",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "done");
    assert_eq!(row.get::<i16, _>("attempts"), 0);
    assert_eq!(row.get::<Option<String>, _>("error_class"), None);
    assert_eq!(
        row.get::<Option<time::OffsetDateTime>, _>("next_attempt_at"),
        None
    );
    assert_eq!(
        row.get::<Value, _>("evidence"),
        json!({"reason": "no match", "asked": ["acoustid", "search"]})
    );
    assert_eq!(row.get::<Option<Uuid>, _>("recording_mbid"), None);
    // Exactly the one batch lookup and the two searches.
    assert_eq!(f.asked().len(), 3);
    assert_eq!(f.asked()[0], "POST /lookup");
}

// ---- AcoustID first (M-SYNC Task 12; unifiedEnrichment.acoustidOrder.test.ts) ----

/// An AcoustID answer: one confident result whose only recording is `rec`.
fn pick(rec: &str, title: &str, secs: u32) -> Entry {
    lookup(json!({"status": "ok", "results": [
        {"id": "a1b2c3d4-0000-4000-8000-0000000000ff", "score": 0.93,
         "recordings": [{"id": rec, "title": title, "duration": secs}]},
    ]}))
}

fn nothing() -> Entry {
    lookup(json!({"status": "ok", "results": []}))
}

fn requests(f: &Fx) -> Vec<String> {
    f.replay.asked()
}

fn is_search(r: &str) -> bool {
    r.contains("query=")
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_key_ladder_unchanged(pool: PgPool) {
    // Review Focus 3: with no key every new AcoustID step is skipped without a request, and the
    // old rungs run in their old order (tracklist, ISRC, search) with the old answers.
    let mut entries = catalogue(vec![row(&id(5), "Decode", 218_000, "Carpenter Brut", &[])]);
    entries.push(get(
        &isrc_url(ISRC),
        200,
        json!({"recordings": [recording(&id(2), "Decode", 218_000, "Paramore")]}),
    ));
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;
    let mut t = with_rg(track());
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());

    let r = matching::identify(&f.ctx, &t, no_key()).await;

    match r {
        LadderResult::NoMatch { asked, skipped } => {
            assert_eq!(asked, vec!["tracklist", "isrc", "search"]);
            assert_eq!(skipped, Some(SkipReason::NoKey));
        }
        other => panic!("expected no match, got {other:?}"),
    }
    assert!(requests(&f).iter().all(|r| !r.contains("/lookup")));

    // And an answer from a lower rung is the same answer it always was.
    let rec = id(8);
    let f = fx(
        &pool,
        vec![
            get(
                Q1,
                200,
                json!({"recordings": [
                    {"id": rec, "title": "Cheerleader Effect", "length": 218_000, "score": 100,
                     "artist-credit": credit("Carpenter Brut")},
                ]}),
            ),
            details(&rec, &[]),
        ],
    )
    .await;
    let a = matched(matching::identify(&f.ctx, &track(), no_key()).await);
    assert_eq!((a.rung, a.recording), (Rung::Search, mbid(&rec)));
    assert!(requests(&f).iter().all(|r| !r.contains("/lookup")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn asks_acoustid_before_the_name_search(pool: PgPool) {
    let mut entries = vec![nothing()];
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;

    let r = matching::identify(&f.ctx, &track(), ask()).await;

    match r {
        LadderResult::NoMatch { asked, skipped } => {
            assert_eq!(asked, vec!["acoustid", "search"]);
            assert_eq!(skipped, None);
        }
        other => panic!("expected no match, got {other:?}"),
    }
    let sent = requests(&f);
    assert_eq!(sent[0], "POST /lookup");
    assert!(is_search(&sent[1]), "{sent:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn never_runs_the_name_search_for_a_track_acoustid_answered(pool: PgPool) {
    let rec = id(9);
    let mut entries = vec![pick(&rec, "Cheerleader Effect", 218), details(&rec, &[])];
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;

    let a = matched(matching::identify(&f.ctx, &track(), ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::AcoustIdTitle, mbid(&rec)));
    assert!(
        requests(&f)
            .iter()
            .all(|r| !is_search(r) && !r.contains("/isrc/"))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_file_that_names_its_recording_never_asks_acoustid(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, vec![nothing(), details(&rec, &[])]).await;
    let mut t = track();
    t.tag_recording_mbid = Some(Uuid::parse_str(&rec).unwrap());

    let a = matched(matching::identify(&f.ctx, &t, ask()).await);

    assert_eq!(a.rung, Rung::FileTag);
    assert!(requests(&f).iter().all(|r| !r.contains("/lookup")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_acoustid_answer_found_in_the_tracklist_takes_its_isrcs_and_the_album_blanks(
    pool: PgPool,
) {
    // The album's tracklist is read for the recording AcoustID named (its row brings the ISRC,
    // genres and the album's label), as upstream does for `answerSource === "acoustid"`.
    let rec = id(9);
    let mut entries = vec![pick(&rec, "Cheerleader Effect", 218)];
    entries.extend(catalogue(vec![row(
        &rec,
        "Cheerleader Effect",
        218_000,
        "Carpenter Brut",
        &["FRX451500123"],
    )]));
    let f = fx(&pool, entries).await;

    let a = matched(matching::identify(&f.ctx, &with_rg(track()), ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::AcoustIdTitle, mbid(&rec)));
    assert_eq!(a.isrc, IsrcWrite::Set("FRX451500123".into()));
    assert_eq!(a.album_label.as_deref(), Some("No Quarter"));
    assert_eq!(
        a.evidence["reason"],
        json!("AcoustID's recording, found in the album's tracklist")
    );
    assert!(requests(&f).iter().all(|r| !is_search(r)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_acoustid_pick_with_only_the_tracks_length_goes_last_the_tracklists_name_match_wins(
    pool: PgPool,
) {
    let by_length = id(9);
    let on_album = id(2);
    let mut entries = vec![pick(&by_length, "Something Else Entirely", 218)];
    entries.extend(catalogue(vec![row(
        &on_album,
        "Cheerleader Effect",
        218_400,
        "Carpenter Brut",
        &[],
    )]));
    let f = fx(&pool, entries).await;

    let a = matched(matching::identify(&f.ctx, &with_rg(track()), ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::Tracklist, mbid(&on_album)));
    assert_eq!(a.evidence["asked"], json!(["acoustid", "tracklist"]));
    assert!(requests(&f).iter().all(|r| !is_search(r)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_length_only_acoustid_pick_is_taken_when_nothing_else_names_the_recording(pool: PgPool) {
    let rec = id(9);
    let mut entries = vec![pick(&rec, "Something Else Entirely", 218)];
    entries.extend(no_hits());
    entries.push(details(&rec, &["FRX451500123"]));
    let f = fx(&pool, entries).await;

    let a = matched(matching::identify(&f.ctx, &track(), ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::AcoustIdLength, mbid(&rec)));
    assert_eq!(a.evidence["asked"], json!(["acoustid", "search"]));
    assert_eq!(
        a.evidence["match"],
        json!("length only: AcoustID's title is not the track's")
    );
    assert_eq!(a.evidence["fingerprintFrom"], json!("stored"));
    assert_eq!(a.rung.as_str(), "acoustid");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_length_only_pick_loses_to_the_files_isrc(pool: PgPool) {
    let by_length = id(9);
    let studio = id(3);
    let f = fx(
        &pool,
        vec![
            pick(&by_length, "Something Else Entirely", 218),
            get(
                &isrc_url(ISRC),
                200,
                json!({"recordings": [recording(&studio, "Cheerleader Effect", 218_400, "Carpenter Brut")]}),
            ),
            details(&studio, &[]),
        ],
    )
    .await;
    let mut t = track();
    t.isrc = Some(ISRC.into());
    t.isrc_source = Some("id3".into());

    let a = matched(matching::identify(&f.ctx, &t, ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::Isrc, mbid(&studio)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_musicbrainz_failure_defers_the_track_instead_of_taking_a_length_only_pick(pool: PgPool) {
    let f = fx(
        &pool,
        vec![
            pick(&id(9), "Something Else Entirely", 218),
            get(Q1, 500, json!({"error": "boom"})),
        ],
    )
    .await;

    let r = matching::identify(&f.ctx, &track(), ask()).await;

    match r {
        LadderResult::Fail(StageFailure::Transient(failure)) => {
            assert_eq!(failure.code, "HTTP_500")
        }
        other => panic!("expected MusicBrainz's failure, got {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn acoustid_failing_does_not_stop_musicbrainz_from_answering(pool: PgPool) {
    let rec = id(8);
    let f = fx(
        &pool,
        vec![
            Entry {
                status: 500,
                ..lookup(json!({}))
            },
            get(
                Q1,
                200,
                json!({"recordings": [
                    {"id": rec, "title": "Cheerleader Effect", "length": 218_000, "score": 100,
                     "artist-credit": credit("Carpenter Brut")},
                ]}),
            ),
            details(&rec, &[]),
        ],
    )
    .await;

    let a = matched(matching::identify(&f.ctx, &track(), ask()).await);

    assert_eq!((a.rung, a.recording), (Rung::Search, mbid(&rec)));
    assert_eq!(a.evidence["asked"], json!(["acoustid", "search"]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn acoustid_failing_and_musicbrainz_knowing_nothing_the_record_waits_for_acoustid(
    pool: PgPool,
) {
    let mut entries = vec![Entry {
        status: 500,
        ..lookup(json!({}))
    }];
    entries.extend(no_hits());
    let f = fx(&pool, entries).await;

    let r = matching::identify(&f.ctx, &track(), ask()).await;

    // AcoustID's own failure is the cause of the deferral, not a no-match.
    match r {
        LadderResult::Fail(StageFailure::Transient(failure)) => {
            assert_eq!(failure.code, "HTTP_500")
        }
        other => panic!("expected AcoustID's failure, got {other:?}"),
    }
    assert!(requests(&f).iter().any(|r| is_search(r)));
}

/// Pauses the `acoustid` source for 30 minutes in `ctx`.
async fn pause_acoustid(pool: &PgPool, ctx: &IdentCtx) {
    sqlx::query(
        "INSERT INTO source_pauses (source, paused_until, reason) \
         VALUES ('acoustid', date_trunc('second', now()) + interval '30 minutes', 'HTTP 429')",
    )
    .execute(pool)
    .await
    .unwrap();
    ctx.net.load_pauses().await.unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn paused_acoustid_with_no_saved_reply_a_lower_source_does_not_name_the_recording(
    pool: PgPool,
) {
    // MusicBrainz could name it, but while AcoustID is paused the question waits.
    let rec = id(8);
    let f = fx(
        &pool,
        vec![
            get(
                Q1,
                200,
                json!({"recordings": [
                    {"id": rec, "title": "Cheerleader Effect", "length": 218_000, "score": 100,
                     "artist-credit": credit("Carpenter Brut")},
                ]}),
            ),
            details(&rec, &[]),
        ],
    )
    .await;
    let until = time::OffsetDateTime::now_utc() + time::Duration::minutes(30);

    let r = matching::identify(&f.ctx, &track(), AcoustidGate::Paused { until }).await;

    assert_eq!(r, LadderResult::Park { until });
    assert!(requests(&f).is_empty(), "{:?}", requests(&f));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn paused_acoustid_with_a_saved_reply_answers_from_it(pool: PgPool) {
    let rec = id(9);
    let f = fx(&pool, vec![details(&rec, &[])]).await;
    let ctx = keyed(f.ctx.clone());
    let t = seed_track(&pool).await;
    fingerprint_stage(&pool, t, "done").await;
    store_fp(&pool, t).await;
    // A reply saved before the pause (by a batch).
    kima_db::acoustid_replies::put(
        &pool,
        &kima_db::acoustid_replies::fingerprint_hash(FINGERPRINT),
        218,
        kima_db::acoustid_replies::Status::Matched,
        &json!([{"id": "a1b2c3d4-0000-4000-8000-0000000000ff", "score": 0.93, "recordings": [
            {"id": rec, "title": "Cheerleader Effect", "duration": 218.0, "releasegroups": []}]}]),
    )
    .await
    .unwrap();
    pause_acoustid(&pool, &ctx).await;

    // The gate lets the question through: the saved reply settles it without a request.
    let gate = acoustid_gate(&ctx, t).await.unwrap();
    assert!(matches!(gate, AcoustidGate::Ask { .. }), "{gate:?}");
    let a = matched(matching::identify(&ctx, &track(), gate).await);

    assert_eq!((a.rung, a.recording), (Rung::AcoustIdTitle, mbid(&rec)));
    assert!(requests(&f).iter().all(|r| !r.contains("/lookup")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn paused_acoustid_with_a_saved_no_match_lets_the_ladder_go_on_down(pool: PgPool) {
    // A saved "nothing matched" settles AcoustID's question too: the lower sources may answer.
    let mut entries = no_hits();
    entries.push(details(&id(8), &[]));
    let f = fx(&pool, entries).await;
    let ctx = keyed(f.ctx.clone());
    let t = seed_track(&pool).await;
    fingerprint_stage(&pool, t, "done").await;
    store_fp(&pool, t).await;
    kima_db::acoustid_replies::put(
        &pool,
        &kima_db::acoustid_replies::fingerprint_hash(FINGERPRINT),
        218,
        kima_db::acoustid_replies::Status::Unmatched,
        &json!([]),
    )
    .await
    .unwrap();
    pause_acoustid(&pool, &ctx).await;

    let gate = acoustid_gate(&ctx, t).await.unwrap();
    assert!(matches!(gate, AcoustidGate::Ask { .. }), "{gate:?}");
    let r = matching::identify(&ctx, &track(), gate).await;

    match r {
        LadderResult::NoMatch { asked, .. } => assert_eq!(asked, vec!["acoustid", "search"]),
        other => panic!("expected no match, got {other:?}"),
    }
    assert!(requests(&f).iter().all(|r| !r.contains("/lookup")));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_recording_stage_asks_the_songs_ahead_in_one_batch_before_it_claims(pool: PgPool) {
    // Three songs, each with its own fingerprint: one batch request answers them all, and each
    // song's ladder then reads its saved reply instead of asking alone.
    let f = fx(
        &pool,
        vec![Entry {
            url: "/lookup".into(),
            ..lookup(json!({"status": "ok", "fingerprints": [
                {"index": 0, "results": []},
                {"index": 1, "results": []},
                {"index": 2, "results": []},
            ]}))
        }],
    )
    .await;
    let ctx = keyed(f.ctx.clone());
    for i in 0..3 {
        let t = seed_track(&pool).await;
        fingerprint_stage(&pool, t, "done").await;
        let mut conn = pool.acquire().await.unwrap();
        kima_db::identity::store_fingerprint(&mut conn, t, &format!("AQAAfp{i}"), 218)
            .await
            .unwrap();
    }

    let report = kima_ident::stages::recording::run_batch(&ctx, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(report.claimed, 3);
    let lookups = f
        .replay
        .asked()
        .iter()
        .filter(|r| r.contains("/lookup"))
        .count();
    assert_eq!(lookups, 1, "{:?}", f.replay.asked());
    let saved: i64 = sqlx::query_scalar("SELECT count(*) FROM acoustid_replies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_recording_stage_without_a_key_asks_nothing_ahead(pool: PgPool) {
    let f = fx(&pool, no_hits()).await;
    let t = seed_track(&pool).await;
    fingerprint_stage(&pool, t, "done").await;
    store_fp(&pool, t).await;

    let report = kima_ident::stages::recording::run_batch(&f.ctx, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!((report.claimed, report.done), (1, 1));
    assert!(f.replay.asked().iter().all(|r| !r.contains("/lookup")));
    let saved: i64 = sqlx::query_scalar("SELECT count(*) FROM acoustid_replies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(saved, 0);
}

#[test]
fn rung_names() {
    // `stage.source` keeps the name `acoustid` for both AcoustID rungs.
    assert_eq!(
        [
            Rung::FileTag,
            Rung::AcoustIdTitle,
            Rung::AcoustIdLength,
            Rung::Tracklist,
            Rung::Isrc,
            Rung::Search,
        ]
        .map(Rung::as_str),
        [
            "file_tag",
            "acoustid",
            "acoustid",
            "tracklist",
            "isrc",
            "search"
        ]
    );
}
