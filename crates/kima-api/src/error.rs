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
    /// 503 with `Retry-After: 1`: the database picked this request as a deadlock or
    /// serialization victim, so it changed nothing and may be sent again.
    #[error("Temporarily busy, please retry")]
    Busy,
    #[error("Internal error")]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        if kima_db::is_retryable_conflict(&error) {
            return Self::Busy;
        }
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
            Self::Busy => StatusCode::SERVICE_UNAVAILABLE,
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
        if matches!(self, Self::Busy) {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, 1.into());
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct FakeDbError(&'static str);

    impl fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "relation \"secret_table\" deadlock detail")
        }
    }

    impl std::error::Error for FakeDbError {}

    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            "relation \"secret_table\" deadlock detail"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    async fn answer(code: &'static str) -> (StatusCode, Option<String>, serde_json::Value) {
        let error = ApiError::from(sqlx::Error::Database(Box::new(FakeDbError(code))));
        let response = error.into_response();
        let status = response.status();
        let retry = response
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        (status, retry, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn deadlock_and_serialization_answer_a_retryable_503() {
        for code in ["40P01", "40001"] {
            let (status, retry, body) = answer(code).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{code}");
            assert_eq!(retry.as_deref(), Some("1"), "{code}");
            assert_eq!(
                body,
                json!({ "error": "Temporarily busy, please retry" }),
                "{code}"
            );
        }
    }

    #[tokio::test]
    async fn other_database_errors_stay_500_without_detail() {
        let (status, retry, body) = answer("23505").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(retry, None);
        assert_eq!(body, json!({ "error": "Internal error" }));
    }
}
