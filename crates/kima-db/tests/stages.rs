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
    assert_eq!(Stage::Embedding.as_str(), "embedding");
    assert_eq!(Stage::Analysis.as_str(), "analysis");
}

/// The analysis stage is eligible as soon as a track exists (unconditional, like
/// fingerprint/embedding): it is never gated on fingerprint, recording or embedding
/// settling.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn analysis_is_claimed_unconditionally_like_embedding(pool: PgPool) {
    let al = album(&pool).await;
    let new = track(&pool, al, 10).await;
    let old = track(&pool, al, 300).await;
    put(&pool, new, "embedding", "pending", "NULL").await;
    let got = stages::claim(&pool, Stage::Analysis, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&got), vec![new, old]);
    for c in &got {
        assert_eq!(c.stage, Stage::Analysis);
        assert_eq!(c.attempts, 0);
        assert_eq!(c.redelivered, 0);
    }
}

/// Analysis shares the exact lease/redelivery policy already used for embedding: a
/// lapsed working lease is redelivered and counted, same as every other stage.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn analysis_lease_and_redelivery_match_embedding(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 10).await;
    let first = stages::claim(&pool, Stage::Analysis, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&first), vec![t]);
    lapse(&pool, "analysis").await;
    let second = stages::claim(&pool, Stage::Analysis, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&second), vec![t]);
    assert_eq!(second[0].redelivered, 1);
}

/// The embedding stage is eligible as soon as a track exists (unconditional, like
/// fingerprint): it is never gated on fingerprint or recording settling.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_is_claimed_unconditionally_like_fingerprint(pool: PgPool) {
    let al = album(&pool).await;
    let new = track(&pool, al, 10).await;
    let old = track(&pool, al, 300).await;
    // No fingerprint or recording row exists for either track.
    let got = stages::claim(&pool, Stage::Embedding, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&got), vec![new, old]);
    for c in &got {
        assert_eq!(c.stage, Stage::Embedding);
        assert_eq!(c.attempts, 0);
        assert_eq!(c.redelivered, 0);
    }
}

/// Embedding shares the exact lease/redelivery policy already used for recording: a
/// lapsed working lease is redelivered and counted, same as every other stage.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_lease_and_redelivery_match_recording(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 10).await;
    let first = stages::claim(&pool, Stage::Embedding, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&first), vec![t]);
    lapse(&pool, "embedding").await;
    let second = stages::claim(&pool, Stage::Embedding, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&second), vec![t]);
    assert_eq!(second[0].redelivered, 1);
}

/// Embedding is claimable even when the track's recording stage is still pending
/// (independent of Recording/Analysis eligibility, per owner decision).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn embedding_does_not_wait_for_recording(pool: PgPool) {
    let al = album(&pool).await;
    let t = track(&pool, al, 10).await;
    put(&pool, t, "recording", "pending", "NULL").await;
    let got = stages::claim(&pool, Stage::Embedding, 20, true)
        .await
        .unwrap();
    assert_eq!(ids(&got), vec![t]);
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

// ---- Another stage's work on the same track (the coordinator runs the phases side by side) --

/// Makes `op` on `table` wait, for rows matching `when`, in a BEFORE trigger until the test
/// releases advisory lock 1: the writer sits inside its transaction holding every lock it
/// took before that write. Returns the session holding the gate.
async fn gate(
    pool: &PgPool,
    op: &str,
    table: &str,
    when: &str,
) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    sqlx::query(
        "CREATE FUNCTION test_gate() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock_shared(1); RETURN NEW; END $$",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER test_gate BEFORE {op} ON {table} FOR EACH ROW WHEN ({when}) \
         EXECUTE FUNCTION test_gate()"
    ))
    .execute(pool)
    .await
    .unwrap();
    let mut holder = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(1)")
        .execute(&mut *holder)
        .await
        .unwrap();
    holder
}

/// Waits until the gated writer is parked on the gate.
async fn gated(pool: &PgPool) {
    for _ in 0..3000 {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
             AND wait_event_type = 'Lock' AND wait_event = 'advisory')",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the gated writer never reached the gate");
}

async fn open_gate(mut holder: sqlx::pool::PoolConnection<sqlx::Postgres>) {
    sqlx::query("SELECT pg_advisory_unlock(1)")
        .execute(&mut *holder)
        .await
        .unwrap();
}

