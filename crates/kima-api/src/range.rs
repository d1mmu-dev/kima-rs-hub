//! `Range` header parsing for single byte ranges.

/// What a `Range` header asks for, given the file length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// No usable range: serve the whole file with 200.
    Full,
    /// Inclusive byte positions, both inside the file.
    Partial { start: u64, end: u64 },
    /// A well-formed range that lies outside the file: 416.
    Unsatisfiable,
}

/// Parses `bytes=a-b`, `bytes=a-` and `bytes=-n`. Anything else (another unit, several ranges,
/// garbage, `start > end`) is ignored and served in full, as RFC 9110 allows.
pub fn parse(header: &str, len: u64) -> RangeSpec {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    if spec.contains(',') {
        return RangeSpec::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeSpec::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse().unwrap_or(u64::MAX))
    };
    match (num(a), num(b)) {
        (None, Some(n)) if a.is_empty() => {
            if n == 0 || len == 0 {
                RangeSpec::Unsatisfiable
            } else {
                RangeSpec::Partial {
                    start: len.saturating_sub(n),
                    end: len - 1,
                }
            }
        }
        (Some(start), None) if b.is_empty() => {
            if start >= len {
                RangeSpec::Unsatisfiable
            } else {
                RangeSpec::Partial {
                    start,
                    end: len - 1,
                }
            }
        }
        (Some(start), Some(end)) => {
            if start > end {
                RangeSpec::Full
            } else if start >= len {
                RangeSpec::Unsatisfiable
            } else {
                RangeSpec::Partial {
                    start,
                    end: end.min(len - 1),
                }
            }
        }
        _ => RangeSpec::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_when_no_header() {
        assert_eq!(parse("", 1000), RangeSpec::Full);
    }

    #[test]
    fn open_ended() {
        assert_eq!(
            parse("bytes=100-", 1000),
            RangeSpec::Partial {
                start: 100,
                end: 999
            }
        );
    }

    #[test]
    fn closed() {
        assert_eq!(
            parse("bytes=0-99", 1000),
            RangeSpec::Partial { start: 0, end: 99 }
        );
        assert_eq!(
            parse("bytes=0-5000", 1000),
            RangeSpec::Partial { start: 0, end: 999 }
        );
    }

    #[test]
    fn suffix() {
        assert_eq!(
            parse("bytes=-500", 1000),
            RangeSpec::Partial {
                start: 500,
                end: 999
            }
        );
    }

    #[test]
    fn suffix_larger_than_file() {
        assert_eq!(
            parse("bytes=-5000", 1000),
            RangeSpec::Partial { start: 0, end: 999 }
        );
    }

    #[test]
    fn start_beyond_end_unsatisfiable() {
        assert_eq!(parse("bytes=1000-", 1000), RangeSpec::Unsatisfiable);
        assert_eq!(parse("bytes=2000-3000", 1000), RangeSpec::Unsatisfiable);
        assert_eq!(parse("bytes=-0", 1000), RangeSpec::Unsatisfiable);
        assert_eq!(parse("bytes=0-", 0), RangeSpec::Unsatisfiable);
    }

    #[test]
    fn garbage_is_full() {
        for h in [
            "bytes=abc",
            "items=0-5",
            "bytes=5-2",
            "bytes=0-1,5-9",
            "bytes=-",
            "bytes=a-b",
            "bytes=1-2-3",
            "bytes=+1-2",
        ] {
            assert_eq!(parse(h, 1000), RangeSpec::Full, "{h}");
        }
    }
}
