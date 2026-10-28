//! The autotag search job (spec §19.1-§19.2) against the replay server. MusicBrainz answers
//! come from the hand-written `fixtures/musicbrainz/autotag.json` plus entries built here;
//! nothing reaches musicbrainz.org.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kima_core::identify::IdentifySwitch;
use kima_db::autotag::{self, LocalAlbum, Origin};
use kima_ident::IdentCtx;
use kima_ident::Mbid;
use kima_ident::autotag::search::{
    self, ManualQuery, OFF_MESSAGE, SearchAttempt, SearchJob, parse_release_ref,
};
use kima_ident::musicbrainz::{ArtistClause, release_query};
use kima_jobs::worker::JobError;
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Outcome};
use kima_test_support::{Tags, make_audio, require_ffmpeg};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const R1: &str = "c0000001-0000-4000-8000-000000000001";
const R2: &str = "c0000002-0000-4000-8000-000000000001";
const R3: &str = "c0000003-0000-4000-8000-000000000001";
const RG1: &str = "d0000001-0000-4000-8000-000000000001";
const REC1: &str = "8e010100-0000-4000-8000-000000000001";
const MISSING: &str = "c0000099-0000-4000-8000-000000000001";

fn mbid(s: &str) -> Mbid {
    Mbid::parse(s).unwrap()
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
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

/// The URL of a release search for `query` (as `search_releases` sends it).
fn search_url(query: &str) -> String {
    format!(
        "/release?query={}&limit=5&fmt=json",
        utf8_percent_encode(query, NON_ALPHANUMERIC)
    )
}

/// A release search answering `ids`.
fn search_entry(query: &str, ids: &[&str]) -> Entry {
    let releases: Vec<Value> = ids.iter().map(|id| json!({"id": id})).collect();
    entry(
        &search_url(query),
        200,
        json!({"count": ids.len(), "releases": releases}),
    )
}

fn recording_entry(rec: &str, releases: &[&str]) -> Entry {
    let releases: Vec<Value> = releases.iter().map(|id| json!({"id": id})).collect();
    entry(
        &format!("/recording/{rec}?inc=releases&fmt=json"),
        200,
        json!({"id": rec, "title": "x", "releases": releases}),
    )
}

/// The fixture recording plus `extra`, served from a fresh replay server.
async fn ctx_with(pool: &PgPool, extra: Vec<Entry>, switch: IdentifySwitch) -> (Replay, IdentCtx) {
    let mut rec = fixture();
    rec.0.extend(extra);
    let replay = Replay::start(rec).await;
    let ctx = common::ident_ctx_with(
        pool,
        Path::new("/nonexistent"),
        Bases::all(&replay.url()),
        switch,
    );
    (replay, ctx)
}

async fn ctx(pool: &PgPool, extra: Vec<Entry>) -> (Replay, IdentCtx) {
    ctx_with(pool, extra, IdentifySwitch::new(false, true)).await
}

/// One local track: `(title, track_no, recording mbid and source)`.
struct T<'a> {
    title: String,
    no: i32,
    recording: Option<(&'a str, &'a str)>,
}

fn titled(prefix: &str, n: i32) -> Vec<T<'static>> {
    (1..=n)
        .map(|i| T {
            title: format!("{prefix} {i}"),
            no: i,
            recording: None,
        })
        .collect()
}

