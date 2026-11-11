use std::collections::HashSet;

use sqlx::PgPool;
use std::sync::OnceLock;

static ZONES: OnceLock<HashSet<String>> = OnceLock::new();

pub async fn safe_zone(pool: &PgPool, tz: Option<&str>) -> sqlx::Result<String> {
    let Some(tz) = tz.filter(|s| !s.is_empty() && s.encode_utf16().count() <= 64) else {
        return Ok("UTC".to_owned());
    };
    if ZONES.get().is_none() {
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM pg_timezone_names")
            .fetch_all(pool)
            .await?;
        let _ = ZONES.set(names.into_iter().collect());
    }
    let zones = ZONES.get().expect("zone cache initialized");
    Ok(if zones.contains(tz) { tz } else { "UTC" }.to_owned())
}

pub const MIN_LISTEN_WINDOW_SECONDS: i32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Days30,
    Months12,
    All,
}
impl Period {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("30d") => Self::Days30,
            Some("12mo") => Self::Months12,
            _ => Self::All,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Days30 => "30d",
            Self::Months12 => "12mo",
            Self::All => "all",
        }
    }
    fn clause(self) -> &'static str {
        match self {
            Self::Days30 => "AND played_at >= now() - interval '30 days'",
            Self::Months12 => "AND played_at >= now() - interval '12 months'",
            Self::All => "",
        }
    }
}

pub fn listen_rows(period: Period, one_track: bool) -> String {
    let track = if one_track { "AND p.track_id = $2" } else { "" };
    format!(
        r#"WITH RECURSIVE ordered AS (
        SELECT p.id, p.track_id, p.played_at, p.source,
            t.duration_secs AS duration, t.album_id, t.track_no,
            GREATEST(t.duration_secs, 30) AS listen_window,
            coalesce(EXTRACT(EPOCH FROM (p.played_at - LAG(p.played_at)
                OVER (PARTITION BY p.track_id ORDER BY p.played_at, p.id))), 1e9)
                > GREATEST(t.duration_secs, 30) AS opens_run
        FROM plays p JOIN tracks t ON t.id = p.track_id
        WHERE p.user_id = $1 {track}
    ), grouped AS (
        SELECT o.*, sum(CASE WHEN o.opens_run THEN 1 ELSE 0 END)
            OVER (PARTITION BY o.track_id ORDER BY o.played_at, o.id) AS run_id
        FROM ordered o
    ), runs AS (
        SELECT g.*, row_number() OVER (PARTITION BY g.track_id, g.run_id ORDER BY g.played_at, g.id) AS seq
        FROM grouped g
    ), walk AS (
        SELECT r.*, r.played_at AS listen_started, TRUE AS starts_listen FROM runs r WHERE r.seq = 1
        UNION ALL
        SELECT r.*, CASE WHEN EXTRACT(EPOCH FROM (r.played_at - w.listen_started)) >= r.listen_window
            THEN r.played_at ELSE w.listen_started END,
            EXTRACT(EPOCH FROM (r.played_at - w.listen_started)) >= r.listen_window
        FROM runs r JOIN walk w ON w.track_id = r.track_id AND w.run_id = r.run_id AND r.seq = w.seq + 1
    ), p AS (SELECT * FROM walk WHERE starts_listen {})"#,
        period.clause()
    )
}

#[cfg(test)]
mod tests {
    use super::Period;
    #[test]
    fn period_parse_is_exact() {
        for (input, expected) in [
            (Some("30d"), Period::Days30),
            (Some("12mo"), Period::Months12),
            (Some("30D"), Period::All),
            (Some(" 30d"), Period::All),
            (None, Period::All),
            (Some("week"), Period::All),
        ] {
            assert_eq!(Period::parse(input), expected);
        }
    }
}
