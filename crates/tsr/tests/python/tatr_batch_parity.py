"""Distinguish checkpoint padding sensitivity from ONNX export errors on a tall stress crop."""
import json
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image
import torch
from transformers import AutoImageProcessor, TableTransformerForObjectDetection

from tatr_onnx import ExportModel, prepare


def main():
    """Check PyTorch/ONNX parity before comparing unpadded and mixed-batch predictions."""
    root = Path(__file__).resolve().parents[4]
    directory = root / "models/tatr-v1.1-all"
    torch.set_num_threads(4)
    model = ExportModel(TableTransformerForObjectDetection.from_pretrained(
        directory, use_pretrained_backbone=False, attn_implementation="eager")).eval()
    processor = AutoImageProcessor.from_pretrained(directory, use_fast=False)
    image = Image.open(root / "crates/tsr/tests/fixtures/table.png").convert("RGB")
    tall = image.resize((400, 800), Image.Resampling.BILINEAR)
    options = ort.SessionOptions()
    options.intra_op_num_threads = 4
    session = ort.InferenceSession(str(directory / "inference.onnx"), sess_options=options,
                                   providers=["CPUExecutionProvider"])
    report, outputs = {}, {}
    with torch.inference_mode():
        for name, images, index in [("singleton", [tall], 0), ("homogeneous", [tall, tall], 0),
                                    ("mixed", [image, tall], 1)]:
            inputs = prepare(processor, images)
            expected = model(inputs["pixel_values"], inputs["pixel_mask"])
            actual = session.run(None, {key: value.numpy() for key, value in inputs.items()})
            errors = []
            for a, b in zip(actual, expected):
                np.testing.assert_allclose(a, b.numpy(), rtol=1e-3, atol=1e-3)
                errors.append(float(np.abs(a-b.numpy()).max()))
            outputs[name] = [value[index].numpy() for value in expected]
            report[name] = {"shape": list(inputs["pixel_values"].shape), "onnx_max_errors": errors}
    logits, boxes = outputs["singleton"]
    probabilities = torch.tensor(logits).softmax(-1).numpy()
    selected = (probabilities.max(-1) > .9) & (probabilities.argmax(-1) != 6)
    for name in ["homogeneous", "mixed"]:
        report[name]["pytorch_confident_box_drift"] = float(np.abs(boxes[selected]-outputs[name][1][selected]).max())
    assert report["homogeneous"]["pytorch_confident_box_drift"] < 1e-3
    assert report["mixed"]["pytorch_confident_box_drift"] > .005
    (directory / "batch-pytorch-parity.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