/// An album row with present tracks (2 s each, artist = the album artist).
async fn album(
    pool: &PgPool,
    title: &str,
    artist: &str,
    compilation: bool,
    release: Option<&str>,
    tracks: &[T<'_>],
) -> Uuid {
    let artist_id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $3)")
        .bind(artist_id)
        .bind(artist)
        .bind(format!("{artist}-{artist_id}"))
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year, is_compilation, release_mbid,
                             release_mbid_source)
         VALUES ($1, $2, $3, $4, 2001, $5, $6, CASE WHEN $6 IS NULL THEN NULL ELSE 'tag' END)",
    )
    .bind(id)
    .bind(artist_id)
    .bind(title)
    .bind(id.to_string())
    .bind(compilation)
    .bind(release.map(uuid))
    .execute(pool)
    .await
    .unwrap();
    for t in tracks {
        sqlx::query(
            "INSERT INTO tracks (id, album_id, artist_name, title, track_no, codec, path, size,
                                 mtime, fingerprint, duration_secs, recording_mbid,
                                 recording_source)
             VALUES ($1, $2, $3, $4, $5, 'flac', $6, 1, now(), '\\x00', 2, $7, $8)",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(artist)
        .bind(&t.title)
        .bind(t.no)
        .bind(format!("{id}/{:02}.flac", t.no))
        .bind(t.recording.map(|(r, _)| uuid(r)))
        .bind(t.recording.map(|(_, s)| s))
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

async fn local(pool: &PgPool, id: Uuid) -> LocalAlbum {
    autotag::local_album(pool, id).await.unwrap().unwrap()
}

fn attempt(last: bool) -> SearchAttempt {
    SearchAttempt {
        last,
        ..SearchAttempt::default()
    }
}

async fn album_state(pool: &PgPool, id: Uuid) -> Option<(String, Option<String>, Option<String>)> {
    sqlx::query_as("SELECT state, recommendation, error FROM autotag_albums WHERE album_id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

fn count(asked: &[String], prefix: &str) -> usize {
    asked.iter().filter(|a| a.starts_with(prefix)).count()
}

fn fixture_query(tracks: Option<usize>) -> String {
    release_query("Fixture Album", ArtistClause::Name("Fixture Band"), tracks)
}

/// §1's M3b success: ten untagged-but-titled tones of "Fixture Band - Fixture Album" find the
/// fixture release as a strong match, with both snapshots stored.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn untagged_copy_matches_strong(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=10 {
        make_audio(
            dir.path(),
            &format!("Fixture Band/Fixture Album/{i:02}.flac"),
            Tags {
                album_artist: Some("Fixture Band".into()),
                artist: Some("Fixture Band".into()),
                album: Some("Fixture Album".into()),
                title: Some(format!("Fixture Song {i}")),
                date: Some("2001".into()),
                track: Some(i.to_string()),
                ..Tags::default()
            },
        );
    }
    let mut rec = fixture();
    rec.0.push(search_entry(&fixture_query(None), &[R1]));
    let replay = Replay::start(rec).await;
    let ctx = common::ident_ctx(&pool, dir.path(), Bases::all(&replay.url()));
    kima_library::scan::reconcile(&ctx.library).await.unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT id FROM albums WHERE title = 'Fixture Album'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut rx = ctx.events.subscribe();

    search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap();

    assert_eq!(
        album_state(&pool, id).await,
        Some(("proposed".into(), Some("strong".into()), None))
    );
    let cands = autotag::candidates(&pool, id).await.unwrap();
    assert_eq!(cands.len(), 1);
    let best = &cands[0];
    assert_eq!(best.release_mbid, uuid(R1));
    assert_eq!(best.release_group_mbid, uuid(RG1));
    assert_eq!(best.origin, "search");
    assert_eq!(best.recommendation, "strong");
    assert!(best.distance < 0.04, "{}", best.distance);
    assert_eq!(best.release["title"], "Fixture Album");
    assert_eq!(
        best.release["media"][0]["tracks"].as_array().unwrap().len(),
        10
    );
    assert_eq!(best.release_group["id"], RG1);
    assert_eq!(best.release_group["genres"][0]["name"], "rock");
    assert_eq!(best.mapping.as_array().unwrap().len(), 10);
    assert_eq!(best.extra_tracks, json!([]));
    assert_eq!(best.missing_tracks, json!([]));
    let age = time::OffsetDateTime::now_utc() - best.fetched_at;
    assert!(age < time::Duration::minutes(5), "{age}");
    let best_id: Option<Uuid> =
        sqlx::query_scalar("SELECT best_candidate FROM autotag_albums WHERE album_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(best_id, Some(best.id));

    // The search announced itself; its last event is the final state.
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if e.data["type"] == "autotag:progress" {
            events.push(e.data);
        }
    }
    assert_eq!(events.first().unwrap()["state"], "searching");
    assert_eq!(
        events.last().unwrap(),
        &json!({"type": "autotag:progress", "job": "search", "albumId": id, "done": 1,
                "total": 1, "state": "proposed"})
    );
}

/// About two searches, at most five recording lookups and ten release and group fetches; a
/// second identical search is answered by the response cache.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn request_budget(pool: PgPool) {
    let mut tracks = titled("Fixture Song", 10);
    tracks[0].recording = Some((REC1, "file_tag"));
    let id = album(&pool, "Fixture Album", "Fixture Band", false, None, &tracks).await;
    let (replay, ctx) = ctx(&pool, vec![search_entry(&fixture_query(None), &[R1])]).await;

    search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap();
    let asked = replay.asked();
    assert!(
        (1..=2).contains(&count(&asked, "GET /release?")),
        "{asked:?}"
    );
    assert!(count(&asked, "GET /recording/") <= 5, "{asked:?}");
    assert!(count(&asked, "GET /release/") <= 10, "{asked:?}");
    assert!(count(&asked, "GET /release-group/") <= 10, "{asked:?}");
    // REC1 lists R1 and R3: both candidates, each fetched once with its group.
    assert_eq!(count(&asked, "GET /recording/"), 1);
    assert_eq!(count(&asked, "GET /release/"), 2);
    assert_eq!(count(&asked, "GET /release-group/"), 2);
    let cands = autotag::candidates(&pool, id).await.unwrap();
    let origins: Vec<(Uuid, String)> = cands
        .iter()
        .map(|c| (c.release_mbid, c.origin.clone()))
        .collect();
    assert_eq!(
        origins,
        [
            (uuid(R1), "search".to_string()),
            (uuid(R3), "recording".to_string())
        ]
    );

    search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap();
    assert_eq!(replay.asked().len(), asked.len(), "served from the cache");
    assert_eq!(autotag::candidates(&pool, id).await.unwrap().len(), 2);
}

