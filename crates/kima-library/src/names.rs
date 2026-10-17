//! Name helpers: identity keys, Various Artists spellings and capitalisation choice.

use unicode_normalization::UnicodeNormalization;

pub const VARIOUS_ARTISTS: &str = "Various Artists";

/// Identity key: NFC, trimmed, whitespace collapsed, lowercase.
pub fn key(s: &str) -> String {
    let nfc: String = s.nfc().collect();
    nfc.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Maps the common spellings of "Various Artists" to the canonical name.
///
/// Covers upstream's `canonicalizeVariousArtists` (VA, V.A., V/A, Various, Various Artist(s),
/// `<Various Artists>`) plus "Varios Artistas". Case-insensitive.
pub fn canonical_various(s: &str) -> Option<&'static str> {
    let lower = s.trim().to_lowercase();
    let t = lower
        .strip_prefix('<')
        .unwrap_or(&lower)
        .trim_end_matches('>');
    if matches!(
        t,
        "various" | "various artist" | "various artists" | "varios artistas"
    ) {
        return Some(VARIOUS_ARTISTS);
    }
    // Upstream `^v\.?\s*[\/.]?\s*a\.?$`
    let mut it = t.chars().peekable();
    if it.next() != Some('v') {
        return None;
    }
    if it.peek() == Some(&'.') {
        it.next();
    }
    while it.peek().is_some_and(|c| c.is_whitespace()) {
        it.next();
    }
    if matches!(it.peek(), Some('/') | Some('.')) {
        it.next();
    }
    while it.peek().is_some_and(|c| c.is_whitespace()) {
        it.next();
    }
    if it.next() != Some('a') {
        return None;
    }
    if it.peek() == Some(&'.') {
        it.next();
    }
    it.next().is_none().then_some(VARIOUS_ARTISTS)
}

/// Which spelling of the same name to display: keep `stored` unless it starts lowercase and
/// `new` starts uppercase (upstream `musicScanner.ts:786`, "prefer better capitalisation").
pub fn better_caps<'a>(stored: &'a str, new: &'a str) -> &'a str {
    let lower = stored.chars().next().is_some_and(|c| c.is_lowercase());
    let upper = new.chars().next().is_some_and(|c| c.is_uppercase());
    if stored != new && lower && upper {
        new
    } else {
        stored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_normalises() {
        assert_eq!(key("  Sigur   R\u{f3}s "), "sigur r\u{f3}s");
        assert_eq!(key("Sigur Ro\u{301}s"), key("Sigur R\u{f3}s"));
        assert_eq!(key("A\tB"), "a b");
    }

    #[test]
    fn upstream_canonical_various() {
        for s in [
            "VA",
            "V.A.",
            "V/A",
            "V.A",
            "Various",
            "Various Artist",
            "Various Artists",
            "<Various Artists>",
            "varios artistas",
            "VARIOUS",
        ] {
            assert_eq!(canonical_various(s), Some("Various Artists"), "{s}");
        }
        for s in [
            "Radiohead",
            "Vampire Weekend",
            "Vera",
            "",
            "V",
            "Various Cruelties",
        ] {
            assert_eq!(canonical_various(s), None, "{s}");
        }
    }

    #[test]
    fn better_caps_prefers_capitalised() {
        assert_eq!(better_caps("radiohead", "Radiohead"), "Radiohead");
        assert_eq!(better_caps("Radiohead", "radiohead"), "Radiohead");
        assert_eq!(better_caps("deadmau5", "Deadmau5"), "Deadmau5");
        assert_eq!(better_caps("Tool", "TOOL"), "Tool");
    }
}
