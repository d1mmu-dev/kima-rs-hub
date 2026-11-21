//! The update check (M5b Task 17; upstream `services/updateCheck.ts` at the M5b reference `9450c94b`): whether a newer
//! release exists, for the version chip. One anonymous request a day per server against GitHub's latest-release
//! endpoint, shared by callers that arrive while it runs; a failure is silent and keeps the last known version.
//! Owner Q4 = B (D-270): kima-rs asks its own repository's releases (`d1mmu-dev/kima-rs-hub`) through `kima-net`
//! (limits, `KIMA_NET_DENY_REAL`), with upstream's `updateChecksEnabled` switch.

use std::time::Duration;

use kima_api_types::releases::UpdateStatus;
use kima_net::{Host, Outcome, Req};
use time::OffsetDateTime;

use crate::state::AppState;

/// Upstream `CHECK_INTERVAL_MS`.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// kima-rs's latest release on GitHub (upstream asks `Chevron7Locked/kima-hub`).
pub const LATEST_RELEASE_PATH: &str = "/repos/d1mmu-dev/kima-rs-hub/releases/latest";

/// The running version (upstream `APP_VERSION`).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// JavaScript's `parseInt(n, 10) || 0` for one version part: leading white space, an optional sign, the digit
/// prefix; no digits (NaN) and zero are 0.
fn version_part(n: &str) -> f64 {
    let s = n.trim_start_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'-') => (-1.0, &s[1..]),
        Some(b'+') => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return 0.0;
    }
    sign * digits.parse::<f64>().unwrap_or(0.0)
}

/// Upstream `compareVersions`: positive when `a` is newer (here its sign: -1, 0 or 1). A leading `v` is ignored.
pub fn compare_versions(a: &str, b: &str) -> i64 {
    let parts = |v: &str| -> Vec<f64> {
        v.strip_prefix('v')
            .unwrap_or(v)
            .split('.')
            .map(version_part)
            .collect()
    };
    let (ap, bp) = (parts(a), parts(b));
    for i in 0..ap.len().max(bp.len()) {
        let d = ap.get(i).copied().unwrap_or(0.0) - bp.get(i).copied().unwrap_or(0.0);
        if d > 0.0 {
            return 1;
        }
        if d < 0.0 {
            return -1;
        }
    }
    0
}

/// `Date.toISOString()`: UTC, milliseconds.
fn iso_ms(t: OffsetDateTime) -> String {
    let format = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    t.to_offset(time::UtcOffset::UTC)
        .format(&format)
        .unwrap_or_default()
}

/// Upstream `statusFrom`.
fn status_from(
    latest: Option<String>,
    last_checked: Option<OffsetDateTime>,
    enabled: bool,
) -> UpdateStatus {
    let current = current_version();
    UpdateStatus {
        enabled,
        current: current.to_string(),
        has_update: enabled
            && latest
                .as_deref()
                .is_some_and(|l| compare_versions(l, current) > 0),
        latest,
        last_checked_at: last_checked.map(iso_ms),
    }
}

/// Upstream `getUpdateStatus()`: off answers without the network; inside a day the stored answer; otherwise one
/// check, which callers arriving meanwhile wait for and share.
pub async fn update_status(state: &AppState) -> sqlx::Result<UpdateStatus> {
    let stored = kima_db::update_check::get(&state.pool).await?;
    if !stored.enabled {
        return Ok(status_from(None, None, false));
    }
    let fresh = |last: Option<OffsetDateTime>| {
        last.is_some_and(|t| OffsetDateTime::now_utc() - t < CHECK_INTERVAL)
    };
    if fresh(stored.last_check) {
        return Ok(status_from(stored.latest_known, stored.last_check, true));
    }
    let _flight = state.update_check.lock().await;
    // A check that finished while this caller waited answers it (upstream's shared `inFlight`).
    let stored = kima_db::update_check::get(&state.pool).await?;
    if fresh(stored.last_check) {
        return Ok(status_from(stored.latest_known, stored.last_check, true));
    }
    refresh(state, stored.latest_known).await
}

/// Upstream `refresh`: asks GitHub; a tag answers, anything else keeps the last known version. The check is
/// stamped either way.
async fn refresh(state: &AppState, known: Option<String>) -> sqlx::Result<UpdateStatus> {
    let mut latest = known;
    let req = Req::get(Host::GitHub, LATEST_RELEASE_PATH)
        .absent_on_404()
        .no_cache()
        .no_retry()
        .user_facing();
    match state.net.get_json::<serde_json::Value>(req).await {
        Outcome::Answered(body) => {
            if let Some(tag) = body.get("tag_name").and_then(serde_json::Value::as_str) {
                latest = Some(tag.strip_prefix('v').unwrap_or(tag).to_string());
            }
        }
        other => tracing::debug!(outcome = ?other.code(), "[UpdateCheck] lookup failed"),
    }
    let now = kima_db::mixes::mood_mix::to_ms(OffsetDateTime::now_utc());
    kima_db::update_check::record(&state.pool, now, latest.as_deref()).await?;
    Ok(status_from(latest, Some(now), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parts_follow_parse_int() {
        assert_eq!(version_part("12"), 12.0);
        assert_eq!(version_part(" 7x"), 7.0);
        assert_eq!(version_part("-3"), -3.0);
        assert_eq!(version_part("x1"), 0.0);
        assert_eq!(version_part(""), 0.0);
        assert_eq!(version_part("\u{85}5"), 0.0);
    }

    #[test]
    fn iso_has_milliseconds() {
        let t = time::macros::datetime!(2026-11-21 12:34:56.789123 UTC);
        assert_eq!(iso_ms(t), "2026-11-21T12:34:56.789Z");
    }
}
