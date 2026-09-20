"""Download the pinned ONNX export and adapt encoder shape metadata for Optimum I/O Binding."""
import hashlib
import json
from pathlib import Path
import shutil

from huggingface_hub import snapshot_download
import numpy as np
import onnx
import onnxruntime as ort
import torch

REVISION = "b2668efe5112082846fde4d446b9bfaab3989533"
MODEL_ROOT = Path(__file__).resolve().parent / "models"
FILES = (
    "config.json", "generation_config.json", "special_tokens_map.json",
    "tokenizer.json", "tokenizer_config.json", "encoder_model.onnx",
    "decoder_model.onnx", "decoder_with_past_model.onnx",
)


def main():
    """Keep original graphs intact and verify the CUDA output before specializing a deployment copy."""
    if not torch.cuda.is_available():
        raise RuntimeError("Model preparation requires an NVIDIA CUDA GPU")
    source = MODEL_ROOT / "FormulaNet" / "onnx"
    target = MODEL_ROOT / "FormulaNet-optimum"
    snapshot_download(
        "alephpi/FormulaNet", revision=REVISION, local_dir=source.parent,
        allow_patterns=[f"onnx/{name}" for name in FILES],
    )
    source_hash = hashlib.sha256((source / "encoder_model.onnx").read_bytes()).hexdigest()
    if source_hash != "fbd69cf63cf833db1e2ef40013d859b560671c1253278441a01bde4516b624ae":
        raise ValueError("The encoder does not match the pinned Texo export")
    ort.preload_dlls()
    session = ort.InferenceSession(str(source / "encoder_model.onnx"), providers=["CUDAExecutionProvider"])
    if session.get_providers()[0] != "CUDAExecutionProvider":
        raise RuntimeError("CUDAExecutionProvider failed to initialize")
    actual = session.run(["last_hidden_state"], {"pixel_values": np.zeros((1, 3, 384, 384), dtype=np.float32)})[0]
    if actual.shape != (1, 144, 2048):
        raise ValueError(f"Unexpected encoder output shape: {actual.shape}")
    target.mkdir(parents=True, exist_ok=True)
    for name in FILES:
        shutil.copy2(source / name, target / name)
    # Only C/H/W and sequence-length metadata change; batch stays dynamic and weights are untouched.
    model = onnx.load(source / "encoder_model.onnx")
    for dimension, size in zip(model.graph.input[0].type.tensor_type.shape.dim[1:], [3, 384, 384]):
        dimension.ClearField("dim_param")
        dimension.dim_value = size
    dimension = model.graph.output[0].type.tensor_type.shape.dim[1]
    dimension.ClearField("dim_param")
    dimension.dim_value = actual.shape[1]
    onnx.checker.check_model(model)
    onnx.save(model, target / "encoder_model.onnx")
    report = {
        "revision": REVISION,
        "change": "encoder input C/H/W=3/384/384 and output sequence length=144; batch remains dynamic; weights unchanged",
        "actual_output_shape": list(actual.shape),
        "source_sha256": source_hash,
        "deployment_sha256": hashlib.sha256((target / "encoder_model.onnx").read_bytes()).hexdigest(),
    }
    (target / "adaptation.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)


if __name__ == "__main__":
    main()
