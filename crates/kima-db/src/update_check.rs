//! The update check's stored state (M5b Task 17; upstream `SystemSettings.updateChecksEnabled`, `lastUpdateCheck`,
//! `latestKnownVersion`, migration 0065).

use sqlx::PgPool;
use time::OffsetDateTime;

/// What the last check left, and the switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckState {
    pub enabled: bool,
    pub last_check: Option<OffsetDateTime>,
    pub latest_known: Option<String>,
}

pub async fn get(pool: &PgPool) -> sqlx::Result<UpdateCheckState> {
    let (enabled, last_check, latest_known): (bool, Option<OffsetDateTime>, Option<String>) =
        sqlx::query_as(
            "SELECT update_checks_enabled, last_update_check, latest_known_version FROM system_settings",
        )
        .fetch_one(pool)
        .await?;
    Ok(UpdateCheckState {
        enabled,
        last_check,
        latest_known,
    })
}

/// Records a check made at `at` and the latest version known after it.
pub async fn record(pool: &PgPool, at: OffsetDateTime, latest: Option<&str>) -> sqlx::Result<()> {
    sqlx::query("UPDATE system_settings SET last_update_check = $1, latest_known_version = $2")
        .bind(at)
        .bind(latest)
        .execute(pool)
        .await?;
    Ok(())
}

/// Upstream's `updateChecksEnabled` switch (an admin system setting).
pub async fn set_enabled(pool: &PgPool, enabled: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE system_settings SET update_checks_enabled = $1")
        .bind(enabled)
        .execute(pool)
        .await?;
    Ok(())
}
