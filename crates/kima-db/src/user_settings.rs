//! Per-profile settings (upstream `UserSettings`, per user there; D-240): playback quality and client preferences.

use kima_core::profile::ProfileId;
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSettings {
    pub profile_id: ProfileId,
    pub playback_quality: String,
    pub wifi_only: bool,
    pub offline_enabled: bool,
    pub max_cache_size_mb: i32,
    pub share_live_listening: bool,
    /// `hide` (default) or `show`: whether podcast lists hide AI-labelled content (D-207).
    pub podcast_ai_filter: String,
}

impl UserSettings {
    /// What a profile without a row has (the column defaults).
    pub fn defaults(profile_id: ProfileId) -> Self {
        Self {
            profile_id,
            playback_quality: "original".into(),
            wifi_only: false,
            offline_enabled: false,
            max_cache_size_mb: 10240,
            share_live_listening: true,
            podcast_ai_filter: "hide".into(),
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
    pub podcast_ai_filter: Option<String>,
}

/// The profile's row, or the defaults when there is none. Writes nothing.
pub async fn get(pool: &PgPool, profile: ProfileId) -> sqlx::Result<UserSettings> {
    let row = sqlx::query_as!(
        UserSettings,
        r#"SELECT profile_id AS "profile_id: ProfileId", playback_quality, wifi_only, offline_enabled, max_cache_size_mb,
                  share_live_listening, podcast_ai_filter
           FROM user_settings WHERE profile_id = $1"#,
        profile.into_uuid()
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or_else(|| UserSettings::defaults(profile)))
}

/// Creates the row from the defaults plus `patch`, or applies `patch` to the existing one.
pub async fn upsert(pool: &PgPool, profile: ProfileId, patch: Patch) -> sqlx::Result<UserSettings> {
    sqlx::query_as!(
        UserSettings,
        r#"INSERT INTO user_settings
               (profile_id, playback_quality, wifi_only, offline_enabled, max_cache_size_mb,
                share_live_listening, podcast_ai_filter)
           VALUES ($1, COALESCE($2, 'original'), COALESCE($3, false), COALESCE($4, false),
                   COALESCE($5, 10240), COALESCE($6, true), COALESCE($7, 'hide'))
           ON CONFLICT (profile_id) DO UPDATE SET
               playback_quality = COALESCE($2, user_settings.playback_quality),
               wifi_only = COALESCE($3, user_settings.wifi_only),
               offline_enabled = COALESCE($4, user_settings.offline_enabled),
               max_cache_size_mb = COALESCE($5, user_settings.max_cache_size_mb),
               share_live_listening = COALESCE($6, user_settings.share_live_listening),
               podcast_ai_filter = COALESCE($7, user_settings.podcast_ai_filter)
           RETURNING profile_id AS "profile_id: ProfileId", playback_quality, wifi_only, offline_enabled, max_cache_size_mb,
                     share_live_listening, podcast_ai_filter"#,
        profile.into_uuid(),
        patch.playback_quality,
        patch.wifi_only,
        patch.offline_enabled,
        patch.max_cache_size_mb,
        patch.share_live_listening,
        patch.podcast_ai_filter
    )
    .fetch_one(pool)
    .await
}

/// Whether the profile's podcast lists hide AI-labelled content: `true` unless they chose `show`.
pub async fn hides_podcast_ai(pool: &PgPool, profile: ProfileId) -> sqlx::Result<bool> {
    Ok(get(pool, profile).await?.podcast_ai_filter != "show")
}
