use axum::Router;
use axum::routing::get;

use crate::error::ApiError;
use crate::state::AppState;

mod health;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health::health))
        .fallback(|| async { ApiError::NotFound("Not found") });

    Router::new()
        .route("/health", get(health::health))
        .nest("/api", api)
        .with_state(state)
}
