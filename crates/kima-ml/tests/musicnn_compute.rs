use kima_ml::musicnn::{MusicnnComputation, MusicnnCompute};
use kima_ml::onnx::{MusiCnnHead, OnnxModel};
use kima_ml::verify_artifact;
use std::collections::BTreeMap;

fn load_compute() -> MusicnnCompute {
    let dir = std::env::var_os("KIMA_MUSICNN_MODEL_DIR")
        .expect("explicit prerequisite: set KIMA_MUSICNN_MODEL_DIR to private approved models");
    let dir = std::path::Path::new(&dir);
    let core = verify_artifact(dir, &OnnxModel::MusiCnnCore.descriptor()).unwrap();
    let heads: BTreeMap<_, _> = MusiCnnHead::all()
        .iter()
        .map(|&head| (head, verify_artifact(dir, &head.descriptor()).unwrap()))
        .collect();
    MusicnnCompute::from_verified_artifacts(&core, &heads).unwrap()
}

fn floats(path: &std::path::Path) -> Vec<f32> {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("explicit reference prerequisite {}: {e}", path.display()));
    assert_eq!(bytes.len() % 4, 0);
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

fn compare<const N: usize>(case: &str, name: &str, actual: &[[f32; N]], expected: Vec<f32>) {
    assert_eq!(actual.len() * N, expected.len(), "{case}/{name} shape");
    assert!(!actual.is_empty());
    let mut min_cosine = 1.0_f64;
    let mut max_absolute = 0.0_f64;
    for (row, reference) in actual.iter().zip(expected.chunks_exact(N)) {
        assert!(row.iter().chain(reference).all(|x| x.is_finite()));
        let dot: f64 = row
            .iter()
            .zip(reference)
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum();
        let norm = |v: &[f32]| v.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>().sqrt();
        let denominator = norm(row) * norm(reference);
        assert!(denominator > 0.0);
        let cosine = dot / denominator;
        assert!(cosine >= 0.999, "{case}/{name}: cosine {cosine}");
        min_cosine = min_cosine.min(cosine);
        for (&a, &b) in row.iter().zip(reference) {
            max_absolute = max_absolute.max((f64::from(a) - f64::from(b)).abs());
        }
    }
    println!(
        "PARITY case={case} model={name} vectors={} min_cosine={min_cosine:.15} max_abs={max_absolute:.9}",
        actual.len()
    );
}

/// References must be generated privately by pinned upstream musicnn_onnx.py
/// using real Essentia preprocessing, never by this Rust frontend. See spec.
#[test]
#[ignore = "requires private pinned models and independently generated private upstream references"]
fn private_pinned_upstream_real_model_parity() {
    let reference = std::env::var_os("KIMA_MUSICNN_REFERENCE_DIR")
        .expect("explicit prerequisite: KIMA_MUSICNN_REFERENCE_DIR from pinned upstream generator");
    let mut compute = load_compute();
    for (case, patches) in [("period17", 3), ("gate30", 19), ("chirp90", 59)] {
        let dir = std::path::Path::new(&reference).join(case);
        let input = floats(&dir.join("input.f32"));
        let MusicnnComputation::Patches(result) = compute.compute(&input).unwrap() else {
            panic!("no patches: {case}")
        };
        assert_eq!(result.embeddings().len(), patches);
        compare(
            case,
            "musicnn",
            result.embeddings(),
            floats(&dir.join("musicnn.f32")),
        );
        for head in MusiCnnHead::all() {
            let name = head.basename().split("-msd-").next().unwrap();
            compare(
                case,
                name,
                &result.heads()[head],
                floats(&dir.join(format!("{name}.f32"))),
            );
        }
    }
}

#[test]
#[ignore = "requires private 11 pinned MusiCNN models and trusted ORT_DYLIB_PATH"]
fn private_eleven_models_compute_nonconstant_tail() {
    let mut compute = load_compute();
    assert!(matches!(
        compute.compute(&[]).unwrap(),
        MusicnnComputation::NoFullPatch
    ));
    let samples: Vec<f32> = (0..373 * 256)
        .map(|i| {
            0.15 * (std::f32::consts::TAU * 220.0 * i as f32 / 16000.0).sin()
                + 0.03 * ((i % 17) as f32 - 8.0) / 8.0
        })
        .collect();
    let MusicnnComputation::Patches(result) = compute.compute(&samples).unwrap() else {
        panic!("meaningful nonzero signal must yield full patches")
    };
    assert_eq!(result.embeddings().len(), 3);
    assert!(result.embeddings().iter().flatten().all(|x| x.is_finite()));
    assert!(result.embeddings().iter().flatten().any(|&x| x != 0.0));
    assert_ne!(result.embeddings()[0], result.embeddings()[1]);
    assert_eq!(result.heads().len(), 10);
    for head in MusiCnnHead::all() {
        let rows = &result.heads()[head];
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().flatten().all(|x| x.is_finite()));
        if !matches!(head, MusiCnnHead::Deam | MusiCnnHead::Emomusic) {
            assert!(rows.iter().all(|r| (r[0] + r[1] - 1.0).abs() < 0.00001));
        }
    }
}
