"""Check real Rust TSR captures against the pinned ONNX models and Paddle-style OpenCV tensors."""
from pathlib import Path
import json

import cv2
import numpy as np
import onnxruntime as ort
import yaml


def main():
    """Compare structure tokens and detector boxes on one unchanged real PDF crop."""
    root = Path(__file__).resolve().parents[4]
    captures = root / "packages/wasm-web/test-results/tsr-comparison"
    case = "4-p6/p6-b-m5-s0"
    bgr = cv2.imread(str(captures / "baseline" / (case + ".png")))
    assert bgr is not None
    height, width = bgr.shape[:2]
    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    report = {"opencv": cv2.__version__, "numpy": np.__version__, "onnxruntime": ort.__version__, "checks": []}
    for model, variant, edge in [("slanet-plus", "baseline", 488), ("slanext-wired", "upgraded-wired", 512), ("slanext-wireless", "upgraded-wireless", 512)]:
        directory = root / "models" / model
        config = yaml.safe_load((directory / "inference.yml").read_text())
        dictionary = ["sos"] + [t for t in config["PostProcess"]["character_dict"] if t != "<td>"] + ["<td></td>", "eos"]
        ratio = edge / max(height, width)
        image = cv2.resize(bgr, (round(width * ratio), round(height * ratio)), interpolation=cv2.INTER_LINEAR).astype(np.float32)
        image = (image / 255.0 - np.array([.485, .456, .406], dtype=np.float32)) / np.array([.229, .224, .225], dtype=np.float32)
        padded = np.zeros((edge, edge, 3), dtype=np.float32)
        padded[:image.shape[0], :image.shape[1]] = image
        session = ort.InferenceSession(str(directory / "inference.onnx"), sess_options=options, providers=["CPUExecutionProvider"])
        _, probabilities = session.run(None, {"x": padded.transpose(2, 0, 1)[None].copy()})
        tokens = ["<html>", "<body>", "<table>"]
        for index in probabilities[0].argmax(axis=1):
            if dictionary[index] == "eos":
                break
            if dictionary[index] != "sos":
                tokens.append(dictionary[index])
        tokens += ["</table>", "</body>", "</html>"]
        expected = json.loads((captures / variant / (case + ".json")).read_text())["prediction"]["structure_tokens"]
        assert tokens == expected, f"{model}: Rust and reference structure tokens differ"
        report["checks"].append({"model": model, "tokens": len(tokens), "exact_token_match": True})
        del session
    rgb = cv2.cvtColor(bgr, cv2.COLOR_BGR2RGB)
    tensor = cv2.resize(rgb, (640, 640), interpolation=cv2.INTER_CUBIC).astype(np.float32).transpose(2, 0, 1)[None] / 255.0
    for suffix in ["wired", "wireless"]:
        model = "rtdetr-table-cell-" + suffix
        session = ort.InferenceSession(str(root / "models" / model / "inference.onnx"), sess_options=options, providers=["CPUExecutionProvider"])
        boxes, _ = session.run(None, {"image": tensor.copy(), "im_shape": np.array([[640, 640]], dtype=np.float32), "scale_factor": np.array([[640 / height, 640 / width]], dtype=np.float32)})
        boxes = boxes[(boxes[:, 0] == 0) & (boxes[:, 1] >= .3), 2:6]
        boxes[:, [0, 2]] = boxes[:, [0, 2]].clip(0, width)
        boxes[:, [1, 3]] = boxes[:, [1, 3]].clip(0, height)
        expected = np.array(json.loads((captures / ("cells-" + suffix) / (case + ".json")).read_text())["prediction"]["detected_cell_bboxes"])
        assert boxes.shape == expected.shape, f"{model}: different accepted cell count"
        # RT-DETR emits unordered top-k detections; tiny score ties can change their order across runtimes.
        distances = np.max(np.abs(boxes[:, None, :] - expected[None, :, :]), axis=2)
        used_actual, used_expected, matched_deltas = set(), set(), []
        for flat_index in np.argsort(distances, axis=None):
            actual_index, expected_index = np.unravel_index(flat_index, distances.shape)
            if actual_index not in used_actual and expected_index not in used_expected:
                used_actual.add(actual_index)
                used_expected.add(expected_index)
                matched_deltas.append(float(distances[actual_index, expected_index]))
        assert len(matched_deltas) == len(boxes)
        delta = max(matched_deltas)
        assert delta < .05, f"{model}: detector boxes differ by {delta} crop pixels"
        report["checks"].append({"model": model, "cells": len(boxes), "max_coordinate_delta_pixels": delta})
        del session
    (captures / "parity.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
