//! Per-user settings (upstream `UserSettings`): playback quality and client preferences.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSettings {
    pub user_id: Uuid,
    pub playback_quality: String,
    pub wifi_only: bool,
    pub offline_enabled: bool,
    pub max_cache_size_mb: i32,
    pub share_live_listening: bool,
}

impl UserSettings {
    /// What a user without a row has (the column defaults).
    pub fn defaults(user_id: Uuid) -> Self {
        Self {
            user_id,
            playback_quality: "original".into(),
            wifi_only: false,
            offline_enabled: false,
            max_cache_size_mb: 10240,
            share_live_listening: true,
        }
    }
}

/// Fields to change; `None` leaves the stored value alone.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    pub playback_quality: Option<String>,
    pub wifi_only: Option<bool>,
    pub offline_enabled: Option<bool>,
    pub max_cache_size_mb: Option<i32>,
    pub share_live_listening: Option<bool>,
}

/// The user's row, or the defaults when there is none. Writes nothing.
pub async fn get(pool: &PgPool, user_id: Uuid) -> sqlx::Result<UserSettings> {
    let row = sqlx::query_as!(
        UserSettings,
        r#"SELECT user_id, playback_quality, wifi_only, offline_enabled, max_cache_size_mb,
                  share_live_listening
           FROM user_settings WHERE user_id = $1"#,
        user_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or_else(|| UserSettings::defaults(user_id)))
}

/// Creates the row from the defaults plus `patch`, or applies `patch` to the existing one.
pub async fn upsert(pool: &PgPool, user_id: Uuid, patch: Patch) -> sqlx::Result<UserSettings> {
    sqlx::query_as!(
        UserSettings,
        r#"INSERT INTO user_settings
               (user_id, playback_quality, wifi_only, offline_enabled, max_cache_size_mb,
                share_live_listening)
           VALUES ($1, COALESCE($2, 'original'), COALESCE($3, false), COALESCE($4, false),
                   COALESCE($5, 10240), COALESCE($6, true))
           ON CONFLICT (user_id) DO UPDATE SET
               playback_quality = COALESCE($2, user_settings.playback_quality),
               wifi_only = COALESCE($3, user_settings.wifi_only),
               offline_enabled = COALESCE($4, user_settings.offline_enabled),
               max_cache_size_mb = COALESCE($5, user_settings.max_cache_size_mb),
               share_live_listening = COALESCE($6, user_settings.share_live_listening)
           RETURNING user_id, playback_quality, wifi_only, offline_enabled, max_cache_size_mb,
                     share_live_listening"#,
        user_id,
        patch.playback_quality,
        patch.wifi_only,
        patch.offline_enabled,
        patch.max_cache_size_mb,
        patch.share_live_listening
    )
    .fetch_one(pool)
    .await
}
