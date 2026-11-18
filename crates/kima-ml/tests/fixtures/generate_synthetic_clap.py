"""Generate weight-free ONNX fixtures for the music CLAP audio session; requires the Python onnx
package (regenerated with onnx 1.23.2). Same signature as the pinned `music_audio.onnx`
(kima-hub c7527577 export): `mel` float32 [batch, 1, 1001, 64] -> `embedding` [batch, 512]; the
body is a mean tiled to 512, not a model.
"""
from pathlib import Path
import onnx
from onnx import TensorProto, helper

HERE = Path(__file__).resolve().parent
x = helper.make_tensor_value_info("mel", TensorProto.FLOAT, ["batch", 1, 1001, 64])
y = helper.make_tensor_value_info("embedding", TensorProto.FLOAT, ["batch", 512])
nodes = [
    helper.make_node("ReduceMean", ["mel"], ["m"], axes=[1, 2, 3], keepdims=0),
    helper.make_node("Unsqueeze", ["m", "axes"], ["u"]),
    helper.make_node("Tile", ["u", "reps"], ["embedding"]),
]
initializers = [
    helper.make_tensor("axes", TensorProto.INT64, [1], [1]),
    helper.make_tensor("reps", TensorProto.INT64, [2], [1, 512]),
]
model = helper.make_model(helper.make_graph(nodes, "synthetic_clap", [x], [y], initializers),
                          opset_imports=[helper.make_opsetid("", 13)], ir_version=7)
onnx.checker.check_model(model)
onnx.save(model, HERE / "synthetic_clap.onnx")
model.graph.initializer[1].int64_data[1] = 511
model.graph.output[0].type.tensor_type.shape.dim[1].dim_value = 511
onnx.save(model, HERE / "synthetic_clap_bad_shape.onnx")
model.graph.initializer[1].int64_data[1] = 512
model.graph.output[0].type.tensor_type.shape.dim[1].dim_value = 512
model.graph.node[-1].output[0] = "temp"
model.graph.node.append(helper.make_node("Div", ["temp", "zero"], ["embedding"]))
model.graph.initializer.append(helper.make_tensor("zero", TensorProto.FLOAT, [], [0.0]))
onnx.checker.check_model(model)
onnx.save(model, HERE / "synthetic_clap_nonfinite.onnx")
