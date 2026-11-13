//! Outbound HTTP (spec §7): the only way kima talks to the internet. One [`Net`] per process,
//! with per-source limits, persisted pauses, a User-Agent and the network guard. Knows sources
//! and limits but nothing about music.

mod audio;
pub mod cache;
mod client;
mod feeds;
mod fetch;
pub mod gate;
mod guard;
mod images;
mod outcome;
pub mod record;
mod req;
mod secrets;
mod source;
pub mod ssrf;
#[cfg(feature = "testing")]
pub mod testing;

pub use audio::AudioResponse;
pub use client::{
    Net, NetConfig, audio_user_agent, feed_user_agent, redirect_allowed, retry_delay, user_agent,
};
pub use feeds::{Conditional, FeedFetch, MAX_FEED, same_feed};
pub use outcome::{Evidence, Failure, Outcome};
pub use req::Req;
pub use source::{Bases, COMMONS_BASE, Host, ITUNES_BASE, Limits, Source};
