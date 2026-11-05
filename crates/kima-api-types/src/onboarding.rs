//! `/api/onboarding/*` response shapes.

use serde::{Deserialize, Serialize};

pub use crate::auth::Credentials;

/// `GET /onboarding/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingStatus {
    pub needs_onboarding: bool,
    pub has_account: bool,
    pub registration_open: bool,
}
