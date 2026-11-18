//! `parse_feel_probe` on synthetic files: the real `feelProbe.json` (CC BY-NC) is a private asset (Q3).
use kima_vibe::feel_probe::parse_feel_probe;
use serde_json::{Value, json};

fn axis(seed: usize) -> Value {
    json!({"alpha": 80.5 + seed as f64, "bias": 0.25, "weights": (0..512).map(|i| ((i * 3 + seed) % 11) as f64 / 11.0 - 0.4).collect::<Vec<f64>>()})
}

fn file() -> Value {
    json!({"model": "synthetic-model", "modelSha256": "ab".repeat(32), "source": "synthetic", "valence": axis(1), "arousal": axis(2)})
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn refused(edit: impl FnOnce(&mut Value)) -> String {
    let mut v = file();
    edit(&mut v);
    parse_feel_probe(&bytes(&v))
        .expect_err("must be refused")
        .to_string()
}

#[test]
fn parses_model_and_512_weights_per_axis() {
    let b = bytes(&file());
    let probe = parse_feel_probe(&b).unwrap();
    assert_eq!(probe.model, "synthetic-model");
    assert_eq!(probe.valence.weights.len(), 512);
    assert_eq!(probe.arousal.weights.len(), 512);
    assert_eq!(probe.valence.bias, 0.25);
    assert_eq!(probe.valence.alpha, 81.5);
    assert_eq!(probe.arousal.alpha, 82.5);
}

#[test]
fn probe_id_is_the_first_16_hex_digits_of_the_sha256_of_the_bytes() {
    use sha2::{Digest, Sha256};
    let b = bytes(&file());
    let want: String = Sha256::digest(&b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect::<String>()[..16]
        .to_owned();
    assert_eq!(parse_feel_probe(&b).unwrap().probe_id, want);
    let mut other = file();
    other["valence"]["bias"] = json!(0.26);
    assert_ne!(
        parse_feel_probe(&bytes(&other)).unwrap().probe_id,
        want,
        "any change of the file changes the id"
    );
}

#[test]
fn rejects_malformed_files() {
    assert!(refused(|v| v["model"] = json!("")).contains("no model"));
    assert!(
        refused(|v| v["valence"]["weights"]
            .as_array_mut()
            .unwrap()
            .pop()
            .map(drop)
            .unwrap_or(()))
        .contains("valence needs 512 weights")
    );
    assert!(refused(|v| v["arousal"]["weights"][5] = json!("x")).contains("arousal has a weight"));
    assert!(refused(|v| v["arousal"]["bias"] = Value::Null).contains("arousal has no finite bias"));
    assert!(
        refused(|v| v["valence"]["alpha"] = json!("1")).contains("valence has no finite alpha")
    );
    assert!(
        refused(|v| {
            v.as_object_mut().unwrap().remove("arousal");
        })
        .contains("no arousal")
    );
    assert!(parse_feel_probe(b"{").is_err());
}
