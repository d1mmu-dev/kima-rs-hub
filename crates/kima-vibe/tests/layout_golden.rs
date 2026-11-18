//! The layout and placement against upstream's own TypeScript output, bit for bit (`ts-golden-layout.json`,
//! `ts-golden-place.json`): `layoutLibrary` and `placeTracks` on a 50-track and a 300-track library.
mod common;
use common::*;
use kima_vibe::colour::ColourFrame;
use kima_vibe::map_compute::{layout_library, place_tracks};
use serde_json::Value;

fn bits(v: &Value) -> Vec<f64> {
    match v {
        Value::Array(a) => a.iter().flat_map(bits).collect(),
        other => vec![from_bits_hex(other.as_str().unwrap())],
    }
}

fn assert_bits(name: &str, actual: &[f64], golden: &Value) {
    let expected = bits(golden);
    assert_eq!(actual.len(), expected.len(), "{name} length");
    for (i, (a, e)) in actual.iter().zip(&expected).enumerate() {
        assert!(same_bits(*a, *e), "{name}[{i}]: {a:e} vs {e:e}");
    }
}

fn assert_frame(name: &str, frame: &ColourFrame, golden: &Value) {
    assert_bits(&format!("{name} centre"), &frame.centre, &golden["centre"]);
    assert_bits(&format!("{name} scale"), &[frame.scale], &golden["scale"]);
    assert_bits(&format!("{name} turn"), &frame.turn, &golden["turn"]);
    assert_bits(
        &format!("{name} hueMin"),
        &[frame.hue_min],
        &golden["hueMin"],
    );
    assert_bits(
        &format!("{name} hueMax"),
        &[frame.hue_max],
        &golden["hueMax"],
    );
}

fn frame_of(golden: &Value) -> ColourFrame {
    let turn = bits(&golden["turn"]);
    let centre = bits(&golden["centre"]);
    ColourFrame {
        centre: [centre[0], centre[1], centre[2]],
        scale: bits(&golden["scale"])[0],
        turn: [turn[0], turn[1], turn[2], turn[3], turn[4], turn[5]],
        hue_min: bits(&golden["hueMin"])[0],
        hue_max: bits(&golden["hueMax"])[0],
    }
}

#[test]
fn layout_matches_ts_golden_bitwise() {
    let golden = fixture("ts-golden-layout.json");
    for name in ["fixture50", "synthetic300"] {
        let lib = load(name);
        let g = &golden["libraries"][name];
        let laid = layout_library(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, None);
        assert_bits(
            &format!("{name} maxScore"),
            &[laid.max_score],
            &g["maxScore"],
        );
        assert_bits(
            &format!("{name} positions"),
            &laid.positions,
            &g["positions"],
        );
        assert_bits(&format!("{name} colour"), &laid.colour, &g["colour"]);
        assert_frame(name, &laid.frame, &g["frame"]);
    }
}

#[test]
fn place_matches_ts_golden_bitwise() {
    let g = fixture("ts-golden-place.json");
    let lib = load("synthetic300");
    let laid_out = g["laidOutCount"].as_u64().unwrap() as usize;
    let new = g["newCount"].as_u64().unwrap() as usize;
    assert_eq!(laid_out + new, lib.tracks.len());
    let positions = bits(&g["laidOutPositions"]);
    let frame = frame_of(&g["frame"]);
    let max_score = bits(&g["maxScore"])[0];
    // `positioned` is the laid-out tracks' positions here, as in the upstream script.
    let placed = place_tracks(
        &lib.tracks[..laid_out],
        &positions,
        &positions,
        &lib.tracks[laid_out..],
        &lib.model,
        lib.gw,
        &frame,
        max_score,
    );
    assert_bits("placed positions", &placed.positions, &g["positions"]);
    assert_bits("placed colour", &placed.colour, &g["colour"]);
}

#[test]
fn layout_is_deterministic_and_seed_sensitive() {
    let lib = load("synthetic300");
    let a = layout_library(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, None);
    let b = layout_library(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, None);
    assert_eq!(a.positions, b.positions);
    let c = layout_library(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, Some(43));
    assert_ne!(a.positions, c.positions);
}

#[test]
fn cancelled_layout_returns_nothing_and_an_uncancelled_one_is_unchanged() {
    use kima_vibe::cancel::Cancel;
    use kima_vibe::map_compute::layout_library_cancellable;
    let lib = load("synthetic300");
    let stop = Cancel::new();
    stop.cancel();
    assert!(
        layout_library_cancellable(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, None, &stop)
            .is_none()
    );
    let live = Cancel::new();
    let a = layout_library_cancellable(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, None, &live)
        .unwrap();
    let b = layout_library(&lib.tracks, &lib.feel_z, &lib.model, lib.gw, None);
    assert_eq!(a.positions, b.positions);
}
