//! The JSON body of every non-2xx `/api` response.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `{"error": "<message>"}`, plus `details` (a string or a list) on validation failures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
