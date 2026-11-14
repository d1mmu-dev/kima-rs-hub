//! Profiles of an account (M-PROF, D-240, D-242): the `profile` object that login, register
//! and `/auth/me` add to their answers.

use serde::{Deserialize, Serialize};
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
