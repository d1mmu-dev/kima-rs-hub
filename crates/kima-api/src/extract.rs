use axum::Json;
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// `Json<T>` that answers malformed bodies with the API's JSON 400 instead of axum's plain text.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| ApiJson(value))
            .map_err(|rejection| {
                if rejection.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                    ApiError::PayloadTooLarge
                } else {
                    ApiError::Invalid(rejection.body_text())
                }
            })
    }
}
