//! Track identification (spec M3a): fingerprints, text rules, and later the identification ladder.

pub mod acoustid;
pub mod chromaprint;
mod ctx;
pub mod matching;
mod mbid;
pub mod musicbrainz;
pub mod stages;
pub mod text;

pub use ctx::{IdentConfig, IdentCtx};
pub use mbid::Mbid;