/// The album's tag first, then the manual MBID, then the search.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tag_candidate_first_and_manual_mbid(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(
        &pool,
        "Fixture Album",
        "Fixture Band",
        false,
        Some(R2),
        &tracks,
    )
    .await;
    let (_replay, ctx) = ctx(&pool, vec![search_entry(&fixture_query(None), &[R1])]).await;
    let job = SearchJob {
        album_id: id,
        query: None,
        release_mbid: Some(mbid(R3)),
    };

    let Outcome::Answered(found) = search::collect(&ctx, &local(&pool, id).await, &job).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(
        found,
        [
            (mbid(R2), Origin::Tag),
            (mbid(R3), Origin::Manual),
            (mbid(R1), Origin::Search)
        ]
    );

    // The manual MBID repeating the tag is kept once, as the tag.
    let again = SearchJob {
        release_mbid: Some(mbid(R2)),
        ..job.clone()
    };
    let Outcome::Answered(found) = search::collect(&ctx, &local(&pool, id).await, &again).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(found, [(mbid(R2), Origin::Tag), (mbid(R1), Origin::Search)]);

    search::run_search(&ctx, &job, attempt(false))
        .await
        .unwrap();
    let mut stored: Vec<(Uuid, String)> = autotag::candidates(&pool, id)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.release_mbid, c.origin))
        .collect();
    stored.sort();
    assert_eq!(
        stored,
        [
            (uuid(R1), "search".to_string()),
            (uuid(R2), "tag".to_string()),
            (uuid(R3), "manual".to_string())
        ]
    );
}

