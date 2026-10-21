//! Outbound HTTP (spec §7): the only way kima talks to the internet. One [`Net`] per process,
//! with per-source limits, persisted pauses, a User-Agent and the network guard. Knows sources
//! and limits but nothing about music.

mod client;
pub mod gate;
mod guard;
mod outcome;
mod req;
mod source;
#[cfg(feature = "testing")]
pub mod testing;

pub use client::{Net, NetConfig, redirect_allowed, retry_delay, user_agent};
pub use outcome::{Evidence, Failure, Outcome};
pub use req::Req;
pub use source::{Bases, Host, Limits, Source};
