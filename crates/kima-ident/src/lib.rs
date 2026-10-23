//! Track identification (spec M3a): fingerprints, text rules, and later the identification ladder.

pub mod acoustid;
pub mod album_covers;
mod answer;
pub mod artist;
pub mod chromaprint;
pub mod coordinator;
pub mod coverartarchive;
mod ctx;
pub mod deezer;
pub mod fanart;
pub mod listenbrainz;
pub mod matching;
mod mbid;
pub mod musicbrainz;
pub mod similar;
pub mod stages;
pub mod text;
pub mod wikidata;

pub use ctx::{IdentConfig, IdentCtx};
pub use mbid::Mbid;