/// A manual query replaces the album's title and artist in the release search.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn manual_query_replaces_title_and_artist(pool: PgPool) {
    let tracks = titled("Song", 6);
    let id = album(&pool, "Something Else", "Someone", false, None, &tracks).await;
    let q = release_query(
        "Double Fixture",
        ArtistClause::Name("Fixture Band"),
        Some(6),
    );
    let q_retry = release_query("Double Fixture", ArtistClause::Name("Fixture Band"), None);
    let (replay, ctx) = ctx(&pool, vec![search_entry(&q_retry, &[R2])]).await;
    let job = SearchJob {
        album_id: id,
        query: Some(ManualQuery {
            artist: "Fixture Band".into(),
            album: "Double Fixture".into(),
        }),
        release_mbid: None,
    };
    let Outcome::Answered(found) = search::collect(&ctx, &local(&pool, id).await, &job).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(found, [(mbid(R2), Origin::Search)]);
    assert_eq!(
        replay.asked(),
        [
            format!("GET {}", search_url(&q)),
            format!("GET {}", search_url(&q_retry))
        ]
    );
}

/// Three probed recordings: a release listed by two is kept, one listed by one is not.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recording_candidates_majority(pool: PgPool) {
    let (ra, rb, rc) = (
        "8e090100-0000-4000-8000-000000000001",
        "8e090100-0000-4000-8000-000000000002",
        "8e090100-0000-4000-8000-000000000003",
    );
    let mut tracks = titled("Probe Song", 3);
    tracks[0].recording = Some((ra, "file_tag"));
    tracks[1].recording = Some((rb, "isrc"));
    tracks[2].recording = Some((rc, "acoustid"));
    let id = album(&pool, "Probe Album", "Probe Band", false, None, &tracks).await;
    let q = |n| release_query("Probe Album", ArtistClause::Name("Probe Band"), n);
    let (_replay, ctx) = ctx(
        &pool,
        vec![
            search_entry(&q(Some(3)), &[]),
            search_entry(&q(None), &[]),
            recording_entry(ra, &[R1, R2]),
            recording_entry(rb, &[R1]),
            recording_entry(rc, &[R2, R3, R2]),
        ],
    )
    .await;
    let Outcome::Answered(found) =
        search::collect(&ctx, &local(&pool, id).await, &SearchJob::plain(id)).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(
        found,
        [(mbid(R1), Origin::Recording), (mbid(R2), Origin::Recording)]
    );
}

/// At most five recordings are probed, those from file tags, tracklists and ISRCs first.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recording_probe_prefers_trusted_sources(pool: PgPool) {
    let recs: Vec<String> = (1..=7)
        .map(|i| format!("8e080100-0000-4000-8000-00000000000{i}"))
        .collect();
    let sources = [
        "search",
        "acoustid",
        "file_tag",
        "tracklist",
        "isrc",
        "file_tag",
        "file_tag",
    ];
    let mut tracks = titled("Probe Song", 7);
    for (t, (r, s)) in tracks.iter_mut().zip(recs.iter().zip(sources)) {
        t.recording = Some((r.as_str(), s));
    }
    let id = album(&pool, "Probe Album", "Probe Band", false, None, &tracks).await;
    let q = |n| release_query("Probe Album", ArtistClause::Name("Probe Band"), n);
    // Only the five trusted ones are recorded: asking another would fail the collection.
    let mut extra = vec![search_entry(&q(Some(7)), &[]), search_entry(&q(None), &[])];
    extra.extend(recs[2..].iter().map(|r| recording_entry(r, &[R1])));
    let (replay, ctx) = ctx(&pool, extra).await;
    let Outcome::Answered(found) =
        search::collect(&ctx, &local(&pool, id).await, &SearchJob::plain(id)).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(found, [(mbid(R1), Origin::Recording)]);
    assert_eq!(count(&replay.asked(), "GET /recording/"), 5);
}

/// A compilation searches by the Various Artists id instead of an artist name.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn compilation_uses_arid(pool: PgPool) {
    let tracks = titled("Hits Song", 3);
    let id = album(
        &pool,
        "Fixture Hits",
        "Various Artists",
        true,
        None,
        &tracks,
    )
    .await;
    let retry = release_query("Fixture Hits", ArtistClause::Various, None);
    let (replay, ctx) = ctx(&pool, vec![search_entry(&retry, &[R3])]).await;
    let Outcome::Answered(found) =
        search::collect(&ctx, &local(&pool, id).await, &SearchJob::plain(id)).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(found, [(mbid(R3), Origin::Search)]);
    let asked = replay.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[0].contains("tracks%3A3"), "{asked:?}");
    assert!(!asked[1].contains("tracks%3A"), "{asked:?}");
    assert!(asked[0].contains("arid%3A89ad4ac3-39f7-470e-963a-56509c546377"));
    assert!(!asked.iter().any(|a| a.contains("artist%3A")), "{asked:?}");
}

