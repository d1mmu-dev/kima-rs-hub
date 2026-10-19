use axum::Router;
use axum::routing::get;

use crate::error::ApiError;
use crate::state::AppState;

mod api_keys;
mod auth;
mod cover_art;
mod events;
mod health;
mod library;
mod library_watch;
mod onboarding;
mod playback_state;
mod playlists;
mod plays;
mod search;
mod settings;
mod stubs;
mod system;
mod system_settings;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health::health))
        .merge(onboarding::routes())
        .merge(auth::routes())
        .merge(api_keys::routes())
        .merge(system::routes())
        .merge(stubs::routes())
        .merge(system_settings::routes())
        .merge(events::routes())
        .merge(cover_art::routes())
        .merge(library_watch::routes())
        .merge(library::routes())
        .merge(search::routes())
        .merge(plays::routes())
        .merge(playlists::routes())
        .merge(playback_state::routes())
        .merge(settings::routes())
        .fallback(|| async { ApiError::NotFound("Not found") });

    Router::new()
        .route("/health", get(health::health))
        .nest("/api", api)
        .with_state(state)
}
