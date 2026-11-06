//! What AcoustID answered for a fingerprint of a given length (migration 0031; upstream's Redis
//! keys `acoustid:lookup:<seconds>:<sha256>`). A saved reply means the question is not asked
//! again until it expires: a match is kept 30 days, "nothing matched" 7, and a fingerprint the
//! server dropped or refused ("rejected") one day.

use std::collections::HashSet;

use serde_json::Value;
use sqlx::PgPool;

/// A reply with results is kept this long.
pub const MATCHED_TTL_SECS: i64 = 30 * 24 * 3600;
/// A reply with no results is kept this long.
pub const UNMATCHED_TTL_SECS: i64 = 7 * 24 * 3600;
/// A fingerprint the server refused or skipped is kept this long: it can be fingerprinted again.
pub const REJECTED_TTL_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Matched,
    Unmatched,
    Rejected,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Matched => "matched",
            Status::Unmatched => "unmatched",
            Status::Rejected => "rejected",
        }
    }

    fn from_db(s: &str) -> Status {
        match s {
            "matched" => Status::Matched,
            "rejected" => Status::Rejected,
            _ => Status::Unmatched,
        }
    }

    pub fn ttl_secs(self) -> i64 {
        match self {
            Status::Matched => MATCHED_TTL_SECS,
            Status::Unmatched => UNMATCHED_TTL_SECS,
            Status::Rejected => REJECTED_TTL_SECS,
        }
    }
}

/// A reply read back.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub status: Status,
    pub results: Value,
}

/// The key of a fingerprint: SHA-256 of its trimmed text.
pub fn fingerprint_hash(fingerprint: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(fingerprint.trim().as_bytes()).to_vec()
}

/// The unexpired reply for this fingerprint and length, if any.
pub async fn get(pool: &PgPool, hash: &[u8], duration_secs: i32) -> sqlx::Result<Option<Saved>> {
    let row = sqlx::query!(
        "SELECT status, results FROM acoustid_replies
         WHERE fingerprint_hash = $1 AND duration_secs = $2 AND expires_at > now()",
        hash,
        duration_secs
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Saved {
        status: Status::from_db(&r.status),
        results: r.results,
    }))
}

/// Which of these (hash, length) questions already have an unexpired reply.
pub async fn saved_among(
    pool: &PgPool,
    questions: &[(Vec<u8>, i32)],
) -> sqlx::Result<HashSet<(Vec<u8>, i32)>> {
    let (hashes, secs): (Vec<Vec<u8>>, Vec<i32>) = questions.iter().cloned().unzip();
    let rows = sqlx::query!(
        r#"SELECT r.fingerprint_hash, r.duration_secs
           FROM acoustid_replies r
           JOIN UNNEST($1::bytea[], $2::int4[]) AS q(h, d)
             ON q.h = r.fingerprint_hash AND q.d = r.duration_secs
           WHERE r.expires_at > now()"#,
        &hashes,
        &secs
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.fingerprint_hash, r.duration_secs))
        .collect())
}

/// Saves (or replaces) the reply, expiring after `status`'s time to live.
pub async fn put(
    pool: &PgPool,
    hash: &[u8],
    duration_secs: i32,
    status: Status,
    results: &Value,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO acoustid_replies
             (fingerprint_hash, duration_secs, status, results, saved_at, expires_at)
         VALUES ($1, $2, $3, $4, now(), now() + make_interval(secs => $5::float8))
         ON CONFLICT (fingerprint_hash, duration_secs) DO UPDATE SET
             status = EXCLUDED.status, results = EXCLUDED.results,
             saved_at = EXCLUDED.saved_at, expires_at = EXCLUDED.expires_at",
        hash,
        duration_secs,
        status.as_str(),
        results,
        status.ttl_secs() as f64
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes expired replies; returns how many.
pub async fn purge_expired(pool: &PgPool) -> sqlx::Result<u64> {
    Ok(
        sqlx::query!("DELETE FROM acoustid_replies WHERE expires_at <= now()")
            .execute(pool)
            .await?
            .rows_affected(),
    )
}
