//! The `/api` HTTP surface the frontend talks to. Thin: logic lives in the domain crates.

pub mod auth;
pub mod error;
pub mod events;
pub mod extract;
pub mod login_limit;
pub mod media;
pub mod range;
mod routes;
pub mod state;

pub use routes::router;
pub use state::{AppState, LibraryServices};

/// Ends the open event streams and pending tickets of every profile of an account. Call after
/// bumping its `token_version` (or deleting it).
pub fn revoke_sessions(state: &AppState, account: kima_core::profile::AccountId) {
    state.sse.revoke_account(account);
    state
        .events
        .publish(kima_core::events::Event::revoke_account(account));
}