/// The recording claim's view while another stage's writer holds the track: it must claim
/// the track, not skip it as if it were taken (an empty claim ends the coordinator's tracks
/// phase for the cycle).
async fn recording_claims_while_held(pool: &PgPool, t: Uuid) {
    let got = stages::claim(pool, Stage::Recording, 20, false)
        .await
        .unwrap();
    assert_eq!(ids(&got), vec![t]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_takes_a_track_another_stage_is_claiming(pool: PgPool) {
    let t = track(&pool, album(&pool).await, 0).await;
    let holder = gate(&pool, "INSERT", "track_stages", "NEW.stage = 'analysis'").await;
    let analysis = tokio::spawn({
        let pool = pool.clone();
        async move {
            stages::claim(&pool, Stage::Analysis, 20, true)
                .await
                .unwrap()
        }
    });
    gated(&pool).await;

    recording_claims_while_held(&pool, t).await;

    open_gate(holder).await;
    assert_eq!(ids(&analysis.await.unwrap()), vec![t]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_takes_a_track_another_stage_is_failing(pool: PgPool) {
    let t = track(&pool, album(&pool).await, 0).await;
    let claim = stages::claim(&pool, Stage::Analysis, 20, true)
        .await
        .unwrap()
        .remove(0);
    let holder = gate(
        &pool,
        "UPDATE",
        "track_stages",
        "NEW.stage = 'analysis' AND NEW.status <> 'working'",
    )
    .await;
    let report = tokio::spawn({
        let pool = pool.clone();
        async move {
            let rec = failure("SNAPSHOT_UNAVAILABLE", None, None);
            stages::report_failure(&pool, &claim, &rec).await.unwrap()
        }
    });
    gated(&pool).await;

    recording_claims_while_held(&pool, t).await;

    open_gate(holder).await;
    assert!(report.await.unwrap());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_takes_a_track_whose_embedding_is_publishing(pool: PgPool) {
    let t = track(&pool, album(&pool).await, 0).await;
    stages::claim(&pool, Stage::Embedding, 20, true)
        .await
        .unwrap();
    let claim = kima_db::ml::embedding_claim(&pool, t)
        .await
        .unwrap()
        .unwrap();
    let holder = gate(
        &pool,
        "UPDATE",
        "track_stages",
        "NEW.stage = 'embedding' AND NEW.status = 'done'",
    )
    .await;
    let publish = tokio::spawn({
        let pool = pool.clone();
        async move {
            let model = kima_db::ml::Provenance {
                model_id: "synthetic-clap-test".into(),
                model_sha256: [42; 32],
                pipeline_version: "v1".into(),
            };
            let mut v = vec![0.; 512];
            v[0] = 1.;
            kima_db::ml::complete_embedding(&pool, &claim, &model, &v)
                .await
                .unwrap()
        }
    });
    gated(&pool).await;

    recording_claims_while_held(&pool, t).await;

    open_gate(holder).await;
    assert!(publish.await.unwrap());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_takes_a_track_whose_analysis_is_publishing(pool: PgPool) {
    let t = track(&pool, album(&pool).await, 0).await;
    let generation: i64 = sqlx::query_scalar("SELECT ml_generation FROM tracks WHERE id = $1")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    let holder = gate(&pool, "INSERT", "track_analysis", "true").await;
    let publish = tokio::spawn({
        let pool = pool.clone();
        async move {
            let loudness = kima_db::analysis::LoudnessAnalysis {
                integrated_lufs: -14.2,
                loudness_range_lu: 6.5,
            };
            kima_db::analysis::store_loudness(&pool, t, generation, "v1", &loudness)
                .await
                .unwrap()
        }
    });
    gated(&pool).await;

    recording_claims_while_held(&pool, t).await;

    open_gate(holder).await;
    assert!(publish.await.unwrap());
}

/// What the track lock is still for: a track being deleted is skipped, not waited on (its
/// stage row could not be written once the delete commits).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn claim_skips_a_track_being_deleted(pool: PgPool) {
    let al = album(&pool).await;
    let gone = track(&pool, al, 0).await;
    let kept = track(&pool, al, 10).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(gone)
        .execute(&mut *tx)
        .await
        .unwrap();

    let got = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        stages::claim(&pool, Stage::Recording, 20, false),
    )
    .await
    .expect("the claim waited on the delete")
    .unwrap();
    assert_eq!(ids(&got), vec![kept]);
    tx.commit().await.unwrap();
}
