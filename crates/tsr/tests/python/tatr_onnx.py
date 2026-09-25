"""Export pinned TATR weights and check parity, structure, and isolated inference latency.

Run with an isolated Python environment; see the accompanying benchmark report.
Weights and generated artifacts live in the ignored models/tatr-v1.1-all directory.
"""
import argparse
import copy
import gzip
import hashlib
import importlib.util
import json
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
import torch
from PIL import Image, ImageDraw
from transformers import AutoImageProcessor, TableTransformerForObjectDetection

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "scripts"))
from export_tatr import ExportModel, export


def prepare(processor, images, edge=800):
    """Apply upstream longest-edge resizing before normalization and batch padding."""
    # Transformers 4.57 rejects this checkpoint's longest_edge-only size configuration.
    # Resize explicitly to retain the checkpoint's intended aspect ratio and 800px edge.
    resized = [image.resize((round(image.width * edge / max(image.size)),
                             round(image.height * edge / max(image.size))),
                            Image.Resampling.BILINEAR) for image in images]
    return processor(images=resized, do_resize=False, return_tensors="pt")


def benchmark(session, inputs, repetitions):
    """Measure synchronous calls, including host/device copies, after three warmups."""
    for _ in range(3):
        session.run(None, inputs)
    samples = []
    for _ in range(repetitions):
        start = time.perf_counter()
        session.run(None, inputs)
        samples.append((time.perf_counter() - start) * 1000)
    return {"median_ms": float(np.median(samples)), "p95_ms": float(np.percentile(samples, 95)),
            "samples_ms": samples}


