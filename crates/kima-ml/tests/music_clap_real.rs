//! Native music CLAP parity on the real private models (ignored: noncommercial weights are never
//! in Git or CI). Loads the pinned `music_audio.onnx`, `music_text.onnx` and `tokenizer.json`
//! through the production adapters (hash pins plus the graph signatures read from the verified
//! graphs), computes [`MusicClapMel`] natively and compares against a private reference produced
//! by upstream kima-hub c7527577 `music_clap_onnx.MusicClapAudio`/`MusicClapText` (slow
//! `RobertaTokenizer`) on the same verified files.
//!
//! Environment (every gap panics, so a skip can never read as a pass):
//! - `KIMA_CLAP_MODEL_DIR`: absolute directory with the three pinned files.
//! - `KIMA_MUSIC_CLAP_PYTHON_REFERENCE`: absolute path of the private reference JSON.
//! - `ORT_DYLIB_PATH`: trusted ONNX Runtime.
//!
//! Synthetic signals only; this is model-level parity, not the 20-track library acceptance gate.
use kima_ml::clap::{MusicClapMel, normalize_embedding};
use kima_ml::onnx::{ClapAudioSession, ClapTextSession, OnnxModel};
use kima_ml::text_tokenizer::{self, ClapTextTokenizer};
use serde_json::Value;

fn env_path(key: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(
        std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set")),
    );
    assert!(path.is_absolute(), "{key} must be absolute");
    path
}

fn reference() -> Value {
    let path = env_path("KIMA_MUSIC_CLAP_PYTHON_REFERENCE");
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        value["audio_sha256"],
        hex(&OnnxModel::ClapAudio.expected_sha256()),
        "reference made with another audio model"
    );
    assert_eq!(
        value["text_sha256"],
        hex(&OnnxModel::ClapText.expected_sha256()),
        "reference made with another text model"
    );
    value
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn vector(value: &Value) -> Vec<f32> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap() as f32)
        .collect()
}

/// Cosine and maximum absolute difference; both vectors are unit-norm.
fn compare(actual: &[f32; 512], expected: &[f32]) -> (f64, f64) {
    assert_eq!(expected.len(), 512);
    let (mut dot, mut a, mut b, mut max) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (&x, &y) in actual.iter().zip(expected) {
        let (x, y) = (f64::from(x), f64::from(y));
        dot += x * y;
        a += x * x;
        b += y * y;
        max = max.max((x - y).abs());
    }
    (dot / (a.sqrt() * b.sqrt()), max)
}

fn signal(name: &str) -> Vec<f32> {
    let square = |n: usize, period: usize| if n % period < period / 2 { 1.0 } else { -1.0 };
    match name {
        "short" => (0..737)
            .map(|n| ((n * 37 % 211) as f32 - 105.0) / 256.0)
            .collect(),
        "broadband" => (0..480_000)
            .map(|n| ((n * 97 % 1021) as f32 - 510.0) / 1024.0)
            .collect(),
        "impulse" => {
            let mut s = vec![0.0; 480_000];
            s[0] = 1.0;
            s[479_999] = -0.5;
            s
        }
        "tones" => (0..480_000)
            .map(|n| (square(n, 256) + square(n, 16) + square(n, 4)) / 4.0)
            .collect(),
        "loud" => (0..480_000_u32)
            .map(|n| (1.7 * (f64::from(n) * 0.0123).sin() + 0.000013 * f64::from(n % 7)) as f32)
            .collect(),
        other => panic!("unknown signal {other}"),
    }
}

