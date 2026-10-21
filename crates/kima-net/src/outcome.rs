//! What a request came to (spec §7.2).

use time::OffsetDateTime;

/// The result of one outbound call. Callers never see `reqwest` types.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// A real answer.
    Answered(T),
    /// The source answered "no such thing".
    Absent(Evidence),
    /// The source is paused until then; costs the caller no attempt.
    RateLimited { until: OffsetDateTime },
    /// Timeout, network, 5xx, 408, bad JSON, switched off: the stage ledger retries it.
    Transient(Failure),
    /// Any other 4xx, a refused redirect: never retried.
    Permanent(Failure),
}

/// Why a call failed: a stable `code` (`HTTP_502`, `TIMEOUT`, `NETWORK`, `BAD_RESPONSE`, ...)
/// and a message for logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

impl Failure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Failure {
        Failure {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// What the source said when it answered "absent".
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence(pub serde_json::Value);

impl<T> Outcome<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Outcome<U> {
        match self {
            Outcome::Answered(t) => Outcome::Answered(f(t)),
            Outcome::Absent(e) => Outcome::Absent(e),
            Outcome::RateLimited { until } => Outcome::RateLimited { until },
            Outcome::Transient(f) => Outcome::Transient(f),
            Outcome::Permanent(f) => Outcome::Permanent(f),
        }
    }

    /// The failure code of a `Transient` or `Permanent` outcome.
    pub fn code(&self) -> Option<&str> {
        match self {
            Outcome::Transient(f) | Outcome::Permanent(f) => Some(&f.code),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_keeps_the_variant() {
        assert_eq!(Outcome::Answered(2).map(|x| x * 2), Outcome::Answered(4));
        let t: Outcome<i32> = Outcome::Transient(Failure::new("TIMEOUT", "slow"));
        assert_eq!(t.map(|x| x + 1).code(), Some("TIMEOUT"));
    }
}
