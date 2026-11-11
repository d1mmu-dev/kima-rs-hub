pub enum Arg {
    Missing,
    One(String),
    Many(Vec<String>),
}
pub struct Args(pub Vec<(String, String)>);
impl Args {
    pub fn get(&self, key: &str) -> Arg {
        let values: Vec<_> = self
            .0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect();
        match values.len() {
            0 => Arg::Missing,
            1 => Arg::One(values.into_iter().next().unwrap()),
            _ => Arg::Many(values),
        }
    }
}
impl Arg {
    pub fn string(&self) -> Option<&str> {
        if let Self::One(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn number(&self) -> f64 {
        match self {
            Self::Missing => f64::NAN,
            Self::One(s) => js_number(s),
            Self::Many(v) => js_number(&v.join(",")),
        }
    }
    pub fn js_string(&self) -> String {
        match self {
            Self::Missing => String::new(),
            Self::One(s) => s.clone(),
            Self::Many(v) => v.join(","),
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Self::Missing => false,
            Self::One(s) => !s.is_empty(),
            Self::Many(_) => true,
        }
    }
}

pub fn js_number(s: &str) -> f64 {
    // ECMAScript whitespace differs from Rust's (notably FEFF and U+0085).
    let s = s.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'));
    if s.is_empty() {
        return 0.0;
    }
    match s {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if s.len() >= 2 && s.as_bytes()[0] == b'0' {
        let base = match s.as_bytes()[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if base != 0 {
            let digits = &s[2..];
            if digits.is_empty() {
                return f64::NAN;
            }
            let mut mantissa = 0_u64;
            let mut count = 0_usize;
            let mut guard = false;
            let mut sticky = false;
            for c in digits.chars() {
                let Some(d) = c.to_digit(base) else {
                    return f64::NAN;
                };
                for shift in (0..base.ilog2()).rev() {
                    let bit = (d >> shift) & 1;
                    if count == 0 && bit == 0 {
                        continue;
                    }
                    count += 1;
                    if count <= 53 {
                        mantissa = (mantissa << 1) | u64::from(bit);
                    } else if count == 54 {
                        guard = bit != 0;
                    } else {
                        sticky |= bit != 0;
                    }
                }
            }
            if guard && (sticky || mantissa & 1 != 0) {
                mantissa += 1;
            }
            let exponent = count.saturating_sub(53);
            return if exponent > 971 {
                f64::INFINITY
            } else {
                (mantissa as f64) * 2_f64.powi(exponent as i32)
            };
        }
    }
    let bytes = s.as_bytes();
    let mut i = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
        i += 1;
        if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return f64::NAN;
        }
    }
    if i != bytes.len() {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_key_is_an_array() {
        let args = Args(vec![("dow".into(), "1".into()), ("dow".into(), "2".into())]);
        let a = args.get("dow");
        assert!(a.number().is_nan());
        assert_eq!(a.js_string(), "1,2");
        assert_eq!(a.string(), None);
        assert!(a.truthy());
        let a = args.get("other");
        assert!(a.number().is_nan());
        assert!(!a.truthy());
        assert_eq!(a.js_string(), "");
        let a = Arg::One("".into());
        assert!(!a.truthy());
        assert_eq!(a.number(), 0.0);
    }
    #[test]
    fn js_number_matches_js() {
        for (input, expected) in [
            ("", 0.0),
            ("  4 ", 4.0),
            ("4.0", 4.0),
            ("1e1", 10.0),
            ("0x10", 16.0),
            ("+.5", 0.5),
            ("\u{feff}", 0.0),
            ("0b11", 3.0),
            ("0o10", 8.0),
        ] {
            assert_eq!(js_number(input), expected, "{input:?}");
        }
        for input in ["-0x10", "inf", "nan", "4a", ".", "1_0", "\u{85}"] {
            assert!(js_number(input).is_nan(), "{input:?}");
        }
        assert_eq!(js_number("Infinity"), f64::INFINITY);
        // Number() rounds the complete integer, not each hexadecimal digit.
        assert_eq!(js_number("0x20000000000001ff"), 2305843009213694500.0);
    }
}
