//! How an upstream's failure is answered (`utils/upstream.ts`): never as kima's own 500.

use kima_net::Outcome;

/// What went wrong with Apple, a feed's host or an episode's host.
#[derive(Debug, thiserror::Error)]
pub enum Upstream {
    /// Refused by the address guard, or the upstream's 4xx (other than 429) says it has no
    /// such thing: a 404 to the client.
    #[error("not available upstream: {0}")]
    Missing(String),
    /// Throttled, an outage, a timeout, unreachable, or a reply that is not what was asked for:
    /// a 502 to the client.
    #[error("upstream did not answer: {0}")]
    Down(String),
    /// Kima's own failure (the database).
    #[error(transparent)]
    Internal(anyhow::Error),
}

impl Upstream {
    /// A copy, for a failure several askers share (the internal error as its message).
    pub fn copy(&self) -> Upstream {
        match self {
            Upstream::Missing(m) => Upstream::Missing(m.clone()),
            Upstream::Down(m) => Upstream::Down(m.clone()),
            Upstream::Internal(e) => Upstream::Internal(anyhow::anyhow!("{e:#}")),
        }
    }

    pub fn internal(e: impl Into<anyhow::Error>) -> Upstream {
        Upstream::Internal(e.into())
    }
}

/// `HTTP_4xx` other than 429: the upstream has no such thing (`upstreamStatusFailure`).
fn missing_status(code: &str) -> bool {
    code.strip_prefix("HTTP_")
        .and_then(|n| n.parse::<u16>().ok())
        .is_some_and(|n| (400..500).contains(&n) && n != 429)
}

/// An outcome as an answer or the failure class upstream gives it (`upstreamFailure`).
pub fn classify<T>(out: Outcome<T>) -> Result<T, Upstream> {
    match out {
        Outcome::Answered(t) => Ok(t),
        Outcome::Absent(e) => Err(Upstream::Missing(e.0.to_string())),
        Outcome::RateLimited { until } => {
            Err(Upstream::Down(format!("rate-limited until {until}")))
        }
        Outcome::Transient(f) | Outcome::Permanent(f)
            if f.code == "NOT_PUBLIC" || missing_status(&f.code) =>
        {
            Err(Upstream::Missing(format!("{}: {}", f.code, f.message)))
        }
        Outcome::Transient(f) | Outcome::Permanent(f) => {
            Err(Upstream::Down(format!("{}: {}", f.code, f.message)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kima_net::{Evidence, Failure, Outcome};
    use serde_json::json;

    fn class(out: Outcome<()>) -> &'static str {
        match classify(out) {
            Ok(()) => "ok",
            Err(Upstream::Missing(_)) => "missing",
            Err(Upstream::Down(_)) => "down",
            Err(Upstream::Internal(_)) => "internal",
        }
    }

    /// `utils/upstream.ts:24-48`.
    #[test]
    fn classify_maps_outcomes_like_upstream() {
        let t = |code: &str| Outcome::Transient(Failure::new(code, "x"));
        let p = |code: &str| Outcome::Permanent(Failure::new(code, "x"));
        assert_eq!(class(Outcome::Answered(())), "ok");
        assert_eq!(
            class(Outcome::Absent(Evidence(json!({"status": 404})))),
            "missing"
        );
        assert_eq!(class(p("HTTP_404")), "missing");
        assert_eq!(class(p("HTTP_403")), "missing");
        assert_eq!(class(p("NOT_PUBLIC")), "missing");
        assert_eq!(class(t("HTTP_429")), "down");
        assert_eq!(
            class(Outcome::RateLimited {
                until: time::OffsetDateTime::now_utc()
            }),
            "down"
        );
        assert_eq!(class(t("HTTP_503")), "down");
        assert_eq!(class(t("TIMEOUT")), "down");
        assert_eq!(class(t("NETWORK")), "down");
        assert_eq!(class(t("BAD_RESPONSE")), "down");
        assert_eq!(class(t("BUSY")), "down");
        assert_eq!(class(p("TOO_MANY_REDIRECTS")), "down");
    }
}