/// Fewer than two hits repeat the search without `AND tracks:<n>`; two or more do not.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fewer_than_two_retries_without_tracks(pool: PgPool) {
    let tracks = titled("Song", 4);
    let one = album(&pool, "One Hit", "Band", false, None, &tracks).await;
    let two = album(&pool, "Two Hits", "Band", false, None, &tracks).await;
    let q = |t, n| release_query(t, ArtistClause::Name("Band"), n);
    let (replay, ctx) = ctx(
        &pool,
        vec![
            search_entry(&q("One Hit", Some(4)), &[R1]),
            search_entry(&q("One Hit", None), &[R2, R1]),
            search_entry(&q("Two Hits", Some(4)), &[R1, R2]),
        ],
    )
    .await;
    let Outcome::Answered(found) =
        search::collect(&ctx, &local(&pool, one).await, &SearchJob::plain(one)).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(
        found,
        [(mbid(R1), Origin::Search), (mbid(R2), Origin::Search)]
    );
    assert_eq!(
        replay.asked(),
        [
            format!("GET {}", search_url(&q("One Hit", Some(4)))),
            format!("GET {}", search_url(&q("One Hit", None)))
        ]
    );

    let Outcome::Answered(found) =
        search::collect(&ctx, &local(&pool, two).await, &SearchJob::plain(two)).await
    else {
        panic!("expected an answer");
    };
    assert_eq!(
        found,
        [(mbid(R1), Origin::Search), (mbid(R2), Origin::Search)]
    );
    assert_eq!(replay.asked().len(), 3, "no retry after two hits");
}

/// A release MusicBrainz no longer has is skipped; the others are stored.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_release_skipped(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(
        &pool,
        "Fixture Album",
        "Fixture Band",
        false,
        Some(MISSING),
        &tracks,
    )
    .await;
    let (_replay, ctx) = ctx(&pool, vec![search_entry(&fixture_query(None), &[R1])]).await;
    search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap();
    let cands = autotag::candidates(&pool, id).await.unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].release_mbid, uuid(R1));
}

/// A release whose group cannot be read is skipped, never stored without its group.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_without_readable_group_skipped(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(&pool, "Fixture Album", "Fixture Band", false, None, &tracks).await;
    let mut rec = fixture();
    // R1 answered with a malformed group: everything else as the fixture has it.
    let at = rec
        .0
        .iter()
        .position(|e| e.url.starts_with(&format!("/release/{R1}")))
        .unwrap();
    rec.0[at].body["release-group"] = json!({"id": "not an id"});
    rec.0.push(search_entry(&fixture_query(None), &[R1]));
    let replay = Replay::start(rec).await;
    let ctx = common::ident_ctx(&pool, Path::new("/nonexistent"), Bases::all(&replay.url()));
    search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap();
    assert!(autotag::candidates(&pool, id).await.unwrap().is_empty());
    assert_eq!(
        album_state(&pool, id).await,
        Some(("no_candidates".into(), None, None))
    );
    assert_eq!(count(&replay.asked(), "GET /release-group/"), 0);
}

