//! The shared styling layer: one stylesheet, ported from the legacy Next.js frontend's design tokens.
//!
//! `assets/kima-base.css` holds the tokens and global rules (colours, type roles, motion, the media grid, the
//! page column, ...) copied from `frontend/app/globals.css`; `tools/gen-kima-web-css.mjs` compiles it with
//! Tailwind v4 into `assets/kima.css`, scanning this crate's sources for class names. The compiled file is
//! checked in, so building kima-web needs neither Node nor Tailwind. Views then use the same utility classes
//! as the legacy components (`class="flex items-center gap-3 ..."`), hover/focus states included.
//!
//! Montserrat (SIL OFL 1.1) and the logo mask are embedded as data URIs so the page needs no extra files
//! (the SSR proxy serves only the wasm and its loader) and the font is there on first paint.

use std::sync::OnceLock;

const CSS: &str = include_str!("../assets/kima.css");
const FONT: &[u8] = include_bytes!("../assets/fonts/montserrat-latin.woff2");
const MARK: &[u8] = include_bytes!("../assets/kima.webp");

/// The logo's gradient (`frontend/lib/brandGradient.ts`), amber at the top to azure at the bottom.
pub const BRAND_GRADIENT: &str = "linear-gradient(180deg, #fca506 2%, #fdac15 9%, #fdb530 17%, #fcb253 24%, #f69472 31%, #e86690 39%, #cd45aa 46%, #a633c2 54%, #7731d2 61%, #483ad9 69%, #234ed9 76%, #0e6bd3 83%, #0583cd 91%, #0191c9 98%)";

/// Standard base64 (RFC 4648, padded).
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The whole stylesheet, with the embedded font and logo mask filled in. Built once.
pub fn stylesheet() -> &'static str {
    static SHEET: OnceLock<String> = OnceLock::new();
    SHEET.get_or_init(|| {
        CSS.replace(
            "__MONTSERRAT__",
            &format!("\"data:font/woff2;base64,{}\"", base64(FONT)),
        )
        .replace(
            "__KIMA_MARK__",
            &format!("\"data:image/webp;base64,{}\"", base64(MARK)),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn placeholders_are_all_replaced() {
        let css = stylesheet();
        assert!(!css.contains("__MONTSERRAT__") && !css.contains("__KIMA_MARK__"));
        assert!(css.contains("data:font/woff2;base64,"));
    }
}
