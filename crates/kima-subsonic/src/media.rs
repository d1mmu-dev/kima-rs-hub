//! Pure helpers for the media endpoints (spec §4.4, §6.1): the quality choice, the download
//! header, the HLS playlist and the avatar.

use kima_library::transcode::Quality;

/// Chooses the stream quality (spec §6.1, D-085).
///
/// 1. `format=raw` is the original.
/// 2. No `maxBitRate` (or 0), or one at least the source bitrate (unknown counts as above 320), is
///    the original, unless `format` names a codec other than the source's: then rule 3 applies
///    with 320 as the cap when `maxBitRate` is 0.
/// 3. Otherwise the highest of 320, 192 and 128 kbps not above `maxBitRate`, and 128 below that.
pub fn quality(
    format: Option<&str>,
    max_bit_rate: Option<u32>,
    source_kbps: Option<u32>,
    source_suffix: &str,
) -> Quality {
    let format = format.map(|f| f.trim().to_ascii_lowercase());
    if format.as_deref() == Some("raw") {
        return Quality::Original;
    }
    let max = max_bit_rate.unwrap_or(0);
    let other_codec = format
        .as_deref()
        .is_some_and(|f| !f.is_empty() && f != source_suffix.to_ascii_lowercase());
    let cap = if max == 0 {
        if !other_codec {
            return Quality::Original;
        }
        320
    } else {
        if source_kbps.is_some_and(|s| max >= s) && !other_codec {
            return Quality::Original;
        }
        max
    };
    match cap {
        320.. => Quality::High,
        192..=319 => Quality::Medium,
        _ => Quality::Low,
    }
}

/// Percent-encodes `name` over RFC 5987's `attr-char`, so the result is ASCII with no quote,
/// semicolon, space or percent left raw.
pub fn rfc5987(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            )
        {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `Content-Disposition` value for a download.
pub fn content_disposition(file_name: &str) -> String {
    format!("attachment; filename*=UTF-8''{}", rfc5987(file_name))
}

/// The one-entry playlist `hls` answers. Control characters are dropped from the title so an
/// `id` cannot add playlist lines.
pub fn hls_playlist(id: &str, stream_url: &str) -> String {
    let title: String = id.chars().filter(|c| !c.is_control()).collect();
    format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:-1,{title}\n{stream_url}\n")
}

/// A 1x1 fully transparent RGBA PNG. Upstream sends the same picture, but its bytes fail the PNG
/// checksum, so this one is regenerated with valid chunks.
pub const TRANSPARENT_PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x60, 0x60, 0x60, 0x60,
    0x00, 0x00, 0x00, 0x05, 0x00, 0x01, 0x7a, 0xa8, 0x57, 0x50, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[cfg(test)]
mod tests {
    use super::*;
    use Quality::*;

    #[test]
    fn quality_table() {
        type Case = (
            Option<&'static str>,
            Option<u32>,
            Option<u32>,
            &'static str,
            Quality,
        );
        let cases: [Case; 14] = [
            (Some("raw"), Some(128), Some(256), "mp3", Original),
            (Some("raw"), None, None, "flac", Original),
            (None, Some(0), None, "flac", Original),
            (None, Some(320), Some(256), "mp3", Original),
            (None, Some(320), None, "flac", High),
            (None, Some(256), None, "flac", Medium),
            (None, Some(192), None, "flac", Medium),
            (None, Some(191), None, "flac", Low),
            (None, Some(64), None, "flac", Low),
            (Some("mp3"), Some(0), None, "flac", High),
            (Some("flac"), Some(0), None, "flac", Original),
            (None, Some(128), Some(320), "mp3", Low),
            (None, Some(256), Some(256), "mp3", Original),
            (None, Some(255), Some(256), "mp3", Medium),
        ];
        for (format, max, kbps, suffix, want) in cases {
            assert_eq!(
                quality(format, max, kbps, suffix),
                want,
                "{format:?} {max:?} {kbps:?} {suffix}"
            );
        }
    }

    #[test]
    fn rfc5987_encodes_everything_but_attr_chars() {
        assert_eq!(rfc5987("a b'c\";d%e*"), "a%20b%27c%22%3Bd%25e%2A");
        assert_eq!(rfc5987("Bj\u{f6}rk"), "Bj%C3%B6rk");
        assert_eq!(rfc5987("ok-1_2.~"), "ok-1_2.~");
    }

    #[test]
    fn hls_playlist_has_no_injected_lines() {
        let list = hls_playlist("a\r\n#EXT-X-ENDLIST", "http://h/rest/stream.view?id=a");
        assert_eq!(list.lines().count(), 4);
        assert!(!list.contains('\r'));
    }
}
