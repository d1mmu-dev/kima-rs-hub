//! MusicBrainz ids.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use uuid::Uuid;

/// A MusicBrainz id. Serialised as the hyphenated lower-case string; deserialised with the same
/// rule as [`Mbid::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mbid(pub Uuid);

impl Mbid {
    /// The trimmed value must be the 36-character hyphenated form (any case) and not the nil
    /// UUID; braced, `urn:uuid:` and 32-hex forms are refused (upstream `isMbid`, and
    /// `kima_library::tags::parse_mbid`).
    pub fn parse(s: &str) -> Option<Mbid> {
        kima_library::tags::parse_mbid(s).map(Mbid)
    }
}

impl fmt::Display for Mbid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl Serialize for Mbid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.0.hyphenated())
    }
}

impl<'de> Deserialize<'de> for Mbid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Mbid::parse(&s).ok_or_else(|| serde::de::Error::custom("not a MusicBrainz id"))
    }
}
