//! `exp`, `log`, `pow` and `atan2` built only from operations IEEE 754 fixes exactly (+, -, *, /, sqrt and bit access),
//! so the vibe map comes out bit for bit the same on every platform. Rust's `f64::exp`/`ln`/`atan2` call the platform
//! libm and may differ in the last ulp.
//!
//! This is a verbatim port of upstream kima-hub `backend/src/services/matching/exactMath.ts` (v1.12.3,
//! c7527577), which itself ports fdlibm's `e_exp.c`, `e_log.c`, `s_atan.c` and `e_atan2.c`. Do not reorder float
//! operations or fuse them: `tests/exact_math.rs` compares against bit patterns produced by the TypeScript code.

#![allow(clippy::excessive_precision, clippy::approx_constant)] // constants are fdlibm's, kept digit for digit

/// The high word of `x` as a signed 32-bit integer (fdlibm `__HI`).
fn high(x: f64) -> i32 {
    (x.to_bits() >> 32) as u32 as i32
}

/// The low word of `x` (fdlibm `__LO`).
fn low(x: f64) -> u32 {
    x.to_bits() as u32
}

fn from_words(hi: i32, lo: u32) -> f64 {
    f64::from_bits(((hi as u32 as u64) << 32) | lo as u64)
}

fn with_high(x: f64, hi: i32) -> f64 {
    f64::from_bits(((hi as u32 as u64) << 32) | (x.to_bits() & 0xffff_ffff))
}

const LN2_HI: f64 = 6.93147180369123816490e-1;
const LN2_LO: f64 = 1.90821492927058770002e-10;
const INV_LN2: f64 = 1.44269504088896338700e0;
const TWO_M1000: f64 = 9.33263618503218878990e-302;
const P1: f64 = 1.66666666666666019037e-1;
const P2: f64 = -2.77777777770155933842e-3;
const P3: f64 = 6.61375632143793436117e-5;
const P4: f64 = -1.65339022054652515390e-6;
const P5: f64 = 4.13813679705723846039e-8;

/// e^x (fdlibm `e_exp.c`).
pub fn exp(mut x: f64) -> f64 {
    let mut hx = high(x);
    let sign = (((hx as u32) >> 31) & 1) as i32;
    hx &= 0x7fff_ffff;
    if hx >= 0x4086_2e42 {
        if hx >= 0x7ff0_0000 {
            if (((hx & 0xfffff) as u32) | low(x)) != 0 {
                return x + x;
            }
            return if sign == 0 { x } else { 0.0 };
        }
        if x > 7.097_827_128_933_839_730_96e2 {
            return f64::INFINITY;
        }
        if x < -7.451_332_191_019_411_084_20e2 {
            return 0.0;
        }
    }
    let mut hi = 0.0;
    let mut lo = 0.0;
    let mut k: i32 = 0;
    if hx > 0x3fd6_2e42 {
        if hx < 0x3ff0_a2b2 {
            hi = x - if sign == 0 { LN2_HI } else { -LN2_HI };
            lo = if sign == 0 { LN2_LO } else { -LN2_LO };
            k = 1 - sign - sign;
        } else {
            k = (INV_LN2 * x + if sign == 0 { 0.5 } else { -0.5 }) as i32;
            hi = x - k as f64 * LN2_HI;
            lo = k as f64 * LN2_LO;
        }
        x = hi - lo;
    } else if hx < 0x3e30_0000 {
        return 1.0 + x;
    }
    let t = x * x;
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - (lo - (x * c) / (2.0 - c) - hi);
    if k >= -1021 {
        if k == 1024 {
            return y * 2.0 * from_words(0x7fe0_0000, 0);
        }
        return y * from_words((0x3ff + k).wrapping_shl(20), 0);
    }
    y * from_words((0x3ff + k + 1000).wrapping_shl(20), 0) * TWO_M1000
}

const TWO54: f64 = 1.80143985094819840000e16;
const LG1: f64 = 6.666666666666735130e-1;
const LG2: f64 = 3.999999999940941908e-1;
const LG3: f64 = 2.857142874366239149e-1;
const LG4: f64 = 2.222219843214978396e-1;
const LG5: f64 = 1.818357216161805012e-1;
const LG6: f64 = 1.531383769920937332e-1;
const LG7: f64 = 1.479819860511658591e-1;

