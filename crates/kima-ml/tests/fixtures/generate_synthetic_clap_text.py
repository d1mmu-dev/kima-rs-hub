"""Generate weight-free ONNX fixtures for CLAP text inference; requires the
Python onnx package. Mirrors generate_synthetic_clap.py's pattern for the
audio session, adapted to an int64 `input_ids` input (no attention_mask,
matching the pinned real text graph) and a `text_embeds` output name.
"""
from pathlib import Path
import onnx
from onnx import TensorProto, helper

HERE = Path(__file__).resolve().parent
x = helper.make_tensor_value_info("input_ids", TensorProto.INT64, ["batch", "sequence"])
y = helper.make_tensor_value_info("text_embeds", TensorProto.FLOAT, ["batch", 512])
nodes = [
    helper.make_node("Cast", ["input_ids"], ["f"], to=TensorProto.FLOAT),
    helper.make_node("ReduceMean", ["f"], ["m"], axes=[1], keepdims=0),
    helper.make_node("Unsqueeze", ["m", "axes"], ["u"]),
    helper.make_node("Tile", ["u", "reps"], ["text_embeds"]),
]
initializers = [
    helper.make_tensor("axes", TensorProto.INT64, [1], [1]),
    helper.make_tensor("reps", TensorProto.INT64, [2], [1, 512]),
]
model = helper.make_model(
    helper.make_graph(nodes, "synthetic_clap_text", [x], [y], initializers),
    opset_imports=[helper.make_opsetid("", 13)],
    ir_version=7,
)
onnx.checker.check_model(model)
onnx.save(model, HERE / "synthetic_clap_text.onnx")

model.graph.initializer[1].int64_data[1] = 511
model.graph.output[0].type.tensor_type.shape.dim[1].dim_value = 511
onnx.save(model, HERE / "synthetic_clap_text_bad_shape.onnx")

model.graph.initializer[1].int64_data[1] = 512
model.graph.output[0].type.tensor_type.shape.dim[1].dim_value = 512
model.graph.node[-1].output[0] = "temp"
model.graph.node.append(helper.make_node("Div", ["temp", "zero"], ["text_embeds"]))
model.graph.initializer.append(helper.make_tensor("zero", TensorProto.FLOAT, [], [0.0]))
onnx.checker.check_model(model)
onnx.save(model, HERE / "synthetic_clap_text_nonfinite.onnx")