def main():
    """Export dynamic ONNX, verify actual providers, and compare one real fixture."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=["cpu", "cuda"], default="cuda")
    parser.add_argument("--repetitions", type=int, default=20)
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    root = Path(__file__).resolve().parents[4]
    directory = root / "models/tatr-v1.1-all"
    fixture = root / "crates/tsr/tests/fixtures"
    torch.set_num_threads(4)
    model = ExportModel(TableTransformerForObjectDetection.from_pretrained(
        directory, use_pretrained_backbone=False, attn_implementation="eager")).eval()
    processor = AutoImageProcessor.from_pretrained(directory, use_fast=False)
    image = Image.open(fixture / "table.png").convert("RGB")
    inputs = prepare(processor, [image])
    path = directory / "inference.onnx"
    if not path.exists():
        export(directory, path)
    onnx.checker.check_model(str(path))
    # Use the existing artifact contract; JSON is a YAML subset, so no configuration rewrite is needed.
    (directory / "inference.yml").write_bytes((directory / "preprocessor_config.json").read_bytes())
    manifest = {"repository": "microsoft/table-transformer-structure-recognition-v1.1-all",
                "revision": "7587a7ef111d9dcbf8ac695f1376ab7014340a0c", "license": "MIT",
                "files": {name: hashlib.sha256((directory / name).read_bytes()).hexdigest()
                          for name in ["inference.onnx", "inference.yml"]}}
    (directory / "model-manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
    options = ort.SessionOptions()
    options.intra_op_num_threads = 4
    options.enable_mem_pattern = False
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    options.enable_profiling = args.provider == "cuda"
    options.profile_file_prefix = str(directory / "ort-profile")
    provider = "CUDAExecutionProvider" if args.provider == "cuda" else "CPUExecutionProvider"
    # Disable TF32 so parity checks compare FP32 arithmetic on both backends.
    providers = [(provider, {"use_tf32": "0"})] if args.provider == "cuda" else [provider]
    session = ort.InferenceSession(str(path), sess_options=options, providers=providers)
    assert session.get_providers()[0] == provider, session.get_providers()
    report = {"model_revision": "7587a7ef111d9dcbf8ac695f1376ab7014340a0c",
              "postprocess_revision": "16d124f616109746b7785f03085100f1f6247575",
              "onnx_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
              "torch": torch.__version__, "onnxruntime": ort.__version__, "provider": provider,
              "parity": [], "batch_size": 1, "cpu_threads": 4, "optimization": "all"}
    # Exercise a different spatial shape and a padded mixed-size batch, not just the export shape.
    cases = [inputs, prepare(processor, [image], edge=640),
             prepare(processor, [image, image.rotate(90, expand=True)])]
    with torch.inference_mode():
        for case in cases:
            actual = session.run(None, {k: v.numpy() for k, v in case.items()})
            expected = model(case["pixel_values"], case["pixel_mask"])
            errors = []
            for result, reference in zip(actual, expected):
                np.testing.assert_allclose(result, reference.numpy(), rtol=1e-3, atol=1e-3)
                errors.append(float(np.max(np.abs(result - reference.numpy()))))
            report["parity"].append({"shape": list(case["pixel_values"].shape), "max_absolute_errors": errors})
    feed = {k: v.numpy() for k, v in inputs.items()}
    logits, boxes = session.run(None, feed)
    if options.enable_profiling:
        profile = json.loads(Path(session.end_profiling()).read_text())
        placements = {}
        for event in profile:
            placement = event.get("args", {}).get("provider")
            if placement:
                placements[placement] = placements.get(placement, 0) + 1
        assert placements.get("CUDAExecutionProvider", 0) > 0, placements
        report["profile_node_events"] = placements
    report["tatr"] = benchmark(session, feed, args.repetitions)
    probabilities = torch.tensor(logits).softmax(-1)[0]
    scores, labels = probabilities.max(-1)
    names = model.model.config.id2label
    objects = []
    for score, label, box in zip(scores, labels, boxes[0]):
        label = int(label)
        if label not in names or names[label] == "no object" or float(score) < 0.5:
            continue
        cx, cy, w, h = box.tolist()
        objects.append({"label": label, "score": float(score), "page_num": 0,
                        "bbox": [(cx-w/2)*image.width, (cy-h/2)*image.height,
                                 (cx+w/2)*image.width, (cy+h/2)*image.height]})
    tables = [obj for obj in objects if names[obj["label"]] == "table"]
    assert tables, "No table detected in the real fixture"
    spec = importlib.util.spec_from_file_location("tatr_postprocess", directory / "postprocess.py")
    postprocess = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(postprocess)
    start = time.perf_counter()
    structures, cells, _ = postprocess.objects_to_cells(
        copy.deepcopy(max(tables, key=lambda obj: obj["score"])), copy.deepcopy(objects), [],
        names, {name: 0.5 for name in names.values()})
    report["postprocess_ms"] = (time.perf_counter()-start)*1000
    # Compare grid topology against the existing SLANet regression oracle, not human ground truth.
    oracle = json.loads((fixture / "table-reference.json").read_text())
    html = ET.fromstring("".join(oracle["structure_tokens"]))
    expected_cells = []
    occupied = set()
    for row, tr in enumerate(html.iter("tr")):
        col = 0
        for td in tr:
            while (row, col) in occupied:
                col += 1
            rows = list(range(row, row+int(td.get("rowspan", 1))))
            cols = list(range(col, col+int(td.get("colspan", 1))))
            occupied.update((r, c) for r in rows for c in cols)
            expected_cells.append((tuple(rows), tuple(cols)))
            col += len(cols)
    actual_cells = [(tuple(c["row_nums"]), tuple(c["column_nums"])) for c in cells]
    report["structure"] = {"rows": len(structures["rows"]), "columns": len(structures["columns"]),
                           "cells": len(cells), "oracle_rows": len(list(html.iter("tr"))),
                           "oracle_cells": len(expected_cells),
                           "exact_oracle_topology": sorted(actual_cells) == sorted(expected_cells),
                           "shared_cell_topologies": len(set(actual_cells) & set(expected_cells))}
    overlay = image.copy()
    draw = ImageDraw.Draw(overlay)
    for cell in cells:
        draw.rectangle(cell["bbox"], outline="red", width=1)
    overlay.save(directory / "cells.png")
    (directory / "cells.json").write_text(json.dumps(cells, indent=2)+"\n")
    # Use the checked-in reference tensor to avoid introducing different SLANet preprocessing.
    options.enable_profiling = False
    baseline = ort.InferenceSession(str(root / "models/slanet-plus/inference.onnx"),
                                    sess_options=options, providers=providers)
    assert baseline.get_providers()[0] == provider
    tensor = np.frombuffer(gzip.decompress((fixture / "table-input.f32.gz").read_bytes()),
                           dtype="<f4").reshape(1, 3, 488, 488)
    report["slanet_plus"] = benchmark(baseline, {baseline.get_inputs()[0].name: tensor}, args.repetitions)
    report["isolated_speedup"] = report["slanet_plus"]["median_ms"] / report["tatr"]["median_ms"]
    (directory / f"report-{args.provider}.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