/// A 503 from MusicBrainz: the same job again at the pause's end, the album `queued`, `Ok`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rate_limit_reenqueues_and_returns_ok(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(&pool, "Fixture Album", "Fixture Band", false, None, &tracks).await;
    let mut rec = fixture();
    rec.0.retain(|e| !e.url.starts_with("/release?"));
    rec.0.push(entry(
        &search_url(&fixture_query(Some(10))),
        503,
        json!({"error": "rate limited"}),
    ));
    let replay = Replay::start(rec).await;
    let ctx = common::ident_ctx(&pool, Path::new("/nonexistent"), Bases::all(&replay.url()));
    let job = SearchJob::plain(id);
    let before = time::OffsetDateTime::now_utc();

    search::run_search(&ctx, &job, attempt(false))
        .await
        .unwrap();

    let jobs: Vec<(Value, time::OffsetDateTime, String)> =
        sqlx::query_as("SELECT payload, run_at, state FROM jobs WHERE queue = 'autotag'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].0, job.payload());
    assert_eq!(jobs[0].2, "queued");
    let pause: time::OffsetDateTime =
        sqlx::query_scalar("SELECT paused_until FROM source_pauses WHERE source = 'musicbrainz'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(jobs[0].1 >= pause, "{} < {pause}", jobs[0].1);
    assert!(jobs[0].1 > before + time::Duration::seconds(20));
    assert_eq!(
        album_state(&pool, id).await,
        Some(("queued".into(), None, None))
    );
}

/// A transient failure is retried; only the last attempt marks the album `failed`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn transient_failure_fails_album_only_on_last_attempt(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(&pool, "Fixture Album", "Fixture Band", false, None, &tracks).await;
    let mut rec = fixture();
    rec.0.retain(|e| !e.url.starts_with("/release?"));
    rec.0.push(entry(
        &search_url(&fixture_query(Some(10))),
        500,
        json!({"error": "boom"}),
    ));
    let replay = Replay::start(rec).await;
    let ctx = common::ident_ctx(&pool, Path::new("/nonexistent"), Bases::all(&replay.url()));
    let job = SearchJob::plain(id);

    let err = search::run_search(&ctx, &job, attempt(false))
        .await
        .unwrap_err();
    assert!(matches!(err, JobError::Retry(_)), "{err:?}");
    assert_eq!(
        album_state(&pool, id).await,
        Some(("queued".into(), None, None))
    );

    let err = search::run_search(&ctx, &job, attempt(true))
        .await
        .unwrap_err();
    let JobError::Retry(text) = err else {
        panic!("expected a retry error");
    };
    assert!(text.contains("HTTP_500"), "{text}");
    assert_eq!(
        album_state(&pool, id).await,
        Some(("failed".into(), None, Some(text)))
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE queue = 'autotag'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "a transient failure queues nothing itself");
}

/// With identification off the job refuses without a request.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn identify_off_refuses_without_request(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(
        &pool,
        "Fixture Album",
        "Fixture Band",
        false,
        Some(R1),
        &tracks,
    )
    .await;
    let (replay, ctx) = ctx_with(&pool, vec![], IdentifySwitch::new(true, true)).await;
    let err = search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap_err();
    assert_eq!(err, JobError::Permanent(OFF_MESSAGE.into()));
    assert_eq!(
        album_state(&pool, id).await,
        Some(("failed".into(), None, Some(OFF_MESSAGE.into())))
    );
    assert!(replay.asked().is_empty());
}

/// An album that is gone fails the job permanently and writes no state.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_gone_is_permanent(pool: PgPool) {
    let (replay, ctx) = ctx(&pool, vec![]).await;
    let gone = Uuid::now_v7();
    let err = search::run_search(&ctx, &SearchJob::plain(gone), attempt(true))
        .await
        .unwrap_err();
    assert_eq!(err, JobError::Permanent("album not found".into()));
    assert_eq!(album_state(&pool, gone).await, None);
    assert!(replay.asked().is_empty());
}

