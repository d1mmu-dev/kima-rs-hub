mod common;
use kima_ml::onnx::{BoundedSessionPool, OnnxModelConsumer};
use kima_ml::{ArtifactId, ModelConsumer};

// Tiny synthetic ONNX protobuf, authored here; no model assets/dependencies.
fn varint(mut n: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    while n > 127 {
        bytes.push((n as u8 & 127) | 128);
        n >>= 7;
    }
    bytes.push(n as u8);
    bytes
}
fn integer(field: u64, n: u64) -> Vec<u8> {
    [varint(field << 3), varint(n)].concat()
}
fn string(field: u64, bytes: &[u8]) -> Vec<u8> {
    [
        varint(field << 3 | 2),
        varint(bytes.len() as u64),
        bytes.to_vec(),
    ]
    .concat()
}
fn external_graph(location: &str) -> Vec<u8> {
    let tensor = [
        integer(1, 1),
        integer(2, 1),
        string(8, b"w"),
        string(
            13,
            &[string(1, b"location"), string(2, location.as_bytes())].concat(),
        ),
        string(13, &[string(1, b"length"), string(2, b"4")].concat()),
        integer(14, 1),
    ]
    .concat();
    let node = [string(1, b"w"), string(2, b"y"), string(4, b"Identity")].concat();
    let typ = string(
        1,
        &[integer(1, 1), string(2, &string(1, &integer(1, 1)))].concat(),
    );
    let graph = [
        string(1, &node),
        string(2, b"external_probe"),
        string(5, &tensor),
        string(12, &[string(1, b"y"), string(2, &typ)].concat()),
    ]
    .concat();
    [integer(1, 7), string(7, &graph), string(8, &integer(2, 13))].concat()
}

#[test]
fn verified_graph_cannot_load_unverified_external_initializer() {
    let dir = tempfile::tempdir().unwrap();
    let weights = dir.path().join("weights.bin");
    std::fs::write(&weights, 42.0f32.to_le_bytes()).unwrap();
    for location in [weights.to_str().unwrap(), "weights.bin"] {
        let artifact = common::verified_fixture(
            ArtifactId::from_static("synthetic-external-initializer"),
            external_graph(location).into_boxed_slice(),
        );
        // ORT's memory loader must reject even a present absolute weights file.
        // Only graph bytes were hashed; never fall back to path-based loading.
        assert!(OnnxModelConsumer::new().load(artifact.clone()).is_err());
        assert!(BoundedSessionPool::from_verified_bytes(&artifact, 1, Some(1)).is_err());
    }
}
