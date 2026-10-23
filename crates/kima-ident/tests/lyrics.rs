//! Lyrics (spec §14): the LRCLIB client and `lyrics::for_track`, against the replay server. Every
//! answer is a hand-written entry (`tests/fixtures/README.md`); nothing reaches lrclib.net.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kima_core::identify::IdentifySwitch;
use kima_ident::lrclib;
use kima_ident::lyrics::{self, LyricsAnswer};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

type Fields = (&'static str, &'static str, &'static str, i32);

/// Recorded with synced lyrics (`lrclib/lyrics.json`).
const HIT: Fields = (
    "Amazing Grace",
    "Judy Collins",
    "Whales & Nightingales",
    243,
);
/// Recorded as a 404.
const NOT_FOUND: Fields = ("No Such Song", "Nobody In Particular", "Nothing Here", 200);
/// Recorded as a 500.
const BROKEN: Fields = (
    "Server Trouble",
    "Nobody In Particular",
    "Nothing Here",
    200,
);

const HIT_ID: i32 = 3_396_226;
const HIT_PATH: &str = "GET /get?track_name=Amazing%20Grace&artist_name=Judy%20Collins\
                        &album_name=Whales%20%26%20Nightingales&duration=243";

fn fixture() -> Vec<Entry> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lrclib/lyrics.json");
    Recording::load(path).0
}

fn on() -> IdentifySwitch {
    IdentifySwitch::new(false, true)
}

fn net_with(pool: &PgPool, replay: &Replay, switch: &IdentifySwitch, scale: f64) -> Net {
    let mut cfg = NetConfig::for_tests(Bases::all(&replay.url()));
    cfg.wait_scale = scale;
    Net::new(cfg, pool.clone(), switch.clone())
}

async fn setup(pool: &PgPool) -> (Replay, Net) {
    let replay = Replay::start(Recording(fixture())).await;
    let net = net_with(pool, &replay, &on(), 0.001);
    (replay, net)
}

/// A present track titled `title` on `album` by album artist `artist`, `secs` long.
async fn seed(pool: &PgPool, (title, artist, album, secs): Fields) -> Uuid {
    let artist_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, enrichment_status) VALUES ($1, $2, $3, 'completed')",
    )
    .bind(artist_id)
    .bind(artist)
    .bind(artist_id.to_string())
    .execute(pool)
    .await
    .unwrap();
    let album_id = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, $3, $4)")
        .bind(album_id)
        .bind(artist_id)
        .bind(album)
        .bind(album_id.to_string())
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, duration_secs) \
         VALUES ($1, $2, 'someone else', $3, 'flac', $4, 1, now(), '\\x00', $5)",
    )
    .bind(id)
    .bind(album_id)
    .bind(title)
    .bind(id.to_string())
    .bind(secs)
    .execute(pool)
    .await
    .unwrap();
    id
}

type Row = (
    Option<String>,
    Option<String>,
    String,
    Option<i32>,
    OffsetDateTime,
);

