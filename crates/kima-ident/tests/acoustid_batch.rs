//! AcoustID asked in batches and its replies kept (M-SYNC Task 12; upstream
//! `acoustidService.batch.test.ts`, `acoustidIdentification.test.ts` prefetch cases). Every
//! answer is hand-written; nothing reaches api.acoustid.org.

mod common;

use std::collections::BTreeMap;

use common::{ident_ctx, track};
use kima_db::acoustid_replies::{self as replies, Status};
use kima_ident::IdentCtx;
use kima_ident::acoustid_batch::{self, PREFETCH_AHEAD};
use kima_net::testing::{Entry, Recording, Replay, body_sha256};
use kima_net::{Bases, Outcome, Source};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const KEY: &str = "test-key";
const REC: &str = "8f6a1a2b-3c4d-5e6f-7a8b-9c0d1e2f3a4b";

fn post(status: u16, body: Value, sha: Option<String>) -> Entry {
    Entry {
        method: "POST".into(),
        url: "/lookup".into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: sha,
    }
}

fn batch_body(qs: &[(String, i32)]) -> String {
    let mut s = format!("client={KEY}&meta=recordings%20releasegroups&batch=1");
    for (i, (fp, secs)) in qs.iter().enumerate() {
        s.push_str(&format!("&duration.{i}={secs}&fingerprint.{i}={fp}"));
    }
    s
}

fn single_body(fp: &str, secs: i32) -> String {
    format!("client={KEY}&duration={secs}&fingerprint={fp}&meta=recordings%20releasegroups")
}

fn qs(n: usize) -> Vec<(String, i32)> {
    (0..n)
        .map(|i| (format!("fp{i:03}"), 200 + i as i32))
        .collect()
}

/// A batch reply naming each of `indexes`, with a confident result for `matched` of them.
fn reply(indexes: &[usize], matched: &[usize]) -> Value {
    let items: Vec<Value> = indexes
        .iter()
        .map(|&i| {
            let results = if matched.contains(&i) {
                json!([{"id": "a1b2c3d4-0000-4000-8000-0000000000ff", "score": 0.9, "recordings": [
                    {"id": REC, "title": "Song", "duration": 200}]}])
            } else {
                json!([])
            };
            json!({"index": i, "results": results})
        })
        .collect();
    json!({"status": "ok", "fingerprints": items})
}

struct Fx {
    replay: Replay,
    ctx: IdentCtx,
    _dir: tempfile::TempDir,
}

