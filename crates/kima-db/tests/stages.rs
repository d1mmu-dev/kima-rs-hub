//! The `track_stages` ledger as a work queue (spec §10.2-§10.3). Ports of upstream
//! `getNextWork`, `stageGating`, `unifiedEnrichment.fingerprintGate`,
//! `enrichment-stale-reports` and `enrichment-retry-cap` cases.

use std::collections::HashSet;

use kima_db::PgPool;
use kima_db::stages::{self, Claim, FailureRecord, Stage};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

async fn album(pool: &PgPool) -> Uuid {
    let artist = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'a', $2)")
        .bind(artist)
        .bind(artist.to_string())
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(id)
        .bind(artist)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

/// A present track whose file is `age_secs` old.
async fn track(pool: &PgPool, album_id: Uuid, age_secs: i64) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint) \
         VALUES ($1, $2, 'a', 't', 'flac', $3, 1, now() - make_interval(secs => $4), '\\x00')",
    )
    .bind(id)
    .bind(album_id)
    .bind(id.to_string())
    .bind(age_secs as f64)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Put a stage row in place. `next` is an SQL expression for `next_attempt_at`.
async fn put(pool: &PgPool, t: Uuid, stage: &str, status: &str, next: &str) {
    sqlx::query(&format!(
        "INSERT INTO track_stages (track_id, stage, status, next_attempt_at, started_at) \
         VALUES ($1, $2, $3, {next}, now() - interval '1 hour')"
    ))
    .bind(t)
    .bind(stage)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

fn ids(claims: &[Claim]) -> Vec<Uuid> {
    claims.iter().map(|c| c.track_id).collect()
}

/// Expire every open lease of `stage`, as if the clock had moved past it.
async fn lapse(pool: &PgPool, stage: &str) {
    sqlx::query(
        "UPDATE track_stages SET next_attempt_at = now() - interval '1 second' \
         WHERE stage = $1 AND status = 'working'",
    )
    .bind(stage)
    .execute(pool)
    .await
    .unwrap();
}

async fn row(pool: &PgPool, t: Uuid, stage: &str) -> sqlx::postgres::PgRow {
    sqlx::query("SELECT * FROM track_stages WHERE track_id = $1 AND stage = $2")
        .bind(t)
        .bind(stage)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn failure(code: &str, class: Option<&str>, retry_at: Option<OffsetDateTime>) -> FailureRecord {
    FailureRecord {
        pending_until: None,
        retry_at,
        count_attempt: true,
        code: code.into(),
        class: class.map(Into::into),
        message: "boom".into(),
    }
}

#[test]
fn stage_names() {
    assert_eq!(Stage::Fingerprint.as_str(), "fingerprint");
    assert_eq!(Stage::Recording.as_str(), "recording");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_row_is_pending_newest_mtime_first(pool: PgPool) {
    let al = album(&pool).await;
    let old = track(&pool, al, 300).await;
    let new = track(&pool, al, 10).await;
    let mid = track(&pool, al, 100).await;
    let got = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&got), vec![new, mid, old]);
    for c in &got {
        assert_eq!(c.stage, Stage::Fingerprint);
        assert_eq!(c.attempts, 0);
        assert_eq!(c.redelivered, 0);
    }
    // The batch size holds.
    let al2 = album(&pool).await;
    for i in 0..5 {
        track(&pool, al2, i).await;
    }
    let two = stages::claim(&pool, Stage::Fingerprint, 2, true)
        .await
        .unwrap();
    assert_eq!(two.len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_tracks_not_claimed(pool: PgPool) {
    let al = album(&pool).await;
    let gone = track(&pool, al, 10).await;
    let here = track(&pool, al, 20).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    let got = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&got), vec![here]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn terminal_done_and_not_yet_due_rows_are_not_claimed(pool: PgPool) {
    let al = album(&pool).await;
    let done = track(&pool, al, 1).await;
    let terminal = track(&pool, al, 2).await;
    let later = track(&pool, al, 3).await;
    let parked = track(&pool, al, 4).await;
    let leased = track(&pool, al, 5).await;
    let due_retry = track(&pool, al, 6).await;
    let due_pending = track(&pool, al, 7).await;
    let parked_over = track(&pool, al, 8).await;
    put(&pool, done, "fingerprint", "done", "NULL").await;
    put(&pool, terminal, "fingerprint", "failed", "NULL").await;
    put(
        &pool,
        later,
        "fingerprint",
        "failed",
        "now() + interval '1 minute'",
    )
    .await;
    put(
        &pool,
        parked,
        "fingerprint",
        "pending",
        "now() + interval '1 minute'",
    )
    .await;
    put(
        &pool,
        leased,
        "fingerprint",
        "working",
        "now() + interval '1 minute'",
    )
    .await;
    put(
        &pool,
        due_retry,
        "fingerprint",
        "failed",
        "now() - interval '1 second'",
    )
    .await;
    put(&pool, due_pending, "fingerprint", "pending", "NULL").await;
    put(
        &pool,
        parked_over,
        "fingerprint",
        "pending",
        "now() - interval '1 second'",
    )
    .await;
    let got: HashSet<Uuid> = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .iter()
        .map(|c| c.track_id)
        .collect();
    assert_eq!(got, HashSet::from([due_retry, due_pending, parked_over]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recording_waits_for_settled_fingerprint(pool: PgPool) {
    let al = album(&pool).await;
    let none = track(&pool, al, 1).await;
    let pending = track(&pool, al, 2).await;
    let retry_due = track(&pool, al, 3).await;
    let retry_later = track(&pool, al, 4).await;
    let working = track(&pool, al, 5).await;
    let done = track(&pool, al, 6).await;
    let terminal = track(&pool, al, 7).await;
    put(&pool, pending, "fingerprint", "pending", "NULL").await;
    put(
        &pool,
        retry_due,
        "fingerprint",
        "failed",
        "now() - interval '1 second'",
    )
    .await;
    put(
        &pool,
        retry_later,
        "fingerprint",
        "failed",
        "now() + interval '1 hour'",
    )
    .await;
    put(
        &pool,
        working,
        "fingerprint",
        "working",
        "now() + interval '1 hour'",
    )
    .await;
    put(&pool, done, "fingerprint", "done", "NULL").await;
    put(&pool, terminal, "fingerprint", "failed", "NULL").await;
    let _ = none;
    let got: HashSet<Uuid> = stages::claim(&pool, Stage::Recording, 20, true)
        .await
        .unwrap()
        .iter()
        .map(|c| c.track_id)
        .collect();
    assert_eq!(got, HashSet::from([done, terminal]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fingerprinting_unavailable_counts_as_settled(pool: PgPool) {
    let al = album(&pool).await;
    let none = track(&pool, al, 1).await;
    let pending = track(&pool, al, 2).await;
    put(&pool, pending, "fingerprint", "pending", "NULL").await;
    let got: HashSet<Uuid> = stages::claim(&pool, Stage::Recording, 20, false)
        .await
        .unwrap()
        .iter()
        .map(|c| c.track_id)
        .collect();
    assert_eq!(got, HashSet::from([none, pending]));
    // The fingerprint stage itself is never gated.
    let fp = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    assert_eq!(fp.len(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_claims_split(pool: PgPool) {
    let al = album(&pool).await;
    let mut all = HashSet::new();
    for i in 0..60 {
        all.insert(track(&pool, al, i).await);
    }
    // Twelve claimers race for 60 tracks in batches of 4 until nothing is left.
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            loop {
                let got = stages::claim(&pool, Stage::Fingerprint, 4, true)
                    .await
                    .unwrap();
                if got.is_empty() {
                    break;
                }
                mine.extend(got);
            }
            mine
        }));
    }
    let mut seen = HashSet::new();
    let mut total = 0;
    for t in tasks {
        for c in t.await.unwrap() {
            assert_eq!(c.redelivered, 0, "a live claim was handed out twice");
            assert!(seen.insert(c.track_id), "track claimed twice");
            total += 1;
        }
    }
    assert_eq!(total, 60);
    assert_eq!(seen, all);
    let redelivered: i64 = sqlx::query_scalar("SELECT sum(redelivered)::bigint FROM track_stages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(redelivered, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_creates_working_rows_with_lease(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 1).await;
    let got = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    let r = row(&pool, t, "fingerprint").await;
    assert_eq!(r.get::<String, _>("status"), "working");
    assert_eq!(
        r.get::<Option<OffsetDateTime>, _>("started_at"),
        Some(got[0].started_at)
    );
    let lease: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM next_attempt_at - started_at)::float8 FROM track_stages \
         WHERE track_id = $1",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(lease, 15.0 * 60.0);
    // started_at is the database clock.
    let skew: f64 = sqlx::query_scalar(
        "SELECT abs(extract(epoch FROM now() - started_at))::float8 FROM track_stages",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(skew < 60.0);
    // A held lease is not offered again.
    assert!(
        stages::claim(&pool, Stage::Fingerprint, 20, true)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn lapsed_claim_is_redelivered(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 1).await;
    let first = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    lapse(&pool, "fingerprint").await;
    let second = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].track_id, t);
    assert_eq!(second[0].redelivered, 1);
    // A redelivery uses up no attempt.
    assert_eq!(second[0].attempts, 0);
    assert_ne!(second[0].started_at, first[0].started_at);
    // A failed retry coming due is not a redelivery.
    let rec = failure(
        "TIMEOUT",
        Some("INCONCLUSIVE"),
        Some(OffsetDateTime::now_utc()),
    );
    assert!(
        stages::report_failure(&pool, &second[0], &rec)
            .await
            .unwrap()
    );
    lapse(&pool, "fingerprint").await;
    sqlx::query("UPDATE track_stages SET next_attempt_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let third = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap();
    assert_eq!(third[0].redelivered, 1);
    assert_eq!(third[0].attempts, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_report_dropped(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 1).await;
    let first = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    lapse(&pool, "fingerprint").await;
    let second = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    let before = row(&pool, t, "fingerprint").await;

    let mut conn = pool.acquire().await.unwrap();
    let ok = stages::report_done(&mut conn, &first, "ffmpeg", &serde_json::json!({"x": 1}))
        .await
        .unwrap();
    assert!(!ok);
    let rec = failure("TIMEOUT", Some("INCONCLUSIVE"), None);
    assert!(!stages::report_failure(&pool, &first, &rec).await.unwrap());
    let after = row(&pool, t, "fingerprint").await;
    for col in ["status", "source", "error_code"] {
        assert_eq!(
            before.get::<Option<String>, _>(col),
            after.get::<Option<String>, _>(col)
        );
    }
    assert_eq!(
        before.get::<Option<OffsetDateTime>, _>("updated_at"),
        after.get::<Option<OffsetDateTime>, _>("updated_at")
    );
    assert_eq!(after.get::<i16, _>("attempts"), 0);

    // The current claim's report lands, and only once.
    assert!(
        stages::report_done(&mut conn, &second, "ffmpeg", &serde_json::json!({}))
            .await
            .unwrap()
    );
    assert!(
        !stages::report_done(&mut conn, &second, "ffmpeg", &serde_json::json!({}))
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn done_clears_errors_sets_finished_at(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 1).await;
    let c = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    let rec = failure(
        "TIMEOUT",
        Some("INCONCLUSIVE"),
        Some(OffsetDateTime::now_utc()),
    );
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let c = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    let mut conn = pool.acquire().await.unwrap();
    let ev = serde_json::json!({"duration": 180});
    assert!(
        stages::report_done(&mut conn, &c, "ffmpeg", &ev)
            .await
            .unwrap()
    );
    let r = row(&pool, t, "fingerprint").await;
    assert_eq!(r.get::<String, _>("status"), "done");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert_eq!(r.get::<Option<String>, _>("last_error"), None);
    assert_eq!(r.get::<Option<String>, _>("error_code"), None);
    assert_eq!(r.get::<Option<String>, _>("error_class"), None);
    assert_eq!(
        r.get::<Option<String>, _>("source").as_deref(),
        Some("ffmpeg")
    );
    assert_eq!(r.get::<Option<serde_json::Value>, _>("evidence"), Some(ev));
    assert!(r.get::<Option<OffsetDateTime>, _>("finished_at").is_some());
    assert_eq!(r.get::<i16, _>("attempts"), 1);
    // done settles the recording prerequisite.
    assert_eq!(
        stages::claim(&pool, Stage::Recording, 20, true)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failure_rows_follow_the_record(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 1).await;
    // Parked (rate limited): pending until then, no attempt.
    let c = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    let until = OffsetDateTime::now_utc() + time::Duration::minutes(5);
    let parked = FailureRecord {
        pending_until: Some(until),
        retry_at: None,
        count_attempt: false,
        code: "RATE_LIMITED".into(),
        class: None,
        message: "paused".into(),
    };
    assert!(stages::report_failure(&pool, &c, &parked).await.unwrap());
    let r = row(&pool, t, "fingerprint").await;
    assert_eq!(r.get::<String, _>("status"), "pending");
    assert_eq!(r.get::<i16, _>("attempts"), 0);
    assert_eq!(
        r.get::<Option<String>, _>("error_code").as_deref(),
        Some("RATE_LIMITED")
    );
    let next = r
        .get::<Option<OffsetDateTime>, _>("next_attempt_at")
        .unwrap();
    assert!((next - until).abs() < time::Duration::milliseconds(1));
    assert!(
        stages::claim(&pool, Stage::Fingerprint, 20, true)
            .await
            .unwrap()
            .is_empty()
    );
    // Terminal failure: failed, NULL next, attempt counted, class kept.
    sqlx::query("UPDATE track_stages SET next_attempt_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let c = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(c.redelivered, 0);
    let rec = failure("HTTP_400", Some("PERMANENT"), None);
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t, "fingerprint").await;
    assert_eq!(r.get::<String, _>("status"), "failed");
    assert_eq!(r.get::<Option<OffsetDateTime>, _>("next_attempt_at"), None);
    assert_eq!(r.get::<i16, _>("attempts"), 1);
    assert_eq!(
        r.get::<Option<String>, _>("error_class").as_deref(),
        Some("PERMANENT")
    );
    assert_eq!(
        r.get::<Option<String>, _>("last_error").as_deref(),
        Some("boom")
    );
    assert!(r.get::<Option<OffsetDateTime>, _>("finished_at").is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn last_error_cut_at_2000(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 1).await;
    let c = stages::claim(&pool, Stage::Fingerprint, 20, true)
        .await
        .unwrap()
        .remove(0);
    // Multi-byte characters: a byte cut would split one.
    let mut rec = failure("TIMEOUT", Some("INCONCLUSIVE"), None);
    rec.message = "é".repeat(2500);
    assert!(stages::report_failure(&pool, &c, &rec).await.unwrap());
    let r = row(&pool, t, "fingerprint").await;
    let stored = r.get::<Option<String>, _>("last_error").unwrap();
    assert_eq!(stored.chars().count(), 2000);
    assert_eq!(stored, "é".repeat(2000));
}
