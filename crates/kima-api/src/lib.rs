//! The `/api` HTTP surface the frontend talks to. Thin: logic lives in the domain crates.

pub mod auth;
pub mod error;
pub mod extract;
mod routes;
pub mod state;

pub use routes::router;
pub use state::AppState;
