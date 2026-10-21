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
    self, AcoustidRecording, AcoustidResult, DEFAULT_CLIENT_KEY, MIN_SCORE, Picked,
};
use kima_net::testing::{Entry, Recording, Replay, body_sha256};
use kima_net::{Bases, Net, NetConfig, Outcome};
use serde_json::{Value, json};
use sqlx::PgPool;

const RECORDING: &str = "8f6a1a2b-3c4d-5e6f-7a8b-9c0d1e2f3a4b";
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
async fn the_key_never_reaches_the_cache_or_a_failure(pool: PgPool) {
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

    // Answered, and cached under a key without the secret.
    picked(acoustid::identify(&net, secret, "fp", 200, None).await);
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("SELECT key, source, request, body::text FROM http_cache")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    for (key, source, request, body) in &rows {
        assert_eq!(source, "acoustid");
        for text in [key, request, body] {
            assert!(!text.contains(secret) && !text.contains(encoded), "{text}");
            assert!(!text.contains("client="), "{text}");
        }
        // The cache key is the source and a hash; the request names the body by its hash.
        assert_eq!(key.len(), "acoustid:".len() + 64);
        assert!(request.starts_with("POST "), "{request}");
        assert_eq!(request.split('\n').nth(1).map(str::len), Some(64));
    }

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
async fn answers_are_cached_by_fingerprint_and_duration_not_key(pool: PgPool) {
    let (replay, net) = net_for(&pool, fixture("confident")).await;
    let first = picked(acoustid::identify(&net, "key-one", "fp", 200, None).await);
    // Another key, same question: served from the cache.
    let second = picked(acoustid::identify(&net, "key-two", "fp", 200, None).await);
    assert_eq!(first, second);
    assert_eq!(replay.asked().len(), 1);
    // Another fingerprint is another question.
    picked(acoustid::identify(&net, "key-one", "fp2", 200, None).await);
    assert_eq!(replay.asked().len(), 2);
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
