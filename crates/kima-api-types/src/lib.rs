//! Wire-format DTOs shared by `kima-api` and `kima-web`.
//!
//! Dependency-light on purpose: it must build for `wasm32-unknown-unknown`. JSON is camelCase on
//! `/api` and errors are `{"error": "<message>"}`; these types mirror that, never redefine it.

pub mod auth;
pub mod error;
pub mod library;
pub mod onboarding;
pub mod playback_state;
pub mod playlists;
pub mod search;
pub mod settings;
pub mod stats;
