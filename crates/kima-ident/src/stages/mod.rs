//! Stage ledger policy (spec §10.3-§10.4): the constants, the error classes and how a
//! failed claim closes. The SQL lives in [`kima_db::stages`].
//!
//! Callers map a failure to [`StageFailure::FileMissing`] only for a path that escapes the
//! library root or an io `NotFound`; any other io error is `Transient`, classified by its
//! message.

pub mod fingerprint;

use std::sync::LazyLock;
use std::time::Duration;

use kima_db::stages::{Claim, FailureRecord, LAST_ERROR_MAX_CHARS, cut_chars};
use kima_net::{Failure, Outcome};
use regex::RegexSet;
use time::OffsetDateTime;

/// How long a claim is held before it is offered again (`STAGE_CLAIM_LEASE_MS`). The claim
/// statement writes the same 15 minutes as an SQL interval, on the database clock.
pub const CLAIM_LEASE: Duration = Duration::from_secs(15 * 60);
/// A claim redelivered this many times is closed as `LOST_WORKER` (`STAGE_REDELIVERY_CAP`).
pub const REDELIVERY_CAP: i16 = 3;
/// Reported failures after which an `INCONCLUSIVE` row is terminal (`STAGE_RETRY_CAP`).
pub const RETRY_CAP: i16 = 4;
/// Wait before retry n+1 after the n-th reported failure (`computeStageRetryWaitMs`).
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(10 * 60),
    Duration::from_secs(60 * 60),
];
/// Budget for one rung's request (`STAGE_TIMEOUT_MS`).
pub const STAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// `last_error` is cut to this many characters.
pub const MAX_ERROR_CHARS: usize = 2000;
const _: () = assert!(MAX_ERROR_CHARS == LAST_ERROR_MAX_CHARS);

/// `track_stages.error_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    Damaged,
    Unsupported,
    /// Upstream's spelling, kept for the UI and M4.
    Multicannel,
    Inconclusive,
    Permanent,
    FileMissing,
    /// Reserved for M4; M3 never writes it.
    DependencyClosed,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Damaged => "DAMAGED",
            ErrorClass::Unsupported => "UNSUPPORTED",
            ErrorClass::Multicannel => "MULTICANNEL",
            ErrorClass::Inconclusive => "INCONCLUSIVE",
            ErrorClass::Permanent => "PERMANENT",
            ErrorClass::FileMissing => "FILE_MISSING",
            ErrorClass::DependencyClosed => "DEPENDENCY_CLOSED",
        }
    }
}

const MULTICANNEL: &[&str] = &[
    r"more\s+than\s+2\s+audio\s+channels?",
    r"more\s+than\s+2\s+channels?",
    r"multichannel",
];
const UNSUPPORTED: &[&str] = &[
    r"unsupported\s+codec",
    r"not\s+capable\s+of\s+decoding",
    r"unknown\s+demuxer",
    r"decoder\s+.*not\s+found",
    r"codec\s+.*not\s+found",
];
const DAMAGED: &[&str] = &[
    r"truncated",
    r"\bcrc\b",
    r"over[hr]ead",
    r"missing\s+keyframe",
    r"non[- ]?monotonou[sd]\s+dts",
    r"error\s+while\s+decoding",
    r"cannot\s+decode",
    r"could\s+not\s+decode",
    r"failed\s+to\s+decode",
    r"stretched\s+frame",
    r"invalid\s+start",
    r"invalid\s+end",
    r"invalid\s+\S+\s+frame",
    r"corrupt(?:ed|ion)?\s*(?:bitstream|audio|data|stream|file)",
    r"malformed\s+(?:audio|bitstream|data|stream|file)",
    r"decode[_\s]frame\b",
    r"\bheader\s+missing\b",
    r"invalid\s+dts",
];

/// Compiled case-insensitively. The `regex` crate runs in time linear in the input, so a
/// hostile message cannot stall classification.
fn set(patterns: &[&str]) -> RegexSet {
    regex::RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .build()
        .expect("stage error patterns compile")
}

