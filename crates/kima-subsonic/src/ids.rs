//! Subsonic ids (spec §6.5): the M1 UUID as a string, with the prefixed forms upstream accepts
//! on some endpoints.

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    /// `ar-<uuid>`
    Ar,
    /// `al-<uuid>`
    Al,
    /// `tr-<uuid>`
    Tr,
    /// `artist:<uuid>`
    ArtistColon,
    /// `album:<uuid>`
    AlbumColon,
}

impl Prefix {
    fn text(self) -> &'static str {
        match self {
            Self::Ar => "ar-",
            Self::Al => "al-",
            Self::Tr => "tr-",
            Self::ArtistColon => "artist:",
            Self::AlbumColon => "album:",
        }
    }
}

/// A bare UUID, or a UUID behind one of the `allow`ed prefixes. Anything else is `None`, which
/// the handler answers with 70.
pub fn parse(raw: &str, allow: &[Prefix]) -> Option<Uuid> {
    let raw = raw.trim();
    let bare = allow
        .iter()
        .find_map(|p| raw.strip_prefix(p.text()))
        .unwrap_or(raw);
    // `Uuid::parse_str` also takes braces, `urn:uuid:` and unhyphenated forms; ids are only ever
    // sent in the hyphenated form.
    if bare.len() != 36 {
        return None;
    }
    Uuid::parse_str(bare).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "00000000-0000-7000-8000-100000000001";

    #[test]
    fn ids_prefixes() {
        let id = Uuid::parse_str(ID).unwrap();
        assert_eq!(parse(ID, &[]), Some(id));
        assert_eq!(parse(&format!("ar-{ID}"), &[Prefix::Ar]), Some(id));
        assert_eq!(
            parse(&format!("artist:{ID}"), &[Prefix::Ar, Prefix::ArtistColon]),
            Some(id)
        );
        assert_eq!(parse(&format!("al-{ID}"), &[Prefix::Al]), Some(id));
        assert_eq!(
            parse(&format!("album:{ID}"), &[Prefix::AlbumColon]),
            Some(id)
        );
        assert_eq!(parse(&format!("tr-{ID}"), &[Prefix::Tr]), Some(id));
        // A prefix that is not allowed here, or the wrong one, is no id.
        assert_eq!(parse(&format!("ar-{ID}"), &[]), None);
        assert_eq!(parse(&format!("ar-{ID}"), &[Prefix::Al]), None);
        assert_eq!(parse(&format!("zz-{ID}"), &[Prefix::Ar]), None);
        assert_eq!(parse("1", &[]), None);
        assert_eq!(parse("", &[Prefix::Ar]), None);
        assert_eq!(parse(&ID.replace('-', ""), &[]), None);
        assert_eq!(parse(&format!("urn:uuid:{ID}"), &[]), None);
    }
}
