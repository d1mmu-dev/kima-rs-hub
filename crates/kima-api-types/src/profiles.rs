//! Profiles of an account (M-PROF, D-240, D-242, D-243): the `profile` object that login,
//! register and `/auth/me` add to their answers, and the bodies of `/api/profiles*`.

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// What a profile is. A kind this client does not know (a newer server) reads as `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    Main,
    Kids,
    Standard,
    #[serde(other)]
    Unknown,
}

/// A profile as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub kind: ProfileKind,
    /// A preset key; `null` = initial on a colour derived from the id.
    pub avatar: Option<String>,
    pub is_main: bool,
    /// RFC 3339 timestamp.
    pub created_at: String,
}

/// The avatar keys a profile may carry (owner answer Q1: presets, no upload). The client draws
/// each one as an icon on a colour derived from the profile id; `null` is the profile's initial
/// on that colour. Keys only ever get added, so a stored key stays valid.
pub const AVATAR_PRESETS: [&str; 12] = [
    "cat", "dog", "fox", "owl", "bear", "panda", "rabbit", "penguin", "robot", "rocket", "star",
    "music",
];

/// Whether `key` is one of [`AVATAR_PRESETS`].
pub fn is_avatar_preset(key: &str) -> bool {
    AVATAR_PRESETS.contains(&key)
}

/// `GET /api/profiles`: the account's profiles (main first, then by creation), the one the
/// session acts as, and how many the account may hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesResponse {
    pub profiles: Vec<Profile>,
    pub current: Uuid,
    pub max: u32,
}

/// `POST /api/profiles` (main profile only): a kids or standard profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProfile {
    pub name: String,
    pub kind: ProfileKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// `PUT /api/profiles/{id}` (main profile only). An absent field keeps the stored value;
/// `"avatar": null` clears the avatar. `"name": null` and `"kind": null` read as absent (keep
/// the stored value): a name or kind cannot be cleared, so `null` has nothing else to mean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProfileKind>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub avatar: Option<Option<String>>,
}

/// A field that is present, even as `null`, is `Some` (absent stays `None` by `default`).
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// `POST /api/profiles/{id}/switch`. `pin` is accepted and not checked until the optional
/// main-profile PIN exists (D-249); any other field is refused, so the wire shape needs no
/// change when the PIN arrives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwitchProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
}

/// A token pair for the profile switched to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResponse {
    pub token: String,
    pub refresh_token: String,
    pub profile: Profile,
}
