//! Pure helpers for the media endpoints (spec §4.4, §6.1): the quality choice, the download
//! header, the HLS playlist and the avatar.

use kima_library::transcode::Quality;

/// The lowest MP3 Kima makes (upstream `LOWEST_TIER_KBPS`).
pub const LOWEST_TIER_KBPS: u32 = 128;

/// `transcodeParams` for direct play: the file as it is (upstream `DIRECT_PARAMS`).
pub const DIRECT_PARAMS: &str = "q-original";

/// What the plan needs to know about a song: its listed format (the file's extension, which is
/// also what `getSong` shows) and the bitrate stored at scan (`None` = never probed).
#[derive(Debug, Clone, Copy)]
pub struct SourceInfo<'a> {
    pub suffix: &'a str,
    pub bitrate_kbps: Option<u32>,
}

/// An MP3 Kima makes for a client (upstream `SubsonicServe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Serve {
    pub tier: Quality,
    /// The client asked for MP3: a file in another format is never served as it is.
    pub mp3_only: bool,
    /// The client's cap: a file within it is still served as it is (a FLAC under a 1000 cap).
    pub cap: Option<u32>,
}

/// The one rule for what a song streams as (upstream `planSubsonicSong`, `6166d0a3`), followed by
/// `getTranscodeDecision`, `getTranscodeStream`, `stream` and `hls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The file as it is.
    Direct,
    /// An MP3 Kima makes.
    Mp3(Serve),
    /// Nothing Kima can make meets the request; the reason is for the client.
    Refused(String),
}

/// The highest MP3 tier within a cap (320, 192, 128 kbps). Under 128 there is none: this answers
/// `Low` and the caller decides whether that is acceptable (upstream `tierFor`).
pub fn tier_for(cap: u32) -> Quality {
    match cap {
        320.. => Quality::High,
        192..=319 => Quality::Medium,
        _ => Quality::Low,
    }
}

/// Upstream `AudioStreamingService.servesOriginal`: is the file itself what the client gets?
/// Not when an MP3 was promised and the file is not one; with a tier, when the file's bitrate is
/// known and within the cap (or, without one, the tier's rate). `tier = None` is "original".
fn serves_original(
    source: SourceInfo<'_>,
    tier: Option<Quality>,
    mp3_only: bool,
    cap: Option<u32>,
) -> bool {
    if mp3_only && !source.suffix.eq_ignore_ascii_case("mp3") {
        return false;
    }
    let Some(tier) = tier else { return true };
    let limit = cap.or(tier.bitrate_kbps()).unwrap_or(u32::MAX);
    source.bitrate_kbps.is_some_and(|kbps| kbps <= limit)
}

/// The plan for a request: `format` and `maxBitRate` as the client sent them.
pub fn plan(format: Option<&str>, max_bit_rate: Option<u32>, source: SourceInfo<'_>) -> Plan {
    let wanted = format
        .map(|f| f.trim().to_ascii_lowercase())
        .filter(|f| !f.is_empty());
    let wanted = wanted.as_deref();
    if wanted == Some("raw") {
        return Plan::Direct;
    }
    let cap = max_bit_rate.filter(|&c| c > 0);
    let fits = |mp3_only: bool| serves_original(source, cap.map(tier_for), mp3_only, cap);
    let source_format = source.suffix.to_ascii_lowercase();
    match wanted {
        Some("mp3") => {
            if fits(true) {
                return Plan::Direct;
            }
        }
        Some(w) if w != source_format => {
            return Plan::Refused(format!("Kima transcodes only to mp3; it cannot make {w}"));
        }
        _ => {
            if cap.is_none() || fits(false) {
                return Plan::Direct;
            }
            if let Some(w) = wanted {
                return Plan::Refused(format!(
                    "Kima cannot make {w} smaller; it transcodes only to mp3"
                ));
            }
        }
    }
    if let Some(cap) = cap
        && cap < LOWEST_TIER_KBPS
    {
        return Plan::Refused(format!(
            "the smallest mp3 Kima makes is {LOWEST_TIER_KBPS} kbps, over the {cap} kbps cap"
        ));
    }
    Plan::Mp3(Serve {
        tier: cap.map_or(Quality::High, tier_for),
        mp3_only: wanted == Some("mp3"),
        cap,
    })
}

