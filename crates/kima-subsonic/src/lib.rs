//! The Subsonic / OpenSubsonic API, mounted by `kima-server` under `/rest`.

use axum::Router;
use axum::middleware::from_fn;
use kima_api::AppState;

pub mod auth;
pub mod ctx;
pub mod error;
pub mod ids;
pub mod map;
pub mod media;
pub mod now_playing;
pub mod params;
pub mod response;
mod routes;

use ctx::SubsonicCtx;

/// The `/rest` routes with their state applied. Mount with `.nest("/rest", router(state))`.
pub fn router(app: AppState) -> Router {
    // Layers added last run first: parameters are parsed before the envelope is rendered.
    routes::router()
        .layer(from_fn(response::render_layer))
        .layer(from_fn(params::params_layer))
        .with_state(SubsonicCtx::new(app))
}
