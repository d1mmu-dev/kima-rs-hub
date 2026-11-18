use kima_vibe::zero_shot::{PROMPTS, Readings, softmax_share};
use sha2::{Digest, Sha256};
fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/zero-shot-reference.json")).unwrap()
}
#[test]
fn softmax_share_matches_python_values() {
    for row in reference()["softmax"].as_array().unwrap() {
        let v: Vec<f64> = row
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect();
        assert!((softmax_share(v[0], v[1], 15.) - v[2]).abs() < 1e-15);
    }
}
#[test]
fn instrumentalness_rounds_to_3_and_clamps() {
    let r = Readings::from_similarities([0.2, 0.3, 1., -1., 0., 0., 0., 0.]).unwrap();
    assert_eq!(r.instrumentalness, 0.818);
    assert_eq!(r.acousticness, 1.);
    let r = Readings::from_similarities([1., -1., -1., 1., 0., 0., 0., 0.]).unwrap();
    assert_eq!(r.instrumentalness, 0.);
    assert_eq!(r.acousticness, 0.);
    assert!(Readings::from_similarities([f64::NAN; 8]).is_err());
}
#[test]
fn valence_platt_constants() {
    for row in reference()["platt"].as_array().unwrap() {
        let m = row[0].as_f64().unwrap();
        assert_eq!(
            Readings::from_similarities([0., 0., 0., 0., m, 0., 0., 0.])
                .unwrap()
                .valence_raw,
            row[1].as_f64().unwrap()
        );
    }
}
#[test]
fn arousal_platt_constants() {
    for row in reference()["platt"].as_array().unwrap() {
        let m = row[0].as_f64().unwrap();
        assert_eq!(
            Readings::from_similarities([0., 0., 0., 0., 0., 0., m, 0.])
                .unwrap()
                .arousal_raw,
            row[2].as_f64().unwrap()
        );
    }
}
#[test]
fn prompt_groups_are_the_24_upstream_strings() {
    let fixture = reference();
    let mut text = String::new();
    assert_eq!(PROMPTS.len() * 3, 24);
    for (name, prompts) in PROMPTS {
        assert_eq!(serde_json::json!(prompts), fixture["groups"][name]);
        text.push_str(name);
        text.push('\n');
        for p in prompts {
            text.push_str(p);
            text.push('\n');
        }
    }
    assert_eq!(
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        fixture["prompt_text_sha256"].as_str().unwrap()
    );
}
#[test]
fn nonconstant_512d_vectors_match_independent_python_reference() {
    let vector = |seed: i64| -> [f32; 512] {
        std::array::from_fn(|i| (((i as i64 * 31 + seed * 17) % 101 - 50) as f64 / 101.) as f32)
    };
    let prompts: Vec<_> = (100..124).map(vector).collect();
    for case in reference()["synthetic"].as_array().unwrap() {
        let r = Readings::compute(&vector(case["seed"].as_i64().unwrap()), &prompts).unwrap();
        assert_eq!(
            serde_json::json!([
                r.instrumentalness,
                r.acousticness,
                r.valence_raw,
                r.arousal_raw
            ]),
            case["expected"]
        );
    }
}
#[test]
fn stored_vectors_use_all_512_dimensions_and_cosine_normalization() {
    let mut audio = [0.; 512];
    audio[511] = 2.;
    let mut prompts = vec![[0.; 512]; 24];
    for p in &mut prompts {
        p[511] = 3.;
    }
    let r = Readings::compute(&audio, &prompts).unwrap();
    assert_eq!(r, Readings::from_similarities([1.; 8]).unwrap());
    assert!(Readings::compute(&[0.; 512], &prompts).is_err());
    prompts[23][0] = f32::INFINITY;
    assert!(Readings::compute(&audio, &prompts).is_err());
}