async fn row(pool: &PgPool, track: Uuid) -> Option<Row> {
    sqlx::query_as(
        "SELECT plain_lyrics, synced_lyrics, source, lrclib_id, fetched_at
         FROM track_lyrics WHERE track_id = $1",
    )
    .bind(track)
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn put_row(pool: &PgPool, track: Uuid, source: &str, plain: Option<&str>, age_days: i64) {
    sqlx::query(
        "INSERT INTO track_lyrics (track_id, plain_lyrics, source, fetched_at)
         VALUES ($1, $2, $3, now() - make_interval(days => $4::int))",
    )
    .bind(track)
    .bind(plain)
    .bind(source)
    .bind(age_days as i32)
    .execute(pool)
    .await
    .unwrap();
}

fn answer(plain: Option<&str>, synced: Option<&str>, source: &str) -> LyricsAnswer {
    LyricsAnswer {
        plain: plain.map(str::to_string),
        synced: synced.map(str::to_string),
        source: source.to_string(),
    }
}

fn nothing() -> LyricsAnswer {
    answer(None, None, "none")
}

fn hit_answer() -> LyricsAnswer {
    let body = &fixture()[0].body;
    answer(
        body["plainLyrics"].as_str(),
        body["syncedLyrics"].as_str(),
        "lrclib",
    )
}

async fn ask(pool: &PgPool, net: &Net, switch: &IdentifySwitch, track: Uuid) -> LyricsAnswer {
    lyrics::for_track(pool, net, switch, track)
        .await
        .unwrap()
        .expect("the track exists")
}

fn recent(t: OffsetDateTime) -> bool {
    OffsetDateTime::now_utc() - t < time::Duration::minutes(5)
}

// ---- lrclib::get ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_sends_four_parameters_and_reads_the_answer(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let (t, a, al, d) = HIT;
    let Outcome::Answered(got) = lrclib::get(&net, t, a, al, d).await else {
        panic!("expected an answer");
    };
    assert_eq!(got.id, HIT_ID);
    assert_eq!(got.plain_lyrics, hit_answer().plain);
    assert_eq!(got.synced_lyrics, hit_answer().synced);
    assert_eq!(replay.asked(), vec![HIT_PATH.to_string()]);
    assert!(replay.user_agents()[0].starts_with("kima-rs/"));

    let (t, a, al, d) = NOT_FOUND;
    assert!(matches!(
        lrclib::get(&net, t, a, al, d).await,
        Outcome::Absent(_)
    ));
    let (t, a, al, d) = BROKEN;
    assert_eq!(
        lrclib::get(&net, t, a, al, d).await.code(),
        Some("HTTP_500")
    );
    // Not cached: asking again asks again.
    let (t, a, al, d) = HIT;
    assert!(matches!(
        lrclib::get(&net, t, a, al, d).await,
        Outcome::Answered(_)
    ));
    assert_eq!(replay.asked().len(), 4);
}

