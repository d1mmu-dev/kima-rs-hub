//! The AcoustID client against the replay server: a port of upstream's `acoustidService.test.ts`
//! (16 cases) with kima-rs's key order (setting, env, empty default), plus the secret-handling and
//! untrusted-answer cases. No kima-rs key exists to record with (spec §27 Q1), so every answer is
//! hand-written from the upstream test bodies (`tests/fixtures/acoustid`). Nothing here reaches
//! api.acoustid.org.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use kima_core::cipher::Cipher;
use kima_core::identify::IdentifySwitch;
use kima_db::settings_kv::{self, Key, KvSettings, SettingsCache};
use kima_ident::Mbid;
use kima_ident::acoustid::{
    self, AcoustidGroup, AcoustidRecording, AcoustidResult, DEFAULT_CLIENT_KEY, MIN_SCORE, Picked,
};
use kima_net::testing::{Entry, Recording, Replay, body_sha256};
use kima_net::{Bases, Net, NetConfig, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;

const RECORDING: &str = "8f6a1a2b-3c4d-5e6f-7a8b-9c0d1e2f3a4b";
const ALBUM: &str = "0e5df449-0e51-4f4f-9a33-6a17e7c6b717";
const DAY_6: &str = "00000000-0000-4000-8000-000000000006";
const DAY_2: &str = "00000000-0000-4000-8000-000000000002";
const LIVE: &str = "00000000-0000-4000-8000-00000000000b";
const STUDIO: &str = "00000000-0000-4000-8000-00000000000c";

/// A key the tests send; never a real one.
const KEY: &str = "test-key";

fn fixture(name: &str) -> Vec<Entry> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acoustid")
        .join(format!("{name}.json"));
    Recording::load(path).0
}

fn post(status: u16, body: Value, body_sha: Option<String>) -> Entry {
    Entry {
        method: "POST".into(),
        url: "/lookup".into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: body_sha,
    }
}

/// The exact body for `key`, already percent-encoded, a 200 s track and fingerprint `fp`.
fn body_for(key_encoded: &str, secs: i32, fp: &str) -> String {
    format!("client={key_encoded}&duration={secs}&fingerprint={fp}&meta=recordings%20releasegroups")
}

