//! `GET /api/settings`: the signed-in user's own settings. The M9 Settings page only shows the
//! account slice (`auth::MeResponse`, `auth::ChangePassword`); this shape is the rest of that endpoint.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSettings {
    pub user_id: Uuid,
    /// `original`, `high`, `medium` or `low`.
    pub playback_quality: String,
    pub wifi_only: bool,
    pub offline_enabled: bool,
    pub max_cache_size_mb: i32,
    pub share_live_listening: bool,
    /// Always `false` in kima-rs.
    pub has_listenbrainz_token: bool,
    /// Always `false` in kima-rs.
    pub live_sharing_available: bool,
}