/// What `stream.view` serves: the same plan, but **a refusal is not an error** (older clients send
/// a format by habit). A refused format is dropped; still refused (a cap under the smallest MP3),
/// the 128 kbps MP3 is made, or the file itself when its stored bitrate is within that.
pub fn plan_for_stream(
    format: Option<&str>,
    max_bit_rate: Option<u32>,
    source: SourceInfo<'_>,
) -> Plan {
    let mut chosen = plan(format, max_bit_rate, source);
    if matches!(chosen, Plan::Refused(_)) {
        chosen = plan(None, max_bit_rate, source);
    }
    if matches!(chosen, Plan::Refused(_)) {
        chosen = Plan::Mp3(Serve {
            tier: Quality::Low,
            mp3_only: false,
            cap: None,
        });
    }
    chosen
}

/// The quality to hand to the transcoder for a plan, settled against the file as upstream does
/// when it serves (`getStreamFilePath`): an MP3 plan still serves the file when the file turns
/// out to be within the cap. Planned before the transcode cache is consulted, so a cached MP3
/// never stands in for the file a decision promised.
pub fn settle(plan: &Plan, source: SourceInfo<'_>) -> Quality {
    match plan {
        Plan::Direct | Plan::Refused(_) => Quality::Original,
        Plan::Mp3(s) if serves_original(source, Some(s.tier), s.mp3_only, s.cap) => {
            Quality::Original
        }
        Plan::Mp3(s) => s.tier,
    }
}

/// `getTranscodeDecision`'s opaque `transcodeParams` for an MP3 it will make:
/// `q-<tier>[-<cap>]`, or `mp3-<tier>[-<cap>]` when the client asked for mp3.
pub fn transcode_params_for(serve: &Serve) -> String {
    let kind = if serve.mp3_only { "mp3" } else { "q" };
    match serve.cap {
        Some(cap) => format!("{kind}-{}-{cap}", serve.tier.name()),
        None => format!("{kind}-{}", serve.tier.name()),
    }
}

