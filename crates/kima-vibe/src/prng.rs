//! mulberry32 and Marsaglia's polar Gaussian, with upstream's call order (`mapLayout.ts:37-55`): the same
//! library always gives the same map. `Math.imul` is `wrapping_mul`, `>>> 0` is `u32`.

use crate::exact_math::log;

/// mulberry32: a small seeded generator returning doubles in `[0, 1)`.
#[derive(Debug, Clone)]
pub struct SeededRandom {
    s: u32,
}

impl SeededRandom {
    pub fn new(seed: u32) -> Self {
        Self { s: seed }
    }

    pub fn next_f64(&mut self) -> f64 {
        self.s = self.s.wrapping_add(0x6d2b_79f5);
        let s = self.s;
        let mut t = (s ^ (s >> 15)).wrapping_mul(1 | s);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

/// A standard normal number by Marsaglia's polar method (needs no trigonometry); `log` is the exact-maths one.
pub fn gaussian(rng: &mut SeededRandom) -> f64 {
    loop {
        let u = 2.0 * rng.next_f64() - 1.0;
        let v = 2.0 * rng.next_f64() - 1.0;
        let s = u * u + v * v;
        if s > 0.0 && s < 1.0 {
            return u * ((-2.0 * log(s)) / s).sqrt();
        }
    }
}
