//! `parse_mood_heads` ported from upstream `moods/__tests__/heads.test.ts`, on synthetic tiny files: the real
//! `moodHeads.json` is a private asset (owner decision Q3) and never enters Git.
use kima_vibe::mood_heads::{MOODS, parse_mood_heads};
use serde_json::{Value, json};

const DIMS: usize = 512;

fn weights(seed: usize) -> Vec<f64> {
    (0..DIMS)
        .map(|i| ((i * 7 + seed * 13) % 17) as f64 / 17.0 - 0.5)
        .collect()
}

fn file() -> Value {
    let moods: serde_json::Map<String, Value> = MOODS
        .iter()
        .enumerate()
        .map(|(n, m)| {
            (
                m.to_string(),
                json!({"w": weights(n), "b": n as f64 * 0.1 - 1.0}),
            )
        })
        .collect();
    json!({"version": "synthetic-v1", "model": "synthetic-model", "dims": DIMS, "order": MOODS, "moods": moods})
}

fn refused(edit: impl FnOnce(&mut Value)) -> String {
    let mut v = file();
    edit(&mut v);
    parse_mood_heads(&serde_json::to_vec(&v).unwrap())
        .expect_err("must be refused")
        .to_string()
}

#[test]
fn loads_all_22_in_order_with_512_weights_and_bias() {
    let heads = parse_mood_heads(&serde_json::to_vec(&file()).unwrap()).unwrap();
    assert_eq!(heads.version, "synthetic-v1");
    assert_eq!(heads.model, "synthetic-model");
    assert_eq!(heads.dims, 512);
    assert_eq!(heads.heads.len(), 22);
    assert_eq!(MOODS[0], "happy");
    assert_eq!(MOODS[21], "sleep");
    for (n, head) in heads.heads.iter().enumerate() {
        assert!(head.weights == weights(n), "weights of {}", MOODS[n]);
        assert_eq!(head.bias, n as f64 * 0.1 - 1.0);
    }
}

#[test]
fn rejects_missing_version() {
    assert!(refused(|v| v["version"] = json!("")).contains("no version"));
    assert!(
        refused(|v| {
            v.as_object_mut().unwrap().remove("model");
        })
        .contains("no model")
    );
    assert!(refused(|v| v["dims"] = json!("512")).contains("no dims"));
}

#[test]
fn rejects_wrong_order() {
    let order: Vec<&str> = MOODS.to_vec();
    assert!(refused(|v| v["order"] = json!(&order[1..])).contains("order must be"));
    assert!(
        refused(|v| v["order"] = json!(order.iter().rev().collect::<Vec<_>>()))
            .contains("order must be")
    );
    assert!(
        refused(|v| {
            v["moods"].as_object_mut().unwrap().remove("sleep");
        })
        .contains("sleep is missing")
    );
}

#[test]
fn rejects_extra_mood() {
    assert!(
        refused(|v| v["moods"]["spooky"] = v["moods"]["dark"].clone())
            .contains("outside the order: spooky")
    );
}

#[test]
fn rejects_non_finite_weight() {
    assert!(refused(|v| v["moods"]["calm"]["w"][3] = json!("0.1")).contains("calm has a weight"));
    assert!(refused(|v| v["moods"]["calm"]["w"][3] = Value::Null).contains("calm has a weight"));
    assert!(
        refused(|v| {
            v["moods"]["happy"].as_object_mut().unwrap().remove("b");
        })
        .contains("happy has no finite bias")
    );
    // JSON cannot hold NaN or infinity; an out-of-range literal is refused as a whole.
    let text = serde_json::to_string(&file())
        .unwrap()
        .replacen("-0.5", "1e999", 1);
    assert!(parse_mood_heads(text.as_bytes()).is_err());
    assert!(parse_mood_heads(b"not json").is_err());
}

#[test]
fn rejects_wrong_dims() {
    assert!(
        refused(|v| {
            v["moods"]["calm"]["w"].as_array_mut().unwrap().pop();
        })
        .contains("calm needs 512 weights")
    );
    // A consistent file of another width is refused too: the vectors are 512-d.
    let mut v = file();
    v["dims"] = json!(4);
    for m in MOODS {
        v["moods"][m]["w"] = json!([0.1, 0.2, 0.3, 0.4]);
    }
    let err = parse_mood_heads(&serde_json::to_vec(&v).unwrap())
        .unwrap_err()
        .to_string();
    assert!(err.contains("dims must be 512"), "{err}");
}
