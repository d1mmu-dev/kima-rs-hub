use axum::Router;
use axum::routing::get;

use crate::error::ApiError;
use crate::state::AppState;

mod auth;
mod events;
mod health;
mod onboarding;
mod system;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health::health))
        .merge(onboarding::routes())
        .merge(auth::routes())
        .merge(system::routes())
        .merge(events::routes())
        .fallback(|| async { ApiError::NotFound("Not found") });

    Router::new()
        .route("/health", get(health::health))
        .nest("/api", api)
        .with_state(state)
}
