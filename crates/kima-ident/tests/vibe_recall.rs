//! Task 14 real recall gate. Ordinary tests below are synthetic harness-logic tests,
//! NOT evidence of real-model recall. See docs/testing/vibe-recall.md.
mod common;
#[path = "vibe_recall/real.rs"]
mod real;
use std::collections::HashSet;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real model + authorized isolated real-embedding copy + 50 predeclared queries; see docs/testing/vibe-recall.md"]
async fn real_music_checkpoint_recall_at_20() {
    real::measure()
        .await
        .expect("real recall prerequisites or measurement failed (never skipped)");
}

fn queries(bytes: &[u8]) -> Result<Vec<String>, String> {
    let values: Vec<String> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let normalized: Vec<_> = values
        .iter()
        .map(|s| s.trim_matches(kima_vibe::ecmascript_whitespace).to_owned())
        .collect();
    let unique: HashSet<_> = normalized.iter().collect();
    if values.len() != 50
        || unique.len() != 50
        || normalized.iter().any(|s| s.encode_utf16().count() < 2)
        || values.iter().any(|s| s.encode_utf16().count() > 500)
    {
        return Err(
            "need exactly 50 unique, nonempty real text queries (2..500 UTF-16 units)".into(),
        );
    }
    Ok(normalized)
}

#[test]
fn synthetic_harness_rejects_wrong_query_count_and_duplicates() {
    assert!(queries(b"[]").is_err());
    assert!(queries(&serde_json::to_vec(&vec!["rain"; 50]).unwrap()).is_err());
    let values: Vec<_> = (0..50).map(|i| format!("query {i}")).collect();
    assert_eq!(
        queries(&serde_json::to_vec(&values).unwrap()).unwrap(),
        values
    );
}

#[test]
fn synthetic_queries_normalize_before_uniqueness_and_encoding() {
    let mut values: Vec<_> = (0..50)
        .map(|i| format!("\u{feff} query {i} \u{feff}"))
        .collect();
    let normalized = queries(&serde_json::to_vec(&values).unwrap()).unwrap();
    assert_eq!(normalized[0], "query 0");
    values[1] = "query 0".into();
    assert!(queries(&serde_json::to_vec(&values).unwrap()).is_err());
    values[1] = "\u{feff}x\u{feff}".into();
    assert!(queries(&serde_json::to_vec(&values).unwrap()).is_err());
    values[1] = "\u{0085}x\u{0085}".into();
    let normalized = queries(&serde_json::to_vec(&values).unwrap()).unwrap();
    assert_eq!(normalized[1], "\u{0085}x\u{0085}");
}

fn recall_at_20(exact: &[uuid::Uuid], ann: &[uuid::Uuid]) -> Result<f64, String> {
    let expected: HashSet<_> = exact.iter().collect();
    let actual: HashSet<_> = ann.iter().collect();
    if exact.len() != 20 || expected.len() != 20 || actual.len() != ann.len() || ann.len() > 20 {
        return Err("invalid top-20 lists".into());
    }
    Ok(expected.intersection(&actual).count() as f64 / 20.0)
}
fn uses_hnsw(plan: &serde_json::Value) -> bool {
    match plan {
        serde_json::Value::Object(fields) => {
            (fields.get("Node Type").and_then(|v| v.as_str()) == Some("Index Scan")
                && fields.get("Index Name").and_then(|v| v.as_str())
                    == Some("track_embeddings_cosine_idx")
                && fields
                    .get("Actual Loops")
                    .and_then(|v| v.as_f64())
                    .is_some_and(|n| n > 0.0)
                && fields
                    .get("Actual Rows")
                    .and_then(|v| v.as_f64())
                    .is_some_and(|n| n > 0.0))
                || fields.values().any(uses_hnsw)
        }
        serde_json::Value::Array(values) => values.iter().any(uses_hnsw),
        _ => false,
    }
}

#[test]
fn synthetic_harness_scores_missing_neighbors_without_fallback() {
    let exact: Vec<_> = (0..20).map(uuid::Uuid::from_u128).collect();
    assert_eq!(recall_at_20(&exact, &exact[..17]).unwrap(), 0.85);
    assert!(recall_at_20(&exact[..19], &exact).is_err());
    assert!(recall_at_20(&exact, &vec![exact[0]; 20]).is_err());
}

#[test]
fn synthetic_harness_rejects_seq_scan_and_wrong_index() {
    use serde_json::json;
    assert!(!uses_hnsw(&json!([{"Plan":{"Node Type":"Seq Scan"}}])));
    assert!(!uses_hnsw(
        &json!([{"Plan":{"Node Type":"Index Scan","Index Name":"tracks_pkey"}}])
    ));
    assert!(uses_hnsw(
        &json!([{"Plan":{"Plans":[{"Node Type":"Index Scan","Index Name":"track_embeddings_cosine_idx","Actual Loops":1,"Actual Rows":20}]}}])
    ));
}
