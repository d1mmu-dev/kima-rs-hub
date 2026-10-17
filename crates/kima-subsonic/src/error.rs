//! Subsonic errors: a code and a message, rendered into the envelope by `render_layer`.

use std::fmt::Debug;

use axum::response::{IntoResponse, Response};
use kima_api::error::ApiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicError {
    pub code: u16,
    pub message: String,
}

impl SubsonicError {
    pub fn new(code: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn missing(param: &str) -> Self {
        Self::new(10, format!("Required parameter is missing: {param}"))
    }

    pub fn invalid(param: &str) -> Self {
        Self::new(10, format!("Invalid parameter: {param}"))
    }

    pub fn wrong_credentials() -> Self {
        Self::new(40, "Wrong username or password")
    }

    pub fn invalid_api_key() -> Self {
        Self::new(44, "Invalid API key")
    }

    pub fn not_authorized(message: impl Into<String>) -> Self {
        Self::new(50, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(70, message)
    }

    pub fn generic(message: impl Into<String>) -> Self {
        Self::new(0, message)
    }

    /// Logs the cause; the client only ever sees `Internal error`.
    pub fn internal(error: impl Debug) -> Self {
        tracing::error!(error = ?error, "internal error");
        Self::generic("Internal error")
    }
}

impl From<sqlx::Error> for SubsonicError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error)
    }
}

impl From<ApiError> for SubsonicError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::NotFound(message) => Self::not_found(message),
            other => Self::internal(other),
        }
    }
}

/// An empty 200 carrying the error; `render_layer` writes the envelope.
impl IntoResponse for SubsonicError {
    fn into_response(self) -> Response {
        let mut response = Response::new(axum::body::Body::empty());
        response.extensions_mut().insert(self);
        response
    }
}
