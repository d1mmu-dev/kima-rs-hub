use axum::Router;
use axum::routing::get;

use crate::error::ApiError;
use crate::state::AppState;

mod auth;
mod cover_art;
mod events;
mod health;
mod library_watch;
mod onboarding;
mod system;
mod system_settings;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health::health))
        .merge(onboarding::routes())
        .merge(auth::routes())
        .merge(system::routes())
        .merge(system_settings::routes())
        .merge(events::routes())
        .merge(cover_art::routes())
        .merge(library_watch::routes())
        .fallback(|| async { ApiError::NotFound("Not found") });

    Router::new()
        .route("/health", get(health::health))
        .nest("/api", api)
        .with_state(state)
}
