"""Generate weight-free MusiCNN head ONNX fixtures; requires the Python onnx package.

Mirrors the Softmax-output fixtures already checked in
(`synthetic_musicnn_head.onnx`, `synthetic_musicnn_bad.onnx`,
`synthetic_musicnn_nan.onnx`) but with the `model/Identity:0` output name
used by the real Deam/Emomusic regression heads, so the head-session
regression suite can exercise both pinned output-name paths.
"""
from pathlib import Path

import onnx
from onnx import TensorProto, helper

HERE = Path(__file__).resolve().parent


def make_head_model(output_name: str, width: int, ones_value: float) -> onnx.ModelProto:
    x = helper.make_tensor_value_info("model/Placeholder:0", TensorProto.FLOAT, ["batch", 200])
    y = helper.make_tensor_value_info(output_name, TensorProto.FLOAT, ["batch", width])
    nodes = [
        helper.make_node("ReduceMean", ["model/Placeholder:0"], ["mean"], axes=[1], keepdims=0),
        helper.make_node("Unsqueeze", ["mean", "axes"], ["column"]),
        helper.make_node("Mul", ["column", "ones"], [output_name]),
    ]
    initializers = [
        helper.make_tensor("axes", TensorProto.INT64, [1], [1]),
        helper.make_tensor("ones", TensorProto.FLOAT, [1, width], [ones_value] * width),
    ]
    model = helper.make_model(
        helper.make_graph(nodes, "synthetic", [x], [y], initializers),
        opset_imports=[helper.make_opsetid("", 13)],
        ir_version=7,
    )
    onnx.checker.check_model(model)
    return model


# Happy-path echo model for the Identity-output heads (Deam/Emomusic): `y = x_mean * 1`.
onnx.save(
    make_head_model("model/Identity:0", 2, 1.0),
    HERE / "synthetic_musicnn_head_identity.onnx",
)

# Wrong output width on the Identity path: valid input/output names, but a [-1, 3]
# output instead of the pinned [-1, 2], mirroring synthetic_musicnn_bad.onnx.
onnx.save(
    make_head_model("model/Identity:0", 3, 1.0),
    HERE / "synthetic_musicnn_head_identity_bad.onnx",
)

# Non-finite output values on the Identity path, mirroring synthetic_musicnn_nan.onnx:
# multiplying by a NaN constant makes every output value NaN regardless of input.
onnx.save(
    make_head_model("model/Identity:0", 2, float("nan")),
    HERE / "synthetic_musicnn_head_identity_nan.onnx",
)
