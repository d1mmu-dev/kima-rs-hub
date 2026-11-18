"""Generate weight-free ONNX fixtures for the music CLAP text session; requires the Python onnx
package (regenerated with onnx 1.23.2). Same signature as the pinned `music_text.onnx` (kima-hub
c7527577 export): int64 `input_ids` and `attention_mask` [batch, tokens] -> `embedding`
[batch, 512]. The body is mean(input_ids * attention_mask) tiled to 512, so a test can see that
the mask reaches the graph.
"""
from pathlib import Path
import onnx
from onnx import TensorProto, helper

HERE = Path(__file__).resolve().parent
ids = helper.make_tensor_value_info("input_ids", TensorProto.INT64, ["batch", "tokens"])
mask = helper.make_tensor_value_info("attention_mask", TensorProto.INT64, ["batch", "tokens"])
y = helper.make_tensor_value_info("embedding", TensorProto.FLOAT, ["batch", 512])
nodes = [
    helper.make_node("Mul", ["input_ids", "attention_mask"], ["kept"]),
    helper.make_node("Cast", ["kept"], ["f"], to=TensorProto.FLOAT),
    helper.make_node("ReduceMean", ["f"], ["m"], axes=[1], keepdims=0),
    helper.make_node("Unsqueeze", ["m", "axes"], ["u"]),
    helper.make_node("Tile", ["u", "reps"], ["embedding"]),
]
initializers = [
    helper.make_tensor("axes", TensorProto.INT64, [1], [1]),
    helper.make_tensor("reps", TensorProto.INT64, [2], [1, 512]),
]
model = helper.make_model(
    helper.make_graph(nodes, "synthetic_clap_text", [ids, mask], [y], initializers),
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
model.graph.node.append(helper.make_node("Div", ["temp", "zero"], ["embedding"]))
model.graph.initializer.append(helper.make_tensor("zero", TensorProto.FLOAT, [], [0.0]))
onnx.checker.check_model(model)
onnx.save(model, HERE / "synthetic_clap_text_nonfinite.onnx")