static SETS: LazyLock<[(RegexSet, ErrorClass); 3]> = LazyLock::new(|| {
    [
        (set(MULTICANNEL), ErrorClass::Multicannel),
        (set(UNSUPPORTED), ErrorClass::Unsupported),
        (set(DAMAGED), ErrorClass::Damaged),
    ]
});

/// The class of a transient or decode error message (upstream `classifyFailure`):
/// `MULTICANNEL`, else `UNSUPPORTED`, else `DAMAGED`, else `INCONCLUSIVE`.
pub fn classify(message: &str) -> ErrorClass {
    SETS.iter()
        .find(|(set, _)| set.is_match(message))
        .map_or(ErrorClass::Inconclusive, |(_, class)| *class)
}

/// Why a stage attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageFailure {
    /// The source is paused until then; costs no attempt.
    RateLimited {
        until: OffsetDateTime,
    },
    Permanent(Failure),
    /// Timeouts, network and server errors, decode errors: classified by message.
    Transient(Failure),
    /// The path escapes the root or the file is gone (io `NotFound` only).
    FileMissing(String),
}

impl StageFailure {
    /// The failure an outcome carries; `None` for `Answered` and `Absent`.
    pub fn from_outcome<T>(o: Outcome<T>) -> Option<StageFailure> {
        match o {
            Outcome::Answered(_) | Outcome::Absent(_) => None,
            Outcome::RateLimited { until } => Some(StageFailure::RateLimited { until }),
            Outcome::Permanent(f) => Some(StageFailure::Permanent(f)),
            Outcome::Transient(f) => Some(StageFailure::Transient(f)),
        }
    }
}

fn message(s: &str) -> String {
    cut_chars(s, MAX_ERROR_CHARS).to_owned()
}

fn terminal(code: &str, class: ErrorClass, msg: &str) -> FailureRecord {
    FailureRecord {
        pending_until: None,
        retry_at: None,
        count_attempt: true,
        code: code.to_owned(),
        class: Some(class.as_str().to_owned()),
        message: message(msg),
    }
}

/// How `claim` closes after failure `f` (spec §10.4). `now` dates an `INCONCLUSIVE` retry.
pub fn record_for(claim: &Claim, f: &StageFailure, now: OffsetDateTime) -> FailureRecord {
    match f {
        StageFailure::RateLimited { until } => FailureRecord {
            pending_until: Some(*until),
            retry_at: None,
            count_attempt: false,
            code: "RATE_LIMITED".to_owned(),
            class: None,
            message: "source paused (rate limited)".to_owned(),
        },
        StageFailure::Permanent(f) => terminal(&f.code, ErrorClass::Permanent, &f.message),
        StageFailure::FileMissing(msg) => terminal("FILE_MISSING", ErrorClass::FileMissing, msg),
        StageFailure::Transient(f) => {
            let class = classify(&f.message);
            let mut rec = terminal(&f.code, class, &f.message);
            if class == ErrorClass::Inconclusive && claim.attempts.saturating_add(1) < RETRY_CAP {
                rec.retry_at = usize::try_from(claim.attempts)
                    .ok()
                    .and_then(|i| RETRY_DELAYS.get(i))
                    .map(|d| now + *d);
            }
            rec
        }
    }
}

/// A claim redelivered [`REDELIVERY_CAP`] times: its workers keep vanishing, so it closes
/// for good.
pub fn lost_worker(claim: &Claim) -> FailureRecord {
    terminal(
        "LOST_WORKER",
        ErrorClass::Inconclusive,
        &format!(
            "claim lease lapsed {} times; the worker was lost",
            claim.redelivered
        ),
    )
}

/// What one batch of a stage did. `claimed − done − failed` claims were stale or abandoned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchReport {
    pub claimed: usize,
    /// Closed `done` by this batch.
    pub done: usize,
    /// Closed `failed` (or parked) by this batch.
    pub failed: usize,
}