/// An empty answer served only to the request whose body is exactly `body`.
fn no_results_for(body: &str) -> Entry {
    post(
        200,
        json!({"status": "ok", "results": []}),
        Some(body_sha256(body)),
    )
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

fn cipher() -> Arc<Cipher> {
    Arc::new(Cipher::from_secret("kima-ident acoustid tests"))
}

fn picked(out: Outcome<Picked>) -> Picked {
    match out {
        Outcome::Answered(p) => p,
        other => panic!("expected answered, got {other:?}"),
    }
}

// ---- acoustidService.lookup ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_a_confident_match_with_the_full_result_shape(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("confident")).await;
    let p = picked(acoustid::identify(&net, KEY, "aqA_ fingerprint", 200, None).await);
    // kima-rs keeps the recording only; artist and release group are not written (spec §8.4).
    assert_eq!(
        p,
        Picked {
            recording: mbid(RECORDING),
            score: 0.92,
            acoustid_id: RECORDING.into(),
            title_agrees: false,
            candidates: vec![mbid(RECORDING)],
            release_groups: vec![AcoustidGroup {
                id: mbid(ALBUM),
                title: "Album".into(),
            }],
        }
    );
    assert_eq!(
        p.evidence(),
        json!({"score": 0.92, "acoustidId": RECORDING})
    );

    // The lookup itself answers every result, parsed.
    let results = match acoustid::lookup(&net, KEY, "aqA_ fingerprint", 200).await {
        Outcome::Answered(r) => r,
        other => panic!("expected answered, got {other:?}"),
    };
    assert_eq!(
        results,
        vec![AcoustidResult {
            id: RECORDING.into(),
            score: 0.92,
            recordings: Some(vec![AcoustidRecording {
                id: mbid(RECORDING),
                title: Some("Song".into()),
                duration: Some(200.0),
                releasegroups: vec![AcoustidGroup {
                    id: mbid(ALBUM),
                    title: "Album".into(),
                }],
            }]),
        }]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn skips_results_below_the_score_floor_and_takes_the_next(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("below-floor")).await;
    let p = picked(acoustid::identify(&net, KEY, "fp", 200, None).await);
    assert_eq!(p.recording, mbid(RECORDING));
    assert_eq!(p.score, 0.7);
    assert_eq!(MIN_SCORE, 0.5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_absent_when_no_recording_has_the_tracks_length(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("wrong-length")).await;
    match acoustid::identify(&net, KEY, "fp", 200, None).await {
        Outcome::Absent(e) => assert_eq!(
            e.0,
            json!({
                "askedDuration": 200,
                "topScore": 1.0,
                "reason": "no result above the confidence floor has a recording of this track's length",
            })
        ),
        other => panic!("expected absent, got {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_a_short_track_with_a_recording_of_its_own_length(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("short-track")).await;
    let p = picked(acoustid::identify(&net, KEY, "fp", 22, None).await);
    assert_eq!(p.recording, mbid(RECORDING));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn picks_the_recording_of_the_tracks_length_not_the_first_one_listed(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("cluster-lengths")).await;
    let p = picked(acoustid::identify(&net, KEY, "fp", 515, None).await);
    assert_eq!(p.recording, mbid(DAY_2));
    assert_eq!(p.acoustid_id, "cluster");
    assert_ne!(p.recording, mbid(DAY_6));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn among_recordings_of_the_right_length_takes_the_one_with_the_tracks_title_and_version(
    pool: PgPool,
) {
    let (_replay, net) = net_for(&pool, fixture("cluster-versions")).await;
    let p = picked(acoustid::identify(&net, KEY, "fp", 200, Some("Song")).await);
    assert_eq!(p.recording, mbid(STUDIO));
    // Without a title the closest length wins: the live take at exactly 200 s.
    let p = picked(acoustid::identify(&net, KEY, "fp", 200, None).await);
    assert_eq!(p.recording, mbid(LIVE));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn skips_a_bare_result_with_no_recordings_array(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("bare-result")).await;
    assert!(matches!(
        acoustid::identify(&net, KEY, "fp", 200, None).await,
        Outcome::Absent(_)
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_transient_on_network_error(pool: PgPool) {
    // Nobody listens on port 9: the connection is refused.
    let net = Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool.clone(),
        IdentifySwitch::new(false, true),
    );
    let out = acoustid::identify(&net, KEY, "fp", 200, None).await;
    assert!(matches!(out, Outcome::Transient(_)), "{out:?}");
    assert_eq!(out.code(), Some("NETWORK"), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_transient_when_the_api_reports_failure_status(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture("error-status")).await;
    let out = acoustid::identify(&net, KEY, "fp", 200, None).await;
    assert_eq!(out.code(), Some("BAD_RESPONSE"), "{out:?}");
    assert!(matches!(out, Outcome::Transient(_)));
    // A failure is never cached: the next call asks again.
    let again = acoustid::identify(&net, KEY, "fp", 200, None).await;
    assert_eq!(again.code(), Some("BAD_RESPONSE"));
    assert_eq!(replay.asked().len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn answers_absent_when_there_are_no_results(pool: PgPool) {
    let (_replay, net) = net_for(&pool, fixture("no-results")).await;
    assert!(matches!(
        acoustid::lookup(&net, KEY, "fp", 200).await,
        Outcome::Absent(_)
    ));
    match acoustid::identify(&net, KEY, "fp", 200, None).await {
        Outcome::Absent(e) => {
            assert_eq!(e.0["askedDuration"], json!(200));
            assert_eq!(e.0["topScore"], Value::Null);
        }
        other => panic!("expected absent, got {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn short_circuits_to_permanent_on_empty_fingerprint_or_bad_duration_without_any_http_call(
    pool: PgPool,
) {
    let (replay, net) = net_for(&pool, fixture("confident")).await;
    for (fp, secs) in [
        ("", 200),
        ("   ", 200),
        ("fp", 0),
        ("fp", -5),
        ("fp", i32::MIN),
    ] {
        let out = acoustid::lookup(&net, KEY, fp, secs).await;
        assert_eq!(out.code(), Some("NO_FINGERPRINT"), "{fp:?} {secs}");
        assert!(matches!(out, Outcome::Permanent(_)));
        let out = acoustid::identify(&net, KEY, fp, secs, Some("Song")).await;
        assert_eq!(out.code(), Some("NO_FINGERPRINT"));
    }
    assert!(replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sends_the_live_verified_request_shape(pool: PgPool) {
    // The key is percent-encoded once; the base64url fingerprint has nothing to encode, so it
    // goes as it is; `meta`'s space is `%20`, never `+`.
    let key = "tk/ey+1 x";
    let fp = "AQAA-_fp123";
    let body = "client=tk%2Fey%2B1%20x&duration=123&fingerprint=AQAA-_fp123\
                &meta=recordings%20releasegroups";
    let (replay, net) = net_for(&pool, vec![no_results_for(body)]).await;
    let out = acoustid::lookup(&net, key, fp, 123).await;
    // Only a request with exactly this body is answered; anything else gets the replay's 599.
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert_eq!(replay.asked(), vec!["POST /lookup".to_string()]);
    assert_eq!(
        replay.content_types(),
        vec!["application/x-www-form-urlencoded".to_string()]
    );
    assert!(!body.contains("track="));
    assert!(!body.contains("recordings%2Breleasegroups"));
}

// ---- key precedence (kima-rs: setting, env, empty default) ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sends_the_stored_settings_key(pool: PgPool) {
    let cipher = cipher();
    settings_kv::set_secret(&pool, &cipher, Key::AcoustidApiKey, Some("operator-key"))
        .await
        .unwrap();
    let settings = SettingsCache::new(pool.clone(), cipher);
    // The setting is read (it beats the env key).
    let key = acoustid::resolve_client_key(&settings, Some("env-key"))
        .await
        .unwrap();
    assert_eq!(key, "operator-key");

    let (replay, net) = net_for(
        &pool,
        vec![no_results_for(&body_for("operator-key", 200, "fp"))],
    )
    .await;
    assert!(matches!(
        acoustid::lookup(&net, &key, "fp", 200).await,
        Outcome::Absent(_)
    ));
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn env_key_when_no_setting(pool: PgPool) {
    let settings = SettingsCache::new(pool.clone(), cipher());
    let key = acoustid::resolve_client_key(&settings, Some("env-key"))
        .await
        .unwrap();
    assert_eq!(key, "env-key");
    // A stored setting beats it, and an empty one does not.
    let with = |k: Option<&str>| KvSettings {
        acoustid_api_key: k.map(str::to_string),
        ..KvSettings::default()
    };
    assert_eq!(
        acoustid::client_key(&with(Some("operator-key")), Some("env-key")).as_deref(),
        Some("operator-key")
    );
    assert_eq!(
        acoustid::client_key(&with(Some("")), Some("env-key")).as_deref(),
        Some("env-key")
    );

    let (replay, net) = net_for(&pool, vec![no_results_for(&body_for("env-key", 200, "fp"))]).await;
    assert!(matches!(
        acoustid::lookup(&net, &key, "fp", 200).await,
        Outcome::Absent(_)
    ));
    assert_eq!(replay.asked().len(), 1);
}

#[test]
fn empty_default_means_no_key() {
    // kima-rs does not reuse upstream's registered key (spec §27 Q1, D-124).
    assert_eq!(DEFAULT_CLIENT_KEY, "");
    let none = KvSettings::default();
    assert_eq!(acoustid::client_key(&none, None), None);
    assert_eq!(acoustid::client_key(&none, Some("")), None);
    assert_eq!(acoustid::client_key(&none, Some("   ")), None);
    let blank = KvSettings {
        acoustid_api_key: Some(" ".into()),
        ..KvSettings::default()
    };
    assert_eq!(acoustid::client_key(&blank, None), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn settings_read_failure_falls_back_to_env(pool: PgPool) {
    // A pool that is closed: every settings read fails.
    let broken = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    broken.close().await;
    let settings = SettingsCache::new(broken, cipher());
    assert!(settings.get().await.is_err());
    let key = acoustid::resolve_client_key(&settings, Some("env-key"))
        .await
        .unwrap();
    assert_eq!(key, "env-key");
    assert_eq!(acoustid::resolve_client_key(&settings, None).await, None);

    // Never propagates: the question is asked with the env key.
    let (replay, net) = net_for(&pool, vec![no_results_for(&body_for("env-key", 200, "fp"))]).await;
    assert!(matches!(
        acoustid::lookup(&net, &key, "fp", 200).await,
        Outcome::Absent(_)
    ));
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_key_is_permanent_no_client_key_without_request(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture("confident")).await;
    for key in ["", "  "] {
        let out = acoustid::lookup(&net, key, "fp", 200).await;
        assert_eq!(out.code(), Some("NO_CLIENT_KEY"));
        assert!(matches!(out, Outcome::Permanent(_)));
        // The key is checked before the fingerprint, as upstream.
        let out = acoustid::identify(&net, key, "", 0, None).await;
        assert_eq!(out.code(), Some("NO_CLIENT_KEY"));
    }
    assert!(replay.asked().is_empty());
}

// ---- kima-rs additions: secrets, cache, untrusted answers ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_key_never_reaches_a_failure_or_a_request_line(pool: PgPool) {
    let secret = "s3cret/KEY+value";
    let encoded = "s3cret%2FKEY%2Bvalue";
    let confident = fixture("confident").remove(0).body;
    let echo = json!({"status": "error", "error": {"message": format!(
        "invalid API key {secret} ({encoded})"
    )}});
    let (replay, net) = net_for(
        &pool,
        vec![
            post(
                200,
                confident,
                Some(body_sha256(&body_for(encoded, 200, "fp"))),
            ),
            post(
                400,
                echo,
                Some(body_sha256(&body_for(encoded, 200, "other"))),
            ),
        ],
    )
    .await;

    // Answered; nothing is kept in the response cache (replies are saved per fingerprint in
    // `acoustid_replies`, which never holds a key either: see acoustid_batch.rs).
    picked(acoustid::identify(&net, secret, "fp", 200, None).await);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM http_cache")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    // A failure whose answer echoes the key carries it nowhere.
    let out = acoustid::identify(&net, secret, "other", 200, None).await;
    match &out {
        Outcome::Permanent(f) | Outcome::Transient(f) => {
            assert!(!f.message.contains(secret), "{}", f.message);
            assert!(!f.message.contains(encoded), "{}", f.message);
            // The echo did reach the message, replaced.
            assert_eq!(f.code, "HTTP_400");
            assert!(f.message.contains("REDACTED"), "{}", f.message);
            assert!(!format!("{out:?}").contains(secret));
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    for asked in replay.asked() {
        assert!(
            !asked.contains(secret) && !asked.contains(encoded),
            "{asked}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_lookup_is_never_kept_in_the_response_cache(pool: PgPool) {
    // M-SYNC Task 12: AcoustID replies live in `acoustid_replies` (30 d / 7 d / 1 d), so Net's
    // `http_cache` keeps none of them, and the same question is asked again at this layer.
    let (replay, net) = net_for(&pool, fixture("confident")).await;
    picked(acoustid::identify(&net, "key-one", "fp", 200, None).await);
    picked(acoustid::identify(&net, "key-two", "fp", 200, None).await);
    assert_eq!(replay.asked().len(), 2);
    let cached: i64 =
        sqlx::query_scalar("SELECT count(*) FROM http_cache WHERE source = 'acoustid'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(cached, 0);
}

fn result(v: Value) -> AcoustidResult {
    serde_json::from_value(v).unwrap()
}

#[test]
fn odd_fields_never_panic_and_never_pick() {
    let rec = json!({"id": RECORDING, "title": "Song", "duration": 200});
    let odd = vec![
        result(json!({"id": "a", "score": "high", "recordings": [rec]})),
        result(json!({"id": "b", "score": -1.0, "recordings": [rec]})),
        result(json!({"id": "c", "score": null, "recordings": [rec]})),
        result(json!({"id": "d", "recordings": [rec]})),
        result(json!({"id": "e", "score": 0.9, "recordings": null})),
        result(json!({"id": "f", "score": 0.9, "recordings": []})),
        result(json!({"id": "g", "score": 0.9, "recordings": "x"})),
        result(json!({"id": "h", "score": 0.9, "recordings": [{"id": "not-an-mbid"}, 7, null]})),
        result(json!({"score": 0.49999, "recordings": [rec]})),
        result(json!({"id": ["x"], "score": 0.9, "recordings": [
            {"id": RECORDING, "duration": 9999}
        ]})),
    ];
    assert!(odd[0].score.is_nan() && odd[2].score.is_nan() && odd[3].score.is_nan());
    assert_eq!(odd[4].recordings, None);
    assert_eq!(odd[6].recordings, None);
    assert_eq!(odd[7].recordings, Some(vec![]));
    assert_eq!(odd[9].id, "");
    let e = acoustid::pick_fingerprint_match(&odd, 200, Some("Song")).unwrap_err();
    // A non-number top score reads as null.
    assert_eq!(e.0["topScore"], Value::Null);
    assert_eq!(e.0["askedDuration"], json!(200));
    assert!(acoustid::pick_fingerprint_match(&[], 200, None).is_err());
    assert!(acoustid::pick_fingerprint_match(&odd, i32::MAX, None).is_err());
    // A duration that is not positive picks nothing (`lookup` never asks with one).
    assert!(acoustid::pick_fingerprint_match(&odd, 0, None).is_err());
    assert!(acoustid::pick_fingerprint_match(&odd, i32::MIN, None).is_err());
}

#[test]
fn unknown_lengths_fit_and_lose_to_known_ones() {
    // A recording with no (or a junk) duration is kept (`lengths_agree` is unknown) but sorts
    // after one with a known length; a score above 1 is still above the floor.
    let results = vec![result(json!({"id": "x", "score": 1.7, "recordings": [
        {"id": LIVE, "title": "Song", "duration": null},
        {"id": DAY_6, "title": "Song", "duration": "long"},
        {"id": DAY_2, "title": "Song", "duration": -3e300},
        {"id": STUDIO, "title": "Song", "duration": 203.5},
    ]}))];
    let p = acoustid::pick_fingerprint_match(&results, 200, None).unwrap();
    assert_eq!(p.recording, mbid(STUDIO));
    assert_eq!(p.score, 1.7);
    assert_eq!(results[0].recordings.as_ref().unwrap()[2].duration, None);
    let only_unknown = vec![result(json!({"id": "y", "score": 0.6, "recordings": [
        {"id": LIVE, "duration": null}, {"id": DAY_6}
    ]}))];
    // Equal gaps: the first listed wins.
    let p = acoustid::pick_fingerprint_match(&only_unknown, 200, Some("Song")).unwrap();
    assert_eq!(p.recording, mbid(LIVE));
}

#[test]
fn a_title_that_agrees_nowhere_falls_back_to_every_fitting_recording() {
    let results = vec![result(json!({"id": "x", "score": 0.8, "recordings": [
        {"id": LIVE, "title": "Other (Live)", "duration": 202},
        {"id": STUDIO, "title": "Other", "duration": 199},
    ]}))];
    let p = acoustid::pick_fingerprint_match(&results, 200, Some("Song")).unwrap();
    assert_eq!(p.recording, mbid(STUDIO));
    // An empty title is no title.
    let p = acoustid::pick_fingerprint_match(&results, 200, Some("")).unwrap();
    assert_eq!(p.recording, mbid(STUDIO));
}

// ---- the pick, M-SYNC Task 12 (acoustidService.pickFingerprintMatch, toResult) ----

#[test]
fn the_pick_says_whether_the_title_agrees_and_lists_the_candidates_best_first() {
    let results = vec![result(json!({"id": "x", "score": 0.8, "recordings": [
        {"id": LIVE, "title": "Song", "duration": 203},
        {"id": STUDIO, "title": "Song", "duration": 200},
        {"id": DAY_2, "title": "Other", "duration": 200},
    ]}))];
    let p = acoustid::pick_fingerprint_match(&results, 200, Some("Song")).unwrap();
    assert!(p.title_agrees);
    // Only the title-agreeing recordings, the closest length first.
    assert_eq!(p.candidates, vec![mbid(STUDIO), mbid(LIVE)]);
    assert_eq!(p.recording, mbid(STUDIO));

    // No recording with the track's title: the pick has only the length, and every fitting
    // recording is a candidate.
    let p = acoustid::pick_fingerprint_match(&results, 200, Some("Nothing like it")).unwrap();
    assert!(!p.title_agrees);
    assert_eq!(p.candidates, vec![mbid(STUDIO), mbid(DAY_2), mbid(LIVE)]);
}

#[test]
fn when_the_title_agrees_the_albums_are_the_union_over_the_candidates() {
    let g = |n: u32| json!({"id": format!("00000000-0000-4000-8000-0000000000{n:02}"), "title": format!("Album {n}")});
    let results = vec![result(json!({"id": "x", "score": 0.9, "recordings": [
        {"id": STUDIO, "title": "Song", "duration": 200, "releasegroups": [g(1), g(2)]},
        {"id": LIVE, "title": "Song", "duration": 201, "releasegroups": [g(2), g(3)]},
        {"id": DAY_2, "title": "Other", "duration": 200, "releasegroups": [g(9)]},
    ]}))];
    let ids =
        |p: &Picked| -> Vec<String> { p.release_groups.iter().map(|g| g.title.clone()).collect() };
    let agreeing = acoustid::pick_fingerprint_match(&results, 200, Some("Song")).unwrap();
    assert_eq!(ids(&agreeing), ["Album 1", "Album 2", "Album 3"]);
    // A length-only pick lists only its own recording's albums.
    let by_length = acoustid::pick_fingerprint_match(&results, 200, Some("Unrelated")).unwrap();
    assert!(!by_length.title_agrees);
    assert_eq!(by_length.recording, mbid(STUDIO));
    assert_eq!(ids(&by_length), ["Album 1", "Album 2"]);
}

#[test]
fn a_result_survives_being_saved_and_read_back() {
    let r = result(json!({"id": "x", "score": 0.9, "recordings": [
        {"id": STUDIO, "title": "Song", "duration": 200.5,
         "releasegroups": [{"id": ALBUM, "title": "Album"}, {"id": "nope"}]},
    ]}));
    let back: Vec<AcoustidResult> =
        serde_json::from_value(serde_json::to_value([&r]).unwrap()).unwrap();
    assert_eq!(back, vec![r]);
    // A score that was not a number comes back as one that is not a number.
    let nan = result(json!({"id": "y", "recordings": []}));
    let back: Vec<AcoustidResult> =
        serde_json::from_value(serde_json::to_value([&nan]).unwrap()).unwrap();
    assert!(back[0].score.is_nan());
}

// ---- lookup_batch ----

fn batch_body(key: &str, qs: &[(&str, i32)]) -> String {
    let mut s = format!("client={key}&meta=recordings%20releasegroups&batch=1");
    for (i, (fp, secs)) in qs.iter().enumerate() {
        s.push_str(&format!("&duration.{i}={secs}&fingerprint.{i}={fp}"));
    }
    s
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_batch_request_has_the_upstream_shape_and_maps_each_index_back(pool: PgPool) {
    let qs = [("fpA", 200), ("fpB", 201), ("fpC", 202)];
    let (replay, net) = net_for(
        &pool,
        vec![post(
            200,
            json!({"status": "ok", "fingerprints": [
                {"index": 2, "results": [{"id": "r2", "score": 0.9, "recordings": [
                    {"id": STUDIO, "title": "Song", "duration": 202}]}]},
                {"index": 0, "results": []},
                {"index": 9, "results": [{"id": "stray", "score": 1.0}]},
            ]}),
            Some(body_sha256(&batch_body(KEY, &qs))),
        )],
    )
    .await;
    let out = acoustid::lookup_batch(&net, KEY, &qs).await;
    let got = match out {
        Outcome::Answered(g) => g,
        other => panic!("expected answered, got {other:?}"),
    };
    // Index 1 is absent (the server dropped it); index 9 names no question and is ignored.
    assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![2, 0]);
    assert_eq!(got[0].1[0].id, "r2");
    assert!(got[1].1.is_empty());
    assert_eq!(replay.asked(), vec!["POST /lookup".to_string()]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_batch_reply_without_a_fingerprints_list_is_a_bad_response(pool: PgPool) {
    let (_replay, net) = net_for(&pool, vec![post(200, json!({"status": "ok"}), None)]).await;
    let out = acoustid::lookup_batch(&net, KEY, &[("fpA", 200)]).await;
    assert_eq!(out.code(), Some("BAD_RESPONSE"), "{out:?}");
    let (_replay, net) = net_for(
        &pool,
        vec![post(
            200,
            json!({"status": "error", "fingerprints": []}),
            None,
        )],
    )
    .await;
    let out = acoustid::lookup_batch(&net, KEY, &[("fpA", 200)]).await;
    assert_eq!(out.code(), Some("BAD_RESPONSE"), "{out:?}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_batch_needs_a_key_and_holds_at_most_ten_questions(pool: PgPool) {
    let (replay, net) = net_for(&pool, vec![]).await;
    let out = acoustid::lookup_batch(&net, " ", &[("fpA", 200)]).await;
    assert_eq!(out.code(), Some("NO_CLIENT_KEY"));
    let eleven: Vec<(&str, i32)> = (0..11).map(|_| ("fp", 200)).collect();
    let out = acoustid::lookup_batch(&net, KEY, &eleven).await;
    assert_eq!(out.code(), Some("BATCH_TOO_LARGE"));
    assert_eq!(
        acoustid::lookup_batch(&net, KEY, &[]).await,
        Outcome::Answered(vec![])
    );
    assert!(replay.asked().is_empty());
}
