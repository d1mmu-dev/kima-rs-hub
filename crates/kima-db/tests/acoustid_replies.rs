use kima_db::PgPool;
use kima_db::acoustid_replies::{
    self as replies, MATCHED_TTL_SECS, REJECTED_TTL_SECS, Status, UNMATCHED_TTL_SECS,
};
use serde_json::json;

async fn ttl_secs(pool: &PgPool, hash: &[u8]) -> i64 {
    sqlx::query_scalar::<_, f64>(
        "SELECT extract(epoch FROM expires_at - saved_at)::float8 FROM acoustid_replies \
         WHERE fingerprint_hash = $1",
    )
    .bind(hash)
    .fetch_one(pool)
    .await
    .unwrap()
    .round() as i64
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_reply_is_kept_for_the_time_its_status_names(pool: PgPool) {
    for (fp, status, ttl) in [
        ("fp-matched", Status::Matched, MATCHED_TTL_SECS),
        ("fp-unmatched", Status::Unmatched, UNMATCHED_TTL_SECS),
        ("fp-rejected", Status::Rejected, REJECTED_TTL_SECS),
    ] {
        let h = replies::fingerprint_hash(fp);
        replies::put(&pool, &h, 200, status, &json!([]))
            .await
            .unwrap();
        assert_eq!(ttl_secs(&pool, &h).await, ttl, "{fp}");
    }
    assert_eq!(
        (MATCHED_TTL_SECS, UNMATCHED_TTL_SECS, REJECTED_TTL_SECS),
        (30 * 86_400, 7 * 86_400, 86_400)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_key_is_the_trimmed_fingerprint_and_its_length(pool: PgPool) {
    let h = replies::fingerprint_hash("AQAA");
    assert_eq!(h, replies::fingerprint_hash("  AQAA\n"));
    replies::put(&pool, &h, 200, Status::Matched, &json!([{"id": "x"}]))
        .await
        .unwrap();
    let saved = replies::get(&pool, &h, 200).await.unwrap().unwrap();
    assert_eq!(saved.status, Status::Matched);
    assert_eq!(saved.results, json!([{"id": "x"}]));
    // Another length is another question.
    assert_eq!(replies::get(&pool, &h, 201).await.unwrap(), None);
    // A later answer replaces the earlier one.
    replies::put(&pool, &h, 200, Status::Unmatched, &json!([]))
        .await
        .unwrap();
    assert_eq!(
        replies::get(&pool, &h, 200).await.unwrap().unwrap().status,
        Status::Unmatched
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_expired_reply_is_not_a_reply(pool: PgPool) {
    let h = replies::fingerprint_hash("old");
    replies::put(&pool, &h, 200, Status::Matched, &json!([]))
        .await
        .unwrap();
    sqlx::query("UPDATE acoustid_replies SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(replies::get(&pool, &h, 200).await.unwrap(), None);
    let q = [(h.clone(), 200)];
    assert!(replies::saved_among(&pool, &q).await.unwrap().is_empty());
    assert_eq!(replies::purge_expired(&pool).await.unwrap(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn saved_among_names_the_questions_that_have_a_reply(pool: PgPool) {
    let a = replies::fingerprint_hash("a");
    let b = replies::fingerprint_hash("b");
    replies::put(&pool, &a, 100, Status::Matched, &json!([]))
        .await
        .unwrap();
    let got = replies::saved_among(&pool, &[(a.clone(), 100), (a.clone(), 101), (b, 100)])
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert!(got.contains(&(a, 100)));
}
