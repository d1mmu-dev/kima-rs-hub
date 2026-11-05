mod common;
use common::*;
use kima_vibe::exact_math::{atan2, exp, log, pow};
use kima_vibe::prng::{SeededRandom, gaussian};

#[test]
fn exp_log_atan2_within_one_ulp_of_std() {
    let mut r = Xorshift(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        let x = (r.unit() * 2.0 - 1.0) * 700.0;
        assert!(
            ulps(exp(x), x.exp()) <= 1,
            "exp({x}) = {} vs {}",
            exp(x),
            x.exp()
        );
        let p = (r.unit() * 2.0 - 1.0) * 60.0;
        let y = p.exp();
        assert!(
            ulps(log(y), y.ln()) <= 1,
            "log({y}) = {} vs {}",
            log(y),
            y.ln()
        );
        let (a, b) = ((r.unit() * 2.0 - 1.0) * 1e3, (r.unit() * 2.0 - 1.0) * 1e3);
        assert!(
            ulps(atan2(a, b), a.atan2(b)) <= 1,
            "atan2({a},{b}) = {} vs {}",
            atan2(a, b),
            a.atan2(b)
        );
    }
}

#[test]
fn pow_within_eight_ulp() {
    // The contract is upstream's: the layout raises distances in [0, 60) to the curve exponent b.
    const CURVE_B: f64 = 0.8950608779639974;
    let mut r = Xorshift(42);
    for _ in 0..100_000 {
        let x = r.unit() * 60.0;
        assert!(
            ulps(pow(x, CURVE_B), x.powf(CURVE_B)) <= 8,
            "pow({x}) = {} vs {}",
            pow(x, CURVE_B),
            x.powf(CURVE_B)
        );
    }
}

#[test]
fn edges_nan_inf_zero_subnormal() {
    assert!(exp(f64::NAN).is_nan());
    assert_eq!(exp(f64::INFINITY), f64::INFINITY);
    assert_eq!(exp(f64::NEG_INFINITY), 0.0);
    assert_eq!(exp(710.0), f64::INFINITY);
    assert_eq!(exp(-746.0), 0.0);
    assert_eq!(exp(0.0), 1.0);
    assert!(
        exp(-745.0) > 0.0,
        "smallest subnormal region still positive"
    );
    assert_eq!(log(0.0), f64::NEG_INFINITY);
    assert_eq!(log(-0.0), f64::NEG_INFINITY);
    assert!(log(-1.0).is_nan());
    assert!(log(f64::NAN).is_nan());
    assert_eq!(log(f64::INFINITY), f64::INFINITY);
    assert_eq!(log(1.0), 0.0);
    assert!(ulps(log(5e-324), 5e-324_f64.ln()) <= 1, "subnormal input");
    assert_eq!(atan2(0.0, 1.0), 0.0);
    assert_eq!(atan2(0.0, -1.0), std::f64::consts::PI);
    assert_eq!(atan2(-0.0, -1.0), -std::f64::consts::PI);
    assert!(atan2(f64::NAN, 1.0).is_nan());
    assert_eq!(atan2(1.0, 0.0), std::f64::consts::FRAC_PI_2);
    assert_eq!(
        atan2(f64::INFINITY, f64::INFINITY),
        std::f64::consts::FRAC_PI_4
    );
    assert_eq!(pow(0.0, 2.0), 0.0);
    assert_eq!(pow(0.0, -1.0), f64::INFINITY);
}

fn check(section: &str, f: impl Fn(&[f64]) -> f64) {
    let golden = &fixture("ts-golden-exactmath.json")[section];
    let cases = golden.as_array().expect("array of cases");
    assert!(cases.len() >= 2000, "{section}: {} cases", cases.len());
    for c in cases {
        let inputs: Vec<f64> = c["in"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| from_bits_hex(h.as_str().unwrap()))
            .collect();
        let want = from_bits_hex(c["out"].as_str().unwrap());
        let got = f(&inputs);
        assert!(
            same_bits(got, want),
            "{section}{inputs:?}: got {:016x} want {:016x}",
            got.to_bits(),
            want.to_bits()
        );
    }
}

#[test]
fn bits_match_ts_golden() {
    check("exp", |a| exp(a[0]));
    check("log", |a| log(a[0]));
    check("atan2", |a| atan2(a[0], a[1]));
    check("pow", |a| pow(a[0], a[1]));
}

#[test]
fn prng_and_gaussian_match_ts_golden() {
    let g = fixture("ts-golden-exactmath.json");
    let mut rng = SeededRandom::new(42);
    for (i, h) in g["prngSeed42"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            rng.next_f64().to_bits(),
            from_bits_hex(h.as_str().unwrap()).to_bits(),
            "output {i}"
        );
    }
    assert_eq!(g["prngSeed42"].as_array().unwrap().len(), 1000);
    let mut rng = SeededRandom::new(1234);
    for (i, h) in g["gaussianSeed1234"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            gaussian(&mut rng).to_bits(),
            from_bits_hex(h.as_str().unwrap()).to_bits(),
            "gaussian {i}"
        );
    }
}

#[test]
fn no_unsafe() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = std::fs::read_to_string(dir.join("lib.rs")).unwrap();
    assert!(lib.contains("#![forbid(unsafe_code)]"));
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            let bad = text
                .lines()
                .filter(|l| l.contains("unsafe") && !l.contains("forbid(unsafe_code)"))
                .count();
            assert_eq!(bad, 0, "{} mentions unsafe", path.display());
        }
    }
}