async fn fx(pool: &PgPool, entries: Vec<Entry>) -> Fx {
    let replay = Replay::start(Recording(entries)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(pool, dir.path(), Bases::all(&replay.url()));
    ctx.config.acoustid_env_key = Some(KEY.into());
    Fx {
        replay,
        ctx,
        _dir: dir,
    }
}

async fn status_and_ttl(pool: &PgPool, fp: &str, secs: i32) -> (String, i64, Value) {
    let h = replies::fingerprint_hash(fp);
    let (status, ttl, results): (String, f64, Value) = sqlx::query_as(
        "SELECT status, extract(epoch FROM expires_at - saved_at)::float8, results \
         FROM acoustid_replies WHERE fingerprint_hash = $1 AND duration_secs = $2",
    )
    .bind(h)
    .bind(secs)
    .fetch_one(pool)
    .await
    .unwrap();
    (status, ttl.round() as i64, results)
}

async fn reply_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM acoustid_replies")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ten_fingerprints_per_request(pool: PgPool) {
    let all = qs(25);
    let entries = all
        .chunks(10)
        .map(|c| {
            let idx: Vec<usize> = (0..c.len()).collect();
            post(200, reply(&idx, &[]), Some(body_sha256(&batch_body(c))))
        })
        .collect();
    let f = fx(&pool, entries).await;

    let asked = acoustid_batch::prefetch(&f.ctx, &all).await;

    // Only requests with exactly these chunk bodies (10, 10, 5) are answered.
    assert_eq!(asked, 25);
    assert_eq!(f.replay.asked(), vec!["POST /lookup"; 3]);
    assert_eq!(reply_count(&pool).await, 25);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reply_saved_30_days_matched_7_days_unmatched(pool: PgPool) {
    let all = qs(2);
    let f = fx(
        &pool,
        vec![post(
            200,
            reply(&[0, 1], &[0]),
            Some(body_sha256(&batch_body(&all))),
        )],
    )
    .await;

    acoustid_batch::prefetch(&f.ctx, &all).await;

    let (s0, t0, r0) = status_and_ttl(&pool, "fp000", 200).await;
    assert_eq!((s0.as_str(), t0), ("matched", 30 * 86_400));
    assert_eq!(r0[0]["recordings"][0]["id"], json!(REC));
    let (s1, t1, r1) = status_and_ttl(&pool, "fp001", 201).await;
    assert_eq!((s1.as_str(), t1, r1), ("unmatched", 7 * 86_400, json!([])));
    // No secret is ever stored with a reply.
    let stored: String =
        sqlx::query_scalar("SELECT string_agg(results::text, '') FROM acoustid_replies")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!stored.contains(KEY));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_fingerprint_dropped_from_a_batch_is_saved_as_rejected_for_a_day(pool: PgPool) {
    let all = qs(3);
    let f = fx(
        &pool,
        vec![post(
            200,
            reply(&[0, 2], &[]),
            Some(body_sha256(&batch_body(&all))),
        )],
    )
    .await;

    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all).await, 3);

    // The server skipped index 1 without a word.
    assert_eq!(
        status_and_ttl(&pool, "fp001", 201).await,
        ("rejected".to_string(), 86_400, json!([]))
    );
    assert_eq!(status_and_ttl(&pool, "fp000", 200).await.0, "unmatched");
    // A rejected reply is still a reply: it is not asked again.
    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all[1..2]).await, 0);
    assert_eq!(f.replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_request_the_server_refuses_as_invalid_rejects_every_fingerprint_in_it(pool: PgPool) {
    let all = qs(2);
    let f = fx(
        &pool,
        vec![post(
            400,
            json!({"status": "error", "error": {"code": 3, "message": "invalid fingerprint"}}),
            None,
        )],
    )
    .await;

    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all).await, 2);

    for (fp, secs) in &all {
        assert_eq!(status_and_ttl(&pool, fp, *secs).await.0, "rejected");
    }
    // That is no source failure: batches are not held and the source is not paused.
    assert!(acoustid_batch::may_ask_in_batches(&f.ctx).await);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn failed_batch_holds_batches_for_a_minute_songs_ask_alone(pool: PgPool) {
    let all = qs(2);
    let f = fx(
        &pool,
        vec![
            post(
                500,
                json!({"status": "error"}),
                Some(body_sha256(&batch_body(&all))),
            ),
            post(
                200,
                json!({"status": "ok", "results": []}),
                Some(body_sha256(&single_body("fp000", 200))),
            ),
        ],
    )
    .await;

    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all).await, 0);
    assert!(!acoustid_batch::may_ask_in_batches(&f.ctx).await);
    let after_failure = f.replay.asked().len();
    assert!(after_failure >= 1);

    // The minute has not passed: nothing is asked in a batch...
    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all).await, 0);
    assert_eq!(f.replay.asked().len(), after_failure);
    // ...but a song can still ask on its own.
    let out = acoustid_batch::answer(&f.ctx, KEY, "fp000", 200, None).await;
    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert_eq!(f.replay.asked().len(), after_failure + 1);
    assert_eq!(status_and_ttl(&pool, "fp000", 200).await.0, "unmatched");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rate_limit_pauses_the_source(pool: PgPool) {
    let all = qs(25);
    let f = fx(&pool, vec![post(429, json!({"status": "error"}), None)]).await;

    // The first request is answered "slow down": the source is paused and nothing else is asked.
    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all).await, 0);

    assert!(f.ctx.net.paused_until(Source::AcoustId).is_some());
    assert!(!acoustid_batch::may_ask_in_batches(&f.ctx).await);
    // Net may repeat the one request once; the other two chunks are never sent.
    let sent = f.replay.asked().len();
    assert!(sent <= 2, "{sent}");
    assert_eq!(acoustid_batch::prefetch(&f.ctx, &all).await, 0);
    assert_eq!(f.replay.asked().len(), sent);
    assert_eq!(reply_count(&pool).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn length_rounding_to_zero_is_never_asked(pool: PgPool) {
    let f = fx(&pool, vec![]).await;
    let none = vec![
        ("fpA".to_string(), 0),
        ("fpB".to_string(), -3),
        (" ".to_string(), 200),
    ];

    assert_eq!(acoustid_batch::prefetch(&f.ctx, &none).await, 0);

    assert!(f.replay.asked().is_empty());
    assert_eq!(reply_count(&pool).await, 0);
    let out = acoustid_batch::answer(&f.ctx, KEY, "fpA", 0, None).await;
    assert_eq!(out.code(), Some("NO_FINGERPRINT"));
    assert!(f.replay.asked().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_saved_reply_is_not_asked_again_and_a_duplicate_is_asked_once(pool: PgPool) {
    let all = qs(2);
    let f = fx(
        &pool,
        vec![post(
            200,
            reply(&[0], &[]),
            Some(body_sha256(&batch_body(&all[..1]))),
        )],
    )
    .await;
    let h = replies::fingerprint_hash("fp001");
    replies::put(&pool, &h, 201, Status::Matched, &json!([]))
        .await
        .unwrap();

    // fp001 is saved; fp000 is listed twice and goes once.
    let asked =
        acoustid_batch::prefetch(&f.ctx, &[all[0].clone(), all[1].clone(), all[0].clone()]).await;

    assert_eq!(asked, 1);
    assert_eq!(f.replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn without_a_key_or_when_disabled_nothing_is_asked(pool: PgPool) {
    let f = fx(&pool, vec![]).await;
    let mut no_key = f.ctx.clone();
    no_key.config.acoustid_env_key = None;
    assert_eq!(acoustid_batch::prefetch(&no_key, &qs(3)).await, 0);
    let mut disabled = f.ctx.clone();
    disabled.config.acoustid_disabled = true;
    assert_eq!(acoustid_batch::prefetch(&disabled, &qs(3)).await, 0);
    assert!(f.replay.asked().is_empty());
    assert_eq!(
        acoustid_batch::prefetch_ahead(&no_key, PREFETCH_AHEAD)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_next_hundred_songs_are_asked_ahead_ten_to_a_request(pool: PgPool) {
    // Every batch is answered with no fingerprints at all, so each question is saved rejected.
    let f = fx(
        &pool,
        vec![post(200, json!({"status": "ok", "fingerprints": []}), None)],
    )
    .await;
    let mut tagged = Vec::new();
    for i in 0..120 {
        let t = track(&pool, &format!("{i}.flac"), 200).await;
        let mut conn = pool.acquire().await.unwrap();
        kima_db::identity::store_fingerprint(&mut conn, t, &format!("fp{i:03}"), 200)
            .await
            .unwrap();
        if i < 10 {
            // A file that names its recording is never asked about.
            sqlx::query("UPDATE tracks SET tag_recording_mbid = $2 WHERE id = $1")
                .bind(t)
                .bind(Uuid::new_v4())
                .execute(&pool)
                .await
                .unwrap();
            tagged.push(format!("fp{i:03}"));
        }
    }

    let asked = acoustid_batch::prefetch_ahead(&f.ctx, PREFETCH_AHEAD)
        .await
        .unwrap();

    assert_eq!(asked, 100);
    assert_eq!(f.replay.asked().len(), 10);
    assert_eq!(reply_count(&pool).await, 100);
    for fp in tagged {
        let h = replies::fingerprint_hash(&fp);
        assert_eq!(replies::get(&pool, &h, 200).await.unwrap(), None, "{fp}");
    }
    // The window is the next 100 songs, as upstream: all saved now, so nothing more is asked
    // until the recording stage has taken some of them.
    let again = acoustid_batch::prefetch_ahead(&f.ctx, PREFETCH_AHEAD)
        .await
        .unwrap();
    assert_eq!(again, 0);
    assert_eq!(f.replay.asked().len(), 10);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_single_lookup_saves_its_reply_and_the_next_is_served_from_it(pool: PgPool) {
    let f = fx(
        &pool,
        vec![post(
            200,
            json!({"status": "ok", "results": [
                {"id": "r", "score": 0.9, "recordings": [{"id": REC, "title": "Song", "duration": 200}]}]}),
            Some(body_sha256(&single_body("fpS", 200))),
        )],
    )
    .await;

    let first = acoustid_batch::answer(&f.ctx, KEY, "fpS", 200, Some("Song")).await;
    let second = acoustid_batch::answer(&f.ctx, "another-key", "fpS", 200, Some("Song")).await;

    assert!(
        matches!(&first, Outcome::Answered(p) if p.title_agrees),
        "{first:?}"
    );
    assert_eq!(first, second);
    assert_eq!(f.replay.asked().len(), 1);
    assert_eq!(status_and_ttl(&pool, "fpS", 200).await.0, "matched");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_refused_single_lookup_is_saved_as_rejected_and_answers_absent(pool: PgPool) {
    let f = fx(
        &pool,
        vec![post(
            400,
            json!({"status": "error", "error": {"code": 8, "message": "invalid duration"}}),
            None,
        )],
    )
    .await;

    let out = acoustid_batch::answer(&f.ctx, KEY, "fpS", 200, None).await;

    assert!(matches!(out, Outcome::Absent(_)), "{out:?}");
    assert_eq!(status_and_ttl(&pool, "fpS", 200).await.0, "rejected");
    let again = acoustid_batch::answer(&f.ctx, KEY, "fpS", 200, None).await;
    assert!(matches!(again, Outcome::Absent(_)));
    assert_eq!(f.replay.asked().len(), 1);
}