/// What a `transcodeParams` value names; `None` unless it is exactly a value a decision hands out
/// (upstream `serveFromTranscodeParams`): `q-original`, or `(q|mp3)-(high|medium|low)[-<1-5 digits>]`
/// whose cap is at least 128 and matches its tier, an uncapped one being only `mp3-high`.
pub fn serve_from_params(value: &str) -> Option<Plan> {
    if value == DIRECT_PARAMS {
        return Some(Plan::Direct);
    }
    let (kind, rest) = value.split_once('-')?;
    let mp3_only = match kind {
        "q" => false,
        "mp3" => true,
        _ => return None,
    };
    let (tier_name, cap) = match rest.split_once('-') {
        Some((t, c)) => (t, Some(c)),
        None => (rest, None),
    };
    let tier = match tier_name {
        "high" => Quality::High,
        "medium" => Quality::Medium,
        "low" => Quality::Low,
        _ => return None,
    };
    let cap = match cap {
        None => None,
        Some(c) if (1..=5).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_digit()) => {
            Some(c.parse::<u32>().ok()?)
        }
        Some(_) => return None,
    };
    let valid = match cap {
        None => mp3_only && tier == Quality::High,
        Some(c) => c >= LOWEST_TIER_KBPS && tier_for(c) == tier,
    };
    valid.then_some(Plan::Mp3(Serve {
        tier,
        mp3_only,
        cap,
    }))
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

    fn src(suffix: &str, kbps: Option<u32>) -> SourceInfo<'_> {
        SourceInfo {
            suffix,
            bitrate_kbps: kbps,
        }
    }

    fn mp3(tier: Quality, mp3_only: bool, cap: Option<u32>) -> Plan {
        Plan::Mp3(Serve {
            tier,
            mp3_only,
            cap,
        })
    }

    /// Replaces `quality_table` (the old rows are re-justified in the comments; the rows whose
    /// answer changed are the ones marked "1.12").
    #[test]
    fn plan_table() {
        use Plan::Direct;
        type Case = (Option<&'static str>, Option<u32>, SourceInfo<'static>, Plan);
        let cases: Vec<Case> = vec![
            // format=raw is the file, whatever the cap (kept).
            (Some("raw"), Some(128), src("mp3", Some(256)), Direct),
            (Some("raw"), None, src("flac", None), Direct),
            // No cap: the file (kept); `maxBitRate=0` is no cap (kept).
            (None, Some(0), src("flac", None), Direct),
            // A file within the cap is the file (kept: `max >= source`).
            (None, Some(320), src("mp3", Some(256)), Direct),
            (None, Some(256), src("mp3", Some(256)), Direct),
            // A file over the cap, or of unknown rate, becomes the highest tier within it (kept
            // for 320/256/192/191; the old `Low` for a cap under 128 is now a refusal, 1.12).
            (
                None,
                Some(320),
                src("flac", None),
                mp3(Quality::High, false, Some(320)),
            ),
            (
                None,
                Some(256),
                src("flac", None),
                mp3(Quality::Medium, false, Some(256)),
            ),
            (
                None,
                Some(192),
                src("flac", None),
                mp3(Quality::Medium, false, Some(192)),
            ),
            (
                None,
                Some(191),
                src("flac", None),
                mp3(Quality::Low, false, Some(191)),
            ),
            (
                None,
                Some(128),
                src("mp3", Some(320)),
                mp3(Quality::Low, false, Some(128)),
            ),
            (
                None,
                Some(255),
                src("mp3", Some(256)),
                mp3(Quality::Medium, false, Some(255)),
            ),
            (
                None,
                Some(64),
                src("flac", None),
                Plan::Refused(
                    "the smallest mp3 Kima makes is 128 kbps, over the 64 kbps cap".into(),
                ),
            ),
            // A file already under a tiny cap still plays (1.12).
            (None, Some(64), src("m4a", Some(64)), Direct),
            // mp3 asked for: always an MP3 (a flac `format=mp3` no cap is `mp3-high`, kept).
            (
                Some("mp3"),
                Some(0),
                src("flac", None),
                mp3(Quality::High, true, None),
            ),
            (Some("mp3"), None, src("mp3", Some(320)), Direct),
            (Some("mp3"), Some(128), src("mp3", Some(128)), Direct),
            (
                Some("mp3"),
                Some(128),
                src("m4a", Some(64)),
                mp3(Quality::Low, true, Some(128)),
            ),
            // The file's own format asked for is the file (kept); under a cap it cannot be made
            // smaller (1.12: refused; before: 320 mp3).
            (Some("flac"), Some(0), src("flac", None), Direct),
            (
                Some("flac"),
                Some(128),
                src("flac", None),
                Plan::Refused("Kima cannot make flac smaller; it transcodes only to mp3".into()),
            ),
            // Any other format is refused (1.12; before: a 320 mp3).
            (
                Some("opus"),
                None,
                src("flac", None),
                Plan::Refused("Kima transcodes only to mp3; it cannot make opus".into()),
            ),
        ];
        for (format, max, source, want) in cases {
            assert_eq!(
                plan(format, max, source),
                want,
                "{format:?} {max:?} {source:?}"
            );
        }
    }

    #[test]
    fn a_refused_stream_falls_back_instead_of_failing() {
        // Format dropped.
        assert_eq!(
            plan_for_stream(Some("opus"), None, src("flac", None)),
            Plan::Direct
        );
        assert_eq!(
            plan_for_stream(Some("opus"), Some(320), src("flac", None)),
            mp3(Quality::High, false, Some(320))
        );
        // Still refused: the smallest MP3, unless the file is within it.
        let low = mp3(Quality::Low, false, None);
        assert_eq!(plan_for_stream(None, Some(64), src("flac", None)), low);
        assert_eq!(settle(&low, src("flac", None)), Quality::Low);
        assert_eq!(settle(&low, src("m4a", Some(100))), Quality::Original);
    }

    #[test]
    fn transcode_params_round_trip_and_forgeries_fail() {
        for serve in [
            Serve {
                tier: Quality::High,
                mp3_only: false,
                cap: Some(320),
            },
            Serve {
                tier: Quality::Medium,
                mp3_only: false,
                cap: Some(256),
            },
            Serve {
                tier: Quality::Low,
                mp3_only: true,
                cap: Some(128),
            },
            Serve {
                tier: Quality::High,
                mp3_only: true,
                cap: None,
            },
        ] {
            let text = transcode_params_for(&serve);
            assert_eq!(serve_from_params(&text), Some(Plan::Mp3(serve)), "{text}");
        }
        assert_eq!(serve_from_params("q-original"), Some(Plan::Direct));
        for bad in [
            "format=mp3",
            "q-bogus",
            "q-",
            "mp3-",
            "x-high",
            "q-high-0",
            "q-high-x",
            "q-high-123456",
            "q-original-128",
            "mp3-original",
            "q-high-100",
            "q-high",
            "mp3-low-100",
            "q-low-64",
            "mp3-medium-320",
            "",
            "x",
            "q-high-+320",
            "q-high-3 0",
        ] {
            assert_eq!(serve_from_params(bad), None, "{bad:?}");
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
