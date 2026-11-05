//! Shared helpers for the integration tests (included with `mod common;`).
#![allow(dead_code)]

use std::path::PathBuf;

pub fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A golden f64 stored as 16 hex digits of its IEEE-754 bit pattern.
pub fn from_bits_hex(h: &str) -> f64 {
    f64::from_bits(
        u64::from_str_radix(h, 16).unwrap_or_else(|e| panic!("bad bit pattern {h:?}: {e}")),
    )
}

/// Bit equality, except that any NaN equals any NaN (engines canonicalise NaN payloads differently).
pub fn same_bits(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

/// Distance in units in the last place between two finite, same-signed-or-zero doubles.
pub fn ulps(a: f64, b: f64) -> u64 {
    fn key(x: f64) -> i64 {
        let b = x.to_bits() as i64;
        if b < 0 { i64::MIN - b } else { b }
    }
    key(a).abs_diff(key(b))
}

/// A tiny xorshift generator, so the tests need no dev-dependency and are reproducible.
pub struct Xorshift(pub u64);
impl Xorshift {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}
