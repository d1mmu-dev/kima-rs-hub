use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 400 `{"error":"Invalid request","details":...}`: the body failed validation.
    #[error("Invalid request")]
    Invalid(String),
    /// 400 `{"error": message, "details": [...]}` with a list of failures.
    #[error("{message}")]
    InvalidWithDetails {
        message: String,
        details: Vec<serde_json::Value>,
    },
    /// 400 `{"error":"Invalid settings","details":"<detail>"}` (system settings).
    #[error("Invalid settings")]
    InvalidSettings(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(&'static str),
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("{0}")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    /// 409 with a message built at run time (the MetadataEditor's taken MBID).
    #[error("{0}")]
    ConflictMessage(String),
    #[error("{0}")]
    TooManyRequests(&'static str),
    /// 429 with `Retry-After`: the failed-login limiter refused the attempt.
    #[error("Too many failed login attempts. Try again in {retry_after_secs} seconds.")]
    RateLimited { retry_after_secs: u64 },
    #[error("Request body too large")]
    PayloadTooLarge,
    #[error("Internal error")]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Invalid(_)
            | Self::InvalidSettings(_)
            | Self::InvalidWithDetails { .. }
            | Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) | Self::ConflictMessage(_) => StatusCode::CONFLICT,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Internal(error) => {
                tracing::error!(error = ?error, "internal error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = match &self {
            Self::Invalid(details) | Self::InvalidSettings(details) => {
                json!({ "error": self.to_string(), "details": details })
            }
            Self::InvalidWithDetails { details, .. } => {
                json!({ "error": self.to_string(), "details": details })
            }
            _ => json!({ "error": self.to_string() }),
        };
        let mut response = (status, Json(body)).into_response();
        if let Self::RateLimited { retry_after_secs } = &self {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, (*retry_after_secs).into());
        }
        response
    }
}