/// Natural logarithm (fdlibm `e_log.c`).
pub fn log(mut x: f64) -> f64 {
    let mut hx = high(x);
    let lx = low(x);
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        if (((hx & 0x7fff_ffff) as u32) | lx) == 0 {
            return f64::NEG_INFINITY;
        }
        if hx < 0 {
            return f64::NAN;
        }
        k -= 54;
        x *= TWO54;
        hx = high(x);
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x95f64) & 0x10_0000;
    x = with_high(x, hx | (i ^ 0x3ff0_0000));
    k += i >> 20;
    let f = x - 1.0;
    if (0x000f_ffff & (2 + hx)) < 3 {
        if f == 0.0 {
            return if k == 0 {
                0.0
            } else {
                k as f64 * LN2_HI + k as f64 * LN2_LO
            };
        }
        let r = f * f * (0.5 - 0.333_333_333_333_333_33 * f);
        return if k == 0 {
            f - r
        } else {
            k as f64 * LN2_HI - (r - k as f64 * LN2_LO - f)
        };
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    let r = t2 + t1;
    if ((hx - 0x6147a) | (0x6b851 - hx)) > 0 {
        let hfsq = 0.5 * f * f;
        return if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            k as f64 * LN2_HI - (hfsq - (s * (hfsq + r) + k as f64 * LN2_LO) - f)
        };
    }
    if k == 0 {
        f - s * (f - r)
    } else {
        k as f64 * LN2_HI - (s * (f - r) - k as f64 * LN2_LO - f)
    }
}

/// x^y for x > 0, as e^(y ln x); 0 for x = 0 and y > 0, infinity for x = 0 and y <= 0.
pub fn pow(x: f64, y: f64) -> f64 {
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else { f64::INFINITY };
    }
    exp(y * log(x))
}

const ATAN_HI: [f64; 4] = [
    4.63647609000806093515e-1,
    7.85398163397448278999e-1,
    9.82793723247329054082e-1,
    1.57079632679489655800e0,
];
const ATAN_LO: [f64; 4] = [
    2.26987774529616870924e-17,
    3.06161699786838301793e-17,
    1.39033110312309984516e-17,
    6.12323399573676603587e-17,
];
const AT: [f64; 11] = [
    3.33333333333329318027e-1,
    -1.99999999998764832476e-1,
    1.42857142725034663711e-1,
    -1.11111104054623557880e-1,
    9.09088713343650656196e-2,
    -7.69187620504482999495e-2,
    6.66107313738753120669e-2,
    -5.83357013379057348645e-2,
    4.97687799461593236017e-2,
    -3.65315727442169155270e-2,
    1.62858201153657823623e-2,
];

/// Arctangent (fdlibm `s_atan.c`).
fn atan(mut x: f64) -> f64 {
    let hx = high(x);
    let ix = hx & 0x7fff_ffff;
    let id: i32;
    if ix >= 0x4410_0000 {
        if ix > 0x7ff0_0000 || (ix == 0x7ff0_0000 && low(x) != 0) {
            return x + x;
        }
        return if hx > 0 {
            ATAN_HI[3] + ATAN_LO[3]
        } else {
            -ATAN_HI[3] - ATAN_LO[3]
        };
    }
    if ix < 0x3fdc_0000 {
        if ix < 0x3e40_0000 {
            return x;
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3ff3_0000 {
            if ix < 0x3fe6_0000 {
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else if ix < 0x4003_8000 {
            id = 2;
            x = (x - 1.5) / (1.0 + 1.5 * x);
        } else {
            id = 3;
            x = -1.0 / x;
        }
    }
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        return x - x * (s1 + s2);
    }
    let id = id as usize;
    let r = ATAN_HI[id] - (x * (s1 + s2) - ATAN_LO[id] - x);
    if hx < 0 { -r } else { r }
}

const PI: f64 = 3.1415926535897931160e0;
const PI_LO: f64 = 1.2246467991473531772e-16;
const PI_O_2: f64 = 1.5707963267948965580e0;
const PI_O_4: f64 = 7.8539816339744827900e-1;

/// The angle of (x, y) from the positive x axis, in radians, -pi to pi (fdlibm `e_atan2.c`).
pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let hx = high(x);
    let ix = hx & 0x7fff_ffff;
    let lx = low(x);
    let hy = high(y);
    let iy = hy & 0x7fff_ffff;
    let ly = low(y);
    if (hx.wrapping_sub(0x3ff0_0000) | lx as i32) == 0 {
        return atan(y);
    }
    let mut m = ((((hy as u32) >> 31) & 1) | (((hx as u32) >> 30) & 2)) as usize;
    if (iy | ly as i32) == 0 {
        if m < 2 {
            return y;
        }
        return if m == 2 { PI } else { -PI };
    }
    if (ix | lx as i32) == 0 {
        return if hy < 0 { -PI_O_2 } else { PI_O_2 };
    }
    if ix == 0x7ff0_0000 {
        if iy == 0x7ff0_0000 {
            return [PI_O_4, -PI_O_4, 3.0 * PI_O_4, -3.0 * PI_O_4][m];
        }
        return [0.0, -0.0, PI, -PI][m];
    }
    if iy == 0x7ff0_0000 {
        return if hy < 0 { -PI_O_2 } else { PI_O_2 };
    }
    let k = (iy - ix) >> 20;
    let z = if k > 60 {
        m &= 1;
        PI_O_2 + 0.5 * PI_LO
    } else if hx < 0 && k < -60 {
        0.0
    } else {
        atan((y / x).abs())
    };
    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => z - PI_LO - PI,
    }
}