#[test]
fn parse_release_ref_forms() {
    let id = mbid(R1);
    assert_eq!(parse_release_ref(R1), Some(id));
    assert_eq!(
        parse_release_ref(&format!("  {}  ", R1.to_uppercase())),
        Some(id)
    );
    for url in [
        format!("https://musicbrainz.org/release/{R1}"),
        format!("http://musicbrainz.org/release/{R1}"),
        format!("https://beta.musicbrainz.org/release/{R1}/discids"),
        format!("HTTPS://MusicBrainz.org/release/{R1}?tab=x#top"),
        format!("https://musicbrainz.org:443/release/{R1}/"),
    ] {
        assert_eq!(parse_release_ref(&url), Some(id), "{url}");
    }
    for url in [
        format!("https://example.org/release/{R1}"),
        format!("https://notmusicbrainz.org/release/{R1}"),
        format!("https://musicbrainz.org.example.org/release/{R1}"),
        format!("https://musicbrainz.org@example.org/release/{R1}"),
        format!("https://musicbrainz.org/recording/{R1}"),
        format!("https://musicbrainz.org/release-group/{RG1}"),
        format!("ftp://musicbrainz.org/release/{R1}"),
        format!("musicbrainz.org/release/{R1}"),
        "https://musicbrainz.org/release/not-an-id".to_string(),
        "https://musicbrainz.org/release/".to_string(),
        String::new(),
    ] {
        assert_eq!(parse_release_ref(&url), None, "{url}");
    }
}

#[test]
fn search_job_payload_round_trips() {
    let album = Uuid::now_v7();
    let plain = SearchJob::plain(album);
    assert_eq!(plain.payload(), json!({"kind": "search", "albumId": album}));
    let manual = SearchJob {
        album_id: album,
        query: Some(ManualQuery {
            artist: "A".into(),
            album: "B".into(),
        }),
        release_mbid: Some(mbid(R1)),
    };
    assert_eq!(
        manual.payload(),
        json!({"kind": "search", "albumId": album, "query": {"artist": "A", "album": "B"},
               "releaseMbid": R1})
    );
    assert_eq!(SearchJob::from_payload(&manual.payload()), Some(manual));
    assert_eq!(
        SearchJob::from_payload(&json!({"kind": "apply", "albumId": album})),
        None
    );
    assert_eq!(SearchJob::from_payload(&json!({"kind": "search"})), None);
}

/// A plain search is not queued twice for one album; a manual one always is.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn plain_search_deduped_manual_not(pool: PgPool) {
    let tracks = titled("Song", 2);
    let a = album(&pool, "A", "Band", false, None, &tracks).await;
    let b = album(&pool, "B", "Band", false, None, &tracks).await;
    let first = search::enqueue_search(&pool, &SearchJob::plain(a))
        .await
        .unwrap();
    assert!(first.is_some());
    assert_eq!(
        search::enqueue_search(&pool, &SearchJob::plain(a))
            .await
            .unwrap(),
        None
    );
    assert!(
        search::enqueue_search(&pool, &SearchJob::plain(b))
            .await
            .unwrap()
            .is_some(),
        "another album's search is not a duplicate"
    );
    let manual = SearchJob {
        album_id: a,
        query: None,
        release_mbid: Some(mbid(R1)),
    };
    let m1 = search::enqueue_search(&pool, &manual).await.unwrap();
    let m2 = search::enqueue_search(&pool, &manual).await.unwrap();
    assert!(m1.is_some() && m2.is_some() && m1 != m2);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE queue = 'autotag' AND state = 'queued'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 4);
    assert_eq!(
        album_state(&pool, a).await,
        Some(("queued".into(), None, None))
    );
}

/// A blank album artist is scored and searched as "Unknown Artist": no artist clause and no
/// artist penalty.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn blank_album_artist_is_unknown(pool: PgPool) {
    let tracks = titled("Fixture Song", 10);
    let id = album(&pool, "Fixture Album", "", false, None, &tracks).await;
    let q = |n| release_query("Fixture Album", ArtistClause::Any, n);
    let (replay, ctx) = ctx(
        &pool,
        vec![
            search_entry(&q(Some(10)), &[R1]),
            search_entry(&q(None), &[R1]),
        ],
    )
    .await;
    search::run_search(&ctx, &SearchJob::plain(id), attempt(false))
        .await
        .unwrap();
    assert!(
        !replay.asked()[0].contains("artist%3A"),
        "{:?}",
        replay.asked()
    );
    let cands = autotag::candidates(&pool, id).await.unwrap();
    assert_eq!(cands.len(), 1);
    assert!(
        cands[0].penalties.get("artist").is_none(),
        "{}",
        cands[0].penalties
    );
}