fn entry(title: &str, status: u16, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: format!(
            "/get?track_name={}&artist_name=x&album_name=y&duration=1",
            title.replace(' ', "%20")
        ),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn get_tolerates_untrusted_answers(pool: PgPool) {
    let huge = "é".repeat(200 * 1024) + "x"; // 400 KiB
    let replay = Replay::start(Recording(vec![
        entry("sparse", 200, json!({"id": 7})),
        entry(
            "nulls",
            200,
            json!({"id": 8, "plainLyrics": null, "syncedLyrics": null, "instrumental": true}),
        ),
        entry("overflow", 200, json!({"id": 4_294_967_296_i64})),
        entry("no id", 200, json!({"plainLyrics": "la"})),
        entry(
            "huge",
            200,
            json!({"id": 9, "plainLyrics": huge, "syncedLyrics": "[00:01.00] la"}),
        ),
        entry("not json", 200, json!("just a string")),
    ]))
    .await;
    let net = net_with(&pool, &replay, &on(), 0.001);
    let get = |t: &'static str| lrclib::get(&net, t, "x", "y", 1);

    let Outcome::Answered(a) = get("sparse").await else {
        panic!("sparse")
    };
    assert_eq!((a.id, a.plain_lyrics, a.synced_lyrics), (7, None, None));
    let Outcome::Answered(a) = get("nulls").await else {
        panic!("nulls")
    };
    assert_eq!((a.id, a.plain_lyrics, a.synced_lyrics), (8, None, None));
    assert_eq!(get("overflow").await.code(), Some("BAD_RESPONSE"));
    assert_eq!(get("no id").await.code(), Some("BAD_RESPONSE"));
    assert_eq!(get("not json").await.code(), Some("BAD_RESPONSE"));
    let Outcome::Answered(a) = get("huge").await else {
        panic!("huge")
    };
    let plain = a.plain_lyrics.unwrap();
    assert!(plain.len() <= kima_db::lyrics::MAX_LYRICS_BYTES);
    assert!(plain.len() > kima_db::lyrics::MAX_LYRICS_BYTES - 4);
    assert!(plain.chars().all(|c| c == 'é'));
    assert_eq!(a.synced_lyrics.as_deref(), Some("[00:01.00] la"));
}

// ---- lyrics::for_track ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_track_is_none(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let got = lyrics::for_track(&pool, &net, &on(), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(got, None);
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedded_row_returned_without_request(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, HIT).await;
    let mut conn = pool.acquire().await.unwrap();
    kima_db::lyrics::upsert_embedded(&mut conn, track, Some("from the tag"), Some("[00:01.00] x"))
        .await
        .unwrap();
    drop(conn);
    assert_eq!(
        ask(&pool, &net, &on(), track).await,
        answer(Some("from the tag"), Some("[00:01.00] x"), "embedded")
    );
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stored_lrclib_row_returned_without_request(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, HIT).await;
    put_row(&pool, track, "lrclib", Some("old words"), 400).await;
    assert_eq!(
        ask(&pool, &net, &on(), track).await,
        answer(Some("old words"), None, "lrclib")
    );
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn none_younger_than_30_days_asks_nothing(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, HIT).await;
    put_row(&pool, track, "none", None, 29).await;
    let before = row(&pool, track).await;
    assert_eq!(ask(&pool, &net, &on(), track).await, nothing());
    assert!(replay.asked().is_empty());
    assert_eq!(row(&pool, track).await, before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn none_older_than_30_days_asks_again(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, HIT).await;
    put_row(&pool, track, "none", None, 31).await;
    assert_eq!(ask(&pool, &net, &on(), track).await, hit_answer());
    assert_eq!(replay.asked(), vec![HIT_PATH.to_string()]);
    let (plain, synced, source, id, fetched) = row(&pool, track).await.unwrap();
    assert_eq!((plain, synced), (hit_answer().plain, hit_answer().synced));
    assert_eq!((source.as_str(), id), ("lrclib", Some(HIT_ID)));
    assert!(recent(fetched));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hit_upserts_lrclib(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, HIT).await;
    assert_eq!(ask(&pool, &net, &on(), track).await, hit_answer());
    assert_eq!(replay.asked(), vec![HIT_PATH.to_string()]);
    let (plain, synced, source, id, fetched) = row(&pool, track).await.unwrap();
    assert_eq!((plain, synced), (hit_answer().plain, hit_answer().synced));
    assert_eq!((source.as_str(), id), ("lrclib", Some(HIT_ID)));
    assert!(recent(fetched));
    // The stored row answers from now on.
    assert_eq!(ask(&pool, &net, &on(), track).await, hit_answer());
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lrclib_404_stores_none(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, NOT_FOUND).await;
    assert_eq!(ask(&pool, &net, &on(), track).await, nothing());
    let (plain, synced, source, id, fetched) = row(&pool, track).await.unwrap();
    assert_eq!(
        (plain, synced, source.as_str(), id),
        (None, None, "none", None)
    );
    assert!(recent(fetched));
    // Cached for 30 days: no second request.
    assert_eq!(ask(&pool, &net, &on(), track).await, nothing());
    assert_eq!(replay.asked().len(), 1);

    // An old none row is refreshed by a new 404.
    let old = seed(&pool, NOT_FOUND).await;
    put_row(&pool, old, "none", None, 45).await;
    assert_eq!(ask(&pool, &net, &on(), old).await, nothing());
    assert!(recent(row(&pool, old).await.unwrap().4));
    assert_eq!(replay.asked().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failure_answers_existing_values_writes_nothing(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    // No row: nulls, and still no row.
    let fresh = seed(&pool, BROKEN).await;
    assert_eq!(ask(&pool, &net, &on(), fresh).await, nothing());
    assert_eq!(row(&pool, fresh).await, None);
    // A stale none row: its values, untouched.
    let stale = seed(&pool, BROKEN).await;
    put_row(&pool, stale, "none", None, 60).await;
    let before = row(&pool, stale).await;
    assert_eq!(ask(&pool, &net, &on(), stale).await, nothing());
    assert_eq!(row(&pool, stale).await, before);
    assert_eq!(replay.asked().len(), 2);

    // A network failure: nothing listens on the base.
    let dead = Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool.clone(),
        on(),
    );
    let track = seed(&pool, HIT).await;
    assert_eq!(ask(&pool, &dead, &on(), track).await, nothing());
    assert_eq!(row(&pool, track).await, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn identify_off_answers_from_storage_no_request(pool: PgPool) {
    let replay = Replay::start(Recording(fixture())).await;
    for switch in [
        IdentifySwitch::new(false, false),
        IdentifySwitch::new(true, true),
    ] {
        let net = net_with(&pool, &replay, &switch, 0.001);
        let fresh = seed(&pool, HIT).await;
        assert_eq!(ask(&pool, &net, &switch, fresh).await, nothing());
        assert_eq!(row(&pool, fresh).await, None);
        let stale = seed(&pool, HIT).await;
        put_row(&pool, stale, "none", None, 90).await;
        let before = row(&pool, stale).await;
        assert_eq!(ask(&pool, &net, &switch, stale).await, nothing());
        assert_eq!(row(&pool, stale).await, before);
        // A switch the route was given off, even with a client whose own switch is on.
        let other = net_with(&pool, &replay, &on(), 0.001);
        assert_eq!(ask(&pool, &other, &switch, fresh).await, nothing());
    }
    assert!(replay.asked().is_empty());
}

/// Holds tokio's paused clock still while it lives (a running blocking task inhibits
/// auto-advance), so real I/O to Postgres and the replay server cannot jump the clock to a
/// timeout. Time moves only by `tokio::time::advance`.
struct Frozen {
    _release: std::sync::mpsc::Sender<()>,
}

fn freeze() -> Frozen {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    tokio::task::spawn_blocking(move || {
        let _ = rx.recv();
    });
    tokio::time::pause();
    Frozen { _release: tx }
}

/// Lets real time pass while the paused clock stands still.
async fn real_sleep(ms: u64) {
    tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(ms)))
        .await
        .unwrap();
}

/// Waits (in real time, at most 20 s) until `n` of `tasks` are finished, so a regression fails
/// instead of hanging on a frozen clock.
async fn until_finished<T>(tasks: &[tokio::task::JoinHandle<T>], n: usize, what: &str) {
    for _ in 0..400 {
        if tasks.iter().filter(|t| t.is_finished()).count() >= n {
            return;
        }
        real_sleep(50).await;
    }
    panic!("{what}: {n} of the lookups never finished");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn limiter_wait_over_5s_answers_from_storage(pool: PgPool) {
    let replay = Replay::start(Recording(fixture())).await;
    // Real limiter spacing: 1 request per 3 s, one in flight.
    let net = Arc::new(net_with(&pool, &replay, &on(), 1.0));
    let tracks = [
        seed(&pool, HIT).await,
        seed(&pool, HIT).await,
        seed(&pool, HIT).await,
    ];
    let frozen = freeze();
    let start = tokio::time::Instant::now();
    let tasks: Vec<_> = tracks
        .iter()
        .map(|&track| {
            let (pool, net) = (pool.clone(), net.clone());
            tokio::spawn(async move { (track, ask(&pool, &net, &on(), track).await) })
        })
        .collect();
    // The first sends at once; the second's cell is 3 s away, within its 5 s wait.
    until_finished(&tasks, 1, "the first").await;
    assert_eq!(replay.asked().len(), 1);
    // The other two reach the limiter at the same frozen instant: give their database reads
    // real time first, then check nothing more was sent.
    real_sleep(500).await;
    assert_eq!(replay.asked().len(), 1);
    assert_eq!(tasks.iter().filter(|t| t.is_finished()).count(), 1);
    // Timers fire on whole milliseconds, rounded up: step just past the cell.
    tokio::time::advance(Duration::from_millis(3_010)).await;
    // The third gets the slot at 3 s; its cell would be at 6 s, past its 5 s wait.
    until_finished(&tasks, 3, "the rest").await;
    let mut done = Vec::new();
    for t in tasks {
        done.push(t.await.unwrap());
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    drop(frozen);
    assert_eq!(replay.asked().len(), 2);
    let answers: Vec<&LyricsAnswer> = done.iter().map(|(_, a)| a).collect();
    assert_eq!(answers.iter().filter(|a| ***a == hit_answer()).count(), 2);
    assert_eq!(answers.iter().filter(|a| ***a == nothing()).count(), 1);
    let mut stored = 0;
    for (track, a) in &done {
        match row(&pool, *track).await {
            Some(r) => {
                stored += 1;
                assert_eq!(r.2, "lrclib");
                assert_eq!(a, &hit_answer());
            }
            None => assert_eq!(a, &nothing()),
        }
    }
    assert_eq!(stored, 2, "the busy track writes nothing");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_track_answers_like_present(pool: PgPool) {
    let (replay, net) = setup(&pool).await;
    let track = seed(&pool, HIT).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(ask(&pool, &net, &on(), track).await, hit_answer());
    assert_eq!(row(&pool, track).await.unwrap().2, "lrclib");
    let none = seed(&pool, NOT_FOUND).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(none)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(ask(&pool, &net, &on(), none).await, nothing());
    assert_eq!(row(&pool, none).await.unwrap().2, "none");
    assert_eq!(replay.asked().len(), 2);
}

// ---- kima_db::lyrics writes ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lookup_writes_never_replace_embedded(pool: PgPool) {
    use kima_db::lyrics as db;
    let track = seed(&pool, HIT).await;
    assert_eq!(
        db::lookup_fields(&pool, track).await.unwrap(),
        Some((
            "Amazing Grace".to_string(),
            "Judy Collins".to_string(),
            "Whales & Nightingales".to_string(),
            243
        ))
    );
    assert_eq!(
        db::lookup_fields(&pool, Uuid::new_v4()).await.unwrap(),
        None
    );

    let mut conn = pool.acquire().await.unwrap();
    db::upsert_embedded(&mut conn, track, Some("tag"), None)
        .await
        .unwrap();
    drop(conn);
    // A lookup that raced the scanner leaves the embedded row alone.
    assert!(
        !db::upsert_lrclib(&pool, track, Some("web"), None, 1)
            .await
            .unwrap()
    );
    assert!(!db::upsert_none(&pool, track).await.unwrap());
    let got = db::get(&pool, track).await.unwrap().unwrap();
    assert_eq!(
        (got.plain.as_deref(), got.source.as_str()),
        (Some("tag"), "embedded")
    );

    // A none never replaces an lrclib row; lrclib replaces none and refreshes lrclib.
    let other = seed(&pool, HIT).await;
    assert!(db::upsert_none(&pool, other).await.unwrap());
    assert!(
        db::upsert_lrclib(&pool, other, Some("web"), Some("[00:01.00] web"), 5)
            .await
            .unwrap()
    );
    assert!(!db::upsert_none(&pool, other).await.unwrap());
    assert!(
        db::upsert_lrclib(&pool, other, Some("new"), None, 6)
            .await
            .unwrap()
    );
    let got = db::get(&pool, other).await.unwrap().unwrap();
    assert_eq!(
        (
            got.plain.as_deref(),
            got.synced.as_deref(),
            got.source.as_str()
        ),
        (Some("new"), None, "lrclib")
    );
    assert_eq!(row(&pool, other).await.unwrap().3, Some(6));

    // A track deleted meanwhile is no error and no row.
    let gone = Uuid::new_v4();
    assert!(
        !db::upsert_lrclib(&pool, gone, Some("x"), None, 1)
            .await
            .unwrap()
    );
    assert!(!db::upsert_none(&pool, gone).await.unwrap());

    // Stored text is capped at a character boundary.
    let big = "é".repeat(db::MAX_LYRICS_BYTES);
    let capped = seed(&pool, HIT).await;
    db::upsert_lrclib(&pool, capped, Some(&format!("a{big}")), None, 1)
        .await
        .unwrap();
    let got = db::get(&pool, capped).await.unwrap().unwrap();
    assert_eq!(got.plain.unwrap().len(), db::MAX_LYRICS_BYTES - 1);
}
