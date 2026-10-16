//! The `/api` HTTP surface the frontend talks to. Thin: logic lives in the domain crates.

pub mod auth;
pub mod error;
pub mod events;
pub mod extract;
mod routes;
pub mod state;

pub use routes::router;
pub use state::AppState;

/// Ends a user's open event streams and pending tickets. Call after bumping their `token_version`.
pub fn revoke_sessions(state: &AppState, user: uuid::Uuid) {
    state.sse.revoke_user(user);
    state.events.publish(kima_core::events::Event::revoke(user));
}