#[test]
#[ignore = "private noncommercial music CLAP models and a private Python reference required"]
fn native_music_audio_matches_upstream_python_single_and_batched() {
    let dir = env_path("KIMA_CLAP_MODEL_DIR");
    let reference = reference();
    let artifact = kima_ml::verify_artifact(&dir, &OnnxModel::ClapAudio.descriptor()).unwrap();
    let model = ClapAudioSession::from_verified_bytes(&artifact, 1, Some(2))
        .expect("pinned music audio graph loads and has the music signature");
    let mel = MusicClapMel::new();
    let names: Vec<String> = reference["audio"]["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 5);
    let mut batch = Vec::new();
    let mut worst = (1.0_f64, 0.0_f64);
    for name in &names {
        let frames = mel.compute(&signal(name)).unwrap();
        let flat: Vec<f32> = frames.iter().flatten().copied().collect();
        let raw = model.infer(&flat, 1).unwrap();
        let unit = normalize_embedding(&raw[0]).unwrap();
        let (cos, max) = compare(&unit, &vector(&reference["audio"]["single"][name]));
        println!("audio {name}: cosine={cos:.9} max_abs={max:.3e}");
        worst = (worst.0.min(cos), worst.1.max(max));
        batch.extend(flat);
    }
    let raw = model.infer(&batch, names.len()).unwrap();
    for (i, name) in names.iter().enumerate() {
        let unit = normalize_embedding(&raw[i]).unwrap();
        let (cos, max) = compare(&unit, &vector(&reference["audio"]["batched"][i]));
        println!("audio batched {name}: cosine={cos:.9} max_abs={max:.3e}");
        worst = (worst.0.min(cos), worst.1.max(max));
    }
    println!("audio worst: cosine={:.9} max_abs={:.3e}", worst.0, worst.1);
    assert!(worst.0 >= 0.99999 && worst.1 < 1e-4, "{worst:?}");
}

#[test]
#[ignore = "private noncommercial music CLAP models and a private Python reference required"]
fn native_music_text_tokens_and_vectors_match_upstream_python() {
    let dir = env_path("KIMA_CLAP_MODEL_DIR");
    let reference = reference();
    let tokenizer = ClapTextTokenizer::from_verified_bytes(
        &kima_ml::verify_artifact(&dir, &text_tokenizer::descriptor()).unwrap(),
    )
    .unwrap();
    let artifact = kima_ml::verify_artifact(&dir, &OnnxModel::ClapText.descriptor()).unwrap();
    let model = ClapTextSession::from_verified_bytes(&artifact, 1, Some(2))
        .expect("pinned music text graph loads and has the music signature");
    let cases = reference["text"].as_array().unwrap();
    assert!(cases.len() >= 10);
    let mut worst = (1.0_f64, 0.0_f64);
    for case in cases {
        let prompt = case["prompt"].as_str().unwrap();
        let (ids, mask) = tokenizer.encode_padded(prompt).unwrap();
        let expect = |key: &str| -> Vec<i64> {
            case[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect()
        };
        assert_eq!(ids, expect("input_ids"), "input_ids for {prompt:?}");
        assert_eq!(
            mask,
            expect("attention_mask"),
            "attention_mask for {prompt:?}"
        );
        let raw = model.infer(&ids, &mask, 1).unwrap();
        let unit = normalize_embedding(&raw[0]).unwrap();
        let (cos, max) = compare(&unit, &vector(&case["embedding"]));
        println!("text {prompt:?}: cosine={cos:.9} max_abs={max:.3e}");
        worst = (worst.0.min(cos), worst.1.max(max));
    }
    println!("text worst: cosine={:.9} max_abs={:.3e}", worst.0, worst.1);
    assert!(worst.0 >= 0.99999 && worst.1 < 1e-4, "{worst:?}");
}

#[test]
#[ignore = "private noncommercial music CLAP models required"]
fn the_general_m4_graphs_do_not_load_under_the_music_names() {
    // A directory holding only the general-model files (or anything else under the music
    // basenames) fails verification before any session exists: the pins are the music export.
    let dir = tempfile::tempdir().unwrap();
    let model_dir = env_path("KIMA_CLAP_MODEL_DIR");
    std::fs::copy(
        model_dir.join("tokenizer.json"),
        dir.path().join(OnnxModel::CLAP_AUDIO_BASENAME),
    )
    .unwrap();
    assert!(kima_ml::verify_artifact(dir.path(), &OnnxModel::ClapAudio.descriptor()).is_err());
    assert!(kima_ml::verify_artifact(dir.path(), &OnnxModel::ClapText.descriptor()).is_err());
}
