//! Failure records and retry classes (spec §10.4). Ports of upstream
//! `enrichment-retry-cap`, `enrichment-per-source-pause` and
//! `enrichment-inconclusive-errors` cases.

use std::time::Duration;

use kima_db::PgPool;
use kima_db::stages::{self, Claim, Stage};
use kima_ident::stages::{
    CLAIM_LEASE, ErrorClass, MAX_ERROR_CHARS, REDELIVERY_CAP, RETRY_CAP, RETRY_DELAYS,
    STAGE_TIMEOUT, StageFailure, classify, lost_worker, record_for,
};
use kima_net::{Evidence, Failure, Outcome};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

async fn track(pool: &PgPool) -> Uuid {
    let artist = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'a', $2)")
        .bind(artist)
        .bind(artist.to_string())
        .execute(pool)
        .await
        .unwrap();
    let album = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(album)
        .bind(artist)
        .bind(album.to_string())
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint) \
         VALUES ($1, $2, 'a', 't', 'flac', $3, 1, now(), '\\x00')",
    )
    .bind(id)
    .bind(album)
    .bind(id.to_string())
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn claim_one(pool: &PgPool) -> Option<Claim> {
    stages::claim(pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .into_iter()
        .next()
}

/// Bring every parked or retry-due row of the ledger due now.
async fn make_due(pool: &PgPool) {
    sqlx::query(
        "UPDATE track_stages SET next_attempt_at = now() - interval '1 second' \
         WHERE next_attempt_at IS NOT NULL",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn row(pool: &PgPool, t: Uuid) -> sqlx::postgres::PgRow {
    sqlx::query("SELECT * FROM track_stages WHERE track_id = $1 AND stage = 'fingerprint'")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn transient(message: &str) -> StageFailure {
    StageFailure::Transient(Failure::new("NETWORK", message))
}

#[test]
fn constants_match_upstream() {
    assert_eq!(CLAIM_LEASE, Duration::from_secs(15 * 60));
    assert_eq!(REDELIVERY_CAP, 3);
    assert_eq!(RETRY_CAP, 4);
    assert_eq!(
        RETRY_DELAYS,
        [
            Duration::from_secs(60),
            Duration::from_secs(600),
            Duration::from_secs(3600)
        ]
    );
    assert_eq!(STAGE_TIMEOUT, Duration::from_secs(30));
    assert_eq!(MAX_ERROR_CHARS, 2000);
}

#[test]
fn classify_table() {
    use ErrorClass::*;
    let cases: &[(&str, ErrorClass)] = &[
        // MULTICANNEL
        ("Audio file has more than 2 audio channels", Multicannel),
        ("Audio file has more than 2 channels", Multicannel),
        ("MultiChannel layout refused", Multicannel),
        ("multichannel stream truncated", Multicannel),
        // UNSUPPORTED
        ("Unsupported codec!", Unsupported),
        ("decoder is not capable of decoding this", Unsupported),
        ("Unknown demuxer 'xyz'", Unsupported),
        ("Decoder (codec id 86) not found", Unsupported),
        ("codec opus not found", Unsupported),
        // DAMAGED
        ("stream truncated at byte 12", Damaged),
        ("CRC mismatch", Damaged),
        ("flac: decode_frame overrun", Damaged),
        ("packet overhead", Damaged),
        ("missing keyframe", Damaged),
        ("non-monotonous dts", Damaged),
        ("nonmonotonic dts", Inconclusive),
        ("non monotonoud DTS", Damaged),
        ("Error while decoding stream #0:0", Damaged),
        ("cannot decode", Damaged),
        ("could not decode", Damaged),
        ("failed to decode", Damaged),
        ("stretched frame", Damaged),
        ("invalid start code", Damaged),
        ("invalid end", Damaged),
        ("invalid mp3 frame", Damaged),
        ("corrupted bitstream", Damaged),
        ("corruption data", Damaged),
        ("corrupt file", Damaged),
        ("malformed stream", Damaged),
        ("decode frame failed", Damaged),
        ("mp3: Header missing", Damaged),
        ("Invalid DTS", Damaged),
        // INCONCLUSIVE
        ("something went wrong", Inconclusive),
        ("Redis connection dropped mid-scan", Inconclusive),
        ("", Inconclusive),
        ("the file is unsupported", Inconclusive),
        ("checking flac decode support", Inconclusive),
        ("HTTP 429 Too Many Requests", Inconclusive),
        ("acrc32 checks", Inconclusive),
        ("header missingno", Inconclusive),
    ];
    for (msg, want) in cases {
        assert_eq!(classify(msg), *want, "{msg:?}");
    }
    // Precedence: MULTICANNEL > UNSUPPORTED > DAMAGED.
    assert_eq!(
        classify("unsupported codec, more than 2 channels"),
        Multicannel
    );
    assert_eq!(classify("truncated: unsupported codec"), Unsupported);
}

#[test]
fn class_names() {
    let all = [
        (ErrorClass::Damaged, "DAMAGED"),
        (ErrorClass::Unsupported, "UNSUPPORTED"),
        (ErrorClass::Multicannel, "MULTICANNEL"),
        (ErrorClass::Inconclusive, "INCONCLUSIVE"),
        (ErrorClass::Permanent, "PERMANENT"),
        (ErrorClass::FileMissing, "FILE_MISSING"),
        (ErrorClass::DependencyClosed, "DEPENDENCY_CLOSED"),
    ];
    for (c, s) in all {
        assert_eq!(c.as_str(), s);
    }
}

#[test]
fn classify_is_linear_on_hostile_input() {
    // Long runs that would make a backtracking engine blow up on `decoder\s+.*not\s+found`.
    let hostile = format!("decoder {}", " not".repeat(200_000));
    let started = std::time::Instant::now();
    assert_eq!(classify(&hostile), ErrorClass::Inconclusive);
    let odd = "\u{0}\u{FFFF}é".repeat(50_000);
    assert_eq!(classify(&odd), ErrorClass::Inconclusive);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn from_outcome_maps_failures_only() {
    let until = OffsetDateTime::now_utc();
    assert!(StageFailure::from_outcome(Outcome::Answered(1)).is_none());
    assert!(
        StageFailure::from_outcome::<()>(Outcome::Absent(Evidence(serde_json::json!({}))))
            .is_none()
    );
    assert!(matches!(
        StageFailure::from_outcome::<()>(Outcome::RateLimited { until }),
        Some(StageFailure::RateLimited { until: u }) if u == until
    ));
    let f = Failure::new("HTTP_400", "bad");
    assert!(matches!(
        StageFailure::from_outcome::<()>(Outcome::Permanent(f.clone())),
        Some(StageFailure::Permanent(g)) if g == f
    ));
    assert!(matches!(
        StageFailure::from_outcome::<()>(Outcome::Transient(f.clone())),
        Some(StageFailure::Transient(g)) if g == f
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rate_limited_parks_without_attempt(pool: PgPool) {
    let t = track(&pool).await;
    let now = OffsetDateTime::now_utc();
    let until = now + Duration::from_secs(120);
    for _ in 0..6 {
        let c = claim_one(&pool).await.unwrap();
        let rec = record_for(&c, &StageFailure::RateLimited { until }, now);
        assert_eq!(rec.pending_until, Some(until));
        assert!(!rec.count_attempt);
        assert_eq!(rec.code, "RATE_LIMITED");
        assert_eq!(rec.class, None);
        assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
        let r = row(&pool, t).await;
        assert_eq!(r.get::<String, _>("status"), "pending");
        assert_eq!(r.get::<i16, _>("attempts"), 0);
        // Not offered while parked.
        assert!(claim_one(&pool).await.is_none());
        make_due(&pool).await;
    }
    // Four reported failures still close it: the pauses used up nothing.
    for i in 0..4 {
        let c = claim_one(&pool).await.unwrap();
        assert_eq!(c.attempts, i);
        let rec = record_for(&c, &transient("socket hang up"), now);
        assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
        make_due(&pool).await;
    }
    assert!(claim_one(&pool).await.is_none());
    let r = row(&pool, t).await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn permanent_is_terminal(pool: PgPool) {
    let t = track(&pool).await;
    let c = claim_one(&pool).await.unwrap();
    let f = StageFailure::Permanent(Failure::new("NO_CLIENT_KEY", "no key"));
    let rec = record_for(&c, &f, OffsetDateTime::now_utc());
    assert_eq!(rec.retry_at, None);
    assert_eq!(rec.pending_until, None);
    assert!(rec.count_attempt);
    assert_eq!(rec.code, "NO_CLIENT_KEY");
    assert_eq!(rec.class.as_deref(), Some("PERMANENT"));
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t).await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert_eq!(r.get::<i16, _>("attempts"), 1);
    assert_eq!(
        r.get::<Option<String>, _>("error_code").as_deref(),
        Some("NO_CLIENT_KEY")
    );
    assert_eq!(
        r.get::<Option<String>, _>("last_error").as_deref(),
        Some("no key")
    );
    assert!(claim_one(&pool).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn inconclusive_retries_1m_10m_1h_then_terminal(pool: PgPool) {
    let t = track(&pool).await;
    let now = OffsetDateTime::now_utc();
    let waits = [60, 600, 3600];
    for (i, wait) in waits.iter().enumerate() {
        let c = claim_one(&pool).await.unwrap();
        assert_eq!(c.attempts, i as i16);
        let rec = record_for(&c, &transient("Redis connection dropped"), now);
        assert_eq!(rec.class.as_deref(), Some("INCONCLUSIVE"));
        assert_eq!(rec.code, "NETWORK");
        assert_eq!(rec.retry_at, Some(now + Duration::from_secs(*wait)));
        assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
        let r = row(&pool, t).await;
        assert_eq!(r.get::<String, _>("status"), "failed");
        assert!(
            r.get::<Option<OffsetDateTime>, _>("next_attempt_at")
                .is_some()
        );
        // Not offered before its retry is due, offered after.
        assert!(claim_one(&pool).await.is_none());
        make_due(&pool).await;
    }
    let c = claim_one(&pool).await.unwrap();
    assert_eq!(c.attempts, 3);
    let rec = record_for(&c, &transient("Redis connection dropped"), now);
    assert_eq!(rec.retry_at, None);
    assert_eq!(rec.class.as_deref(), Some("INCONCLUSIVE"));
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t).await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert_eq!(r.get::<i16, _>("attempts"), 4);
    assert!(claim_one(&pool).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn damaged_terminal_at_once(pool: PgPool) {
    let t = track(&pool).await;
    let c = claim_one(&pool).await.unwrap();
    let rec = record_for(
        &c,
        &transient("flac: decode_frame overrun"),
        OffsetDateTime::now_utc(),
    );
    assert_eq!(rec.retry_at, None);
    assert_eq!(rec.class.as_deref(), Some("DAMAGED"));
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t).await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert_eq!(
        r.get::<Option<String>, _>("error_class").as_deref(),
        Some("DAMAGED")
    );
    assert_eq!(
        r.get::<Option<String>, _>("last_error").as_deref(),
        Some("flac: decode_frame overrun")
    );
    assert!(claim_one(&pool).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_missing_terminal(pool: PgPool) {
    let t = track(&pool).await;
    let c = claim_one(&pool).await.unwrap();
    let f = StageFailure::FileMissing("No such file or directory".into());
    let rec = record_for(&c, &f, OffsetDateTime::now_utc());
    assert_eq!(rec.retry_at, None);
    assert_eq!(rec.pending_until, None);
    assert_eq!(rec.code, "FILE_MISSING");
    assert_eq!(rec.class.as_deref(), Some("FILE_MISSING"));
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t).await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert!(claim_one(&pool).await.is_none());
    // A scan that finds the file again reopens it.
    let mut conn = pool.acquire().await.unwrap();
    stages::reopen_file_missing(&mut conn, t).await.unwrap();
    assert!(claim_one(&pool).await.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redelivery_cap_closes_lost_worker(pool: PgPool) {
    let t = track(&pool).await;
    let mut c = claim_one(&pool).await.unwrap();
    while c.redelivered < REDELIVERY_CAP {
        // The worker vanished: the lease lapses and the claim is offered again.
        sqlx::query("UPDATE track_stages SET next_attempt_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        c = claim_one(&pool).await.unwrap();
    }
    assert_eq!(c.redelivered, 3);
    let rec = lost_worker(&c);
    assert_eq!(rec.code, "LOST_WORKER");
    assert_eq!(rec.class.as_deref(), Some("INCONCLUSIVE"));
    assert!(rec.count_attempt);
    assert_eq!(rec.retry_at, None);
    assert_eq!(rec.pending_until, None);
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t).await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert_eq!(r.get::<i16, _>("attempts"), 1);
    assert!(claim_one(&pool).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn last_error_cut_at_2000(pool: PgPool) {
    let t = track(&pool).await;
    let c = claim_one(&pool).await.unwrap();
    // 4-byte characters: a byte cut would land inside one.
    let long = "𝄞".repeat(3000);
    let rec = record_for(&c, &transient(&long), OffsetDateTime::now_utc());
    assert_eq!(rec.message.chars().count(), MAX_ERROR_CHARS);
    assert_eq!(rec.message, "𝄞".repeat(MAX_ERROR_CHARS));
    let rec = record_for(
        &c,
        &StageFailure::FileMissing("x".repeat(5000)),
        OffsetDateTime::now_utc(),
    );
    assert_eq!(rec.message.len(), MAX_ERROR_CHARS);
    let rec = record_for(&c, &transient(&long), OffsetDateTime::now_utc());
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t).await;
    assert_eq!(
        r.get::<Option<String>, _>("last_error")
            .unwrap()
            .chars()
            .count(),
        2000
    );
    // A short message is kept whole.
    let rec = record_for(&c, &transient("short"), OffsetDateTime::now_utc());
    assert_eq!(rec.message, "short");
}
