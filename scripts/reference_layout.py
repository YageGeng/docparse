# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "numpy==2.3.5",
#   "onnxruntime==1.29.0",
#   "opencv-contrib-python==4.10.0.84",
#   "paddleocr==3.6.0",
# ]
# ///
"""Generate a deterministic PP-DocLayoutV3 preprocessing and output oracle."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path

import cv2
import numpy as np

# ONNX Runtime requires the process-wide opt-out before its module initializes.
os.environ.setdefault("ORT_DISABLE_TELEMETRY", "1")
import onnxruntime as ort
from paddlex.inference.models.object_detection.processors import (
    Normalize,
    ReadImage,
    Resize,
    ToBatch,
    ToCHWImage,
)

PADDLEX_SOURCE_COMMIT = "ffb64904d23708863ff5b8da312a5cbd52a7f462"
MODEL_REVISION = "46bbdf188bb0a772c08aed74882ce7e51a8f1ea6"
MODEL_INPUT_SIZE = (800, 800)
DEFAULT_THRESHOLD = 0.5
LABELS = (
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
)


class OracleError(RuntimeError):
    """Signals an unsupported model schema or invalid oracle input."""


def configure_deterministic_opencv() -> None:
    """Disables platform-specific optimized resize paths for portable parity."""
    cv2.setNumThreads(1)
    cv2.setUseOptimized(False)
    cv2.ipp.setUseIPP(False)


def sha256_file(path: Path) -> str:
    """Streams one input file through SHA-256."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def array_sha256(array: np.ndarray, dtype: str) -> str:
    """Hashes a C-contiguous array using an explicit little-endian dtype."""
    canonical = np.ascontiguousarray(array).astype(dtype, copy=False)
    return hashlib.sha256(canonical.tobytes(order="C")).hexdigest()


def array_contract(array: np.ndarray, name: str, dtype: str) -> dict:
    """Builds deterministic shape, range, and digest metadata for one tensor."""
    return {
        "name": name,
        "dtype": str(array.dtype),
        "shape": list(array.shape),
        "min": float(array.min()) if array.size else None,
        "max": float(array.max()) if array.size else None,
        "sha256": array_sha256(array, dtype),
    }


def official_preprocess(input_path: Path) -> tuple[list[dict], list[np.ndarray]]:
    """Runs the fixed PaddleX object-detection preprocessing operators."""
    configure_deterministic_opencv()
    data = ReadImage(format="RGB")([str(input_path)])
    data = Resize(
        target_size=MODEL_INPUT_SIZE,
        keep_ratio=False,
        interp="BICUBIC",
    )(data)
    data = Normalize(
        scale=1.0 / 255.0,
        mean=[0.0, 0.0, 0.0],
        std=[1.0, 1.0, 1.0],
    )(data)
    data = ToCHWImage()(data)
    batch = ToBatch(("img_size", "img", "scale_factors"))(data)
    return data, list(batch)


def run_onnx(
    model_path: Path, batch: list[np.ndarray]
) -> tuple[ort.InferenceSession, list[np.ndarray]]:
    """Runs the fixed three-input ONNX graph with the CPU provider."""
    # Oracle runs are local verification and must not emit runtime telemetry.
    ort.disable_telemetry_events()
    session = ort.InferenceSession(
        str(model_path), providers=["CPUExecutionProvider"]
    )
    input_names = [value.name for value in session.get_inputs()]
    output_names = [value.name for value in session.get_outputs()]
    if input_names != ["im_shape", "image", "scale_factor"]:
        raise OracleError(f"unsupported input names: {input_names}")
    if output_names != ["fetch_name_0", "fetch_name_1", "fetch_name_2"]:
        raise OracleError(f"unsupported output names: {output_names}")
    image_size, image, scale_factor = batch
    outputs = session.run(
        None,
        {
            "im_shape": image_size,
            "image": image,
            "scale_factor": scale_factor,
        },
    )
    return session, outputs


def lossless_detections(
    bbox_rows: np.ndarray,
    original_width: int,
    original_height: int,
    threshold: float,
) -> list[dict]:
    """Applies only thresholding, ties-to-even rounding, clipping, and stable sorting."""
    detections = []
    for source_index, row in enumerate(bbox_rows):
        if row.shape != (7,):
            raise OracleError(f"unsupported bbox row shape: {row.shape}")
        class_id = int(row[0])
        score = float(row[1])
        if class_id < 0 or score <= threshold:
            continue
        if class_id >= len(LABELS):
            raise OracleError(f"class id out of range: {class_id}")

        rounded = np.rint(row[2:6]).astype(np.int64)
        xmin = int(np.clip(rounded[0], 0, original_width))
        ymin = int(np.clip(rounded[1], 0, original_height))
        xmax = int(np.clip(rounded[2], 0, original_width))
        ymax = int(np.clip(rounded[3], 0, original_height))
        if xmax <= xmin or ymax <= ymin:
            continue
        detections.append(
            {
                "source_detection_index": source_index,
                "class_id": class_id,
                "label": LABELS[class_id],
                "score": score,
                "bbox": [xmin, ymin, xmax, ymax],
                "order_seq": int(row[6]),
            }
        )
    detections.sort(
        key=lambda item: (item["order_seq"], item["source_detection_index"])
    )
    return detections


def session_schema(session: ort.InferenceSession) -> dict:
    """Returns stable input and output names, element types, and dimensions."""
    return {
        "inputs": [
            {"name": value.name, "dtype": value.type, "shape": value.shape}
            for value in session.get_inputs()
        ],
        "outputs": [
            {"name": value.name, "dtype": value.type, "shape": value.shape}
            for value in session.get_outputs()
        ],
    }


def generate_oracle(
    model_directory: Path,
    input_path: Path,
    dump_resized_directory: Path | None = None,
) -> dict:
    """Generates one path-independent oracle from fixed model and image bytes."""
    model_path = model_directory / "inference.onnx"
    if not model_path.is_file():
        raise OracleError(f"model not found: {model_path}")
    if not input_path.is_file():
        raise OracleError(f"input image not found: {input_path}")

    data, batch = official_preprocess(input_path)
    session, outputs = run_onnx(model_path, batch)
    image_size, image, scale_factor = batch
    if dump_resized_directory is not None:
        dump_resized_directory.mkdir(parents=True, exist_ok=True)
        resized = np.rint(image[0].transpose(1, 2, 0) * 255.0).astype(np.uint8)
        (dump_resized_directory / f"{input_path.stem}.rgb").write_bytes(
            resized.tobytes(order="C")
        )
        (dump_resized_directory / f"{input_path.stem}.f32").write_bytes(
            np.ascontiguousarray(image)
            .astype("<f4", copy=False)
            .tobytes(order="C")
        )
    bbox_rows, bbox_num, masks = outputs
    if bbox_rows.shape != (300, 7):
        raise OracleError(f"unsupported bbox output shape: {bbox_rows.shape}")
    if bbox_num.shape != (1,) or int(bbox_num[0]) != len(bbox_rows):
        raise OracleError(
            f"unsupported bbox_num output: shape={bbox_num.shape}, values={bbox_num}"
        )
    if masks.shape != (300, 200, 200):
        raise OracleError(f"unsupported mask output shape: {masks.shape}")

    original = data[0]["ori_img"]
    original_height, original_width = original.shape[:2]
    return {
        "paddlex_source_commit": PADDLEX_SOURCE_COMMIT,
        "model_revision": MODEL_REVISION,
        "versions": {
            "paddleocr": importlib.metadata.version("paddleocr"),
            "paddlex": importlib.metadata.version("paddlex"),
            "onnxruntime": importlib.metadata.version("onnxruntime"),
            "numpy": importlib.metadata.version("numpy"),
            "opencv_contrib_python": importlib.metadata.version(
                "opencv-contrib-python"
            ),
        },
        "opencv_profile": {
            "threads": 1,
            "optimized": False,
            "ipp": False,
        },
        "input": {
            "basename": input_path.name,
            "width": original_width,
            "height": original_height,
            "color_mode": "RGB",
            "sha256": sha256_file(input_path),
            # PaddleX retains ori_img in OpenCV BGR while the model path uses RGB.
            "decoded_rgb_sha256": array_sha256(original[:, :, ::-1], "|u1"),
        },
        "tensor": array_contract(image, "image", "<f4"),
        "image_size": [float(value) for value in image_size[0]],
        "scale_factor": [float(value) for value in scale_factor[0]],
        "schema": session_schema(session),
        "raw_outputs": {
            "bbox": array_contract(bbox_rows, "fetch_name_0", "<f4"),
            "bbox_num": {
                **array_contract(bbox_num, "fetch_name_1", "<i4"),
                "values": [int(value) for value in bbox_num],
            },
            "masks": array_contract(masks, "fetch_name_2", "<i4"),
        },
        "threshold": DEFAULT_THRESHOLD,
        "postprocess_profile": {
            "layout_nms": False,
            "layout_unclip_ratio": None,
            "layout_merge_bboxes_mode": None,
            "filter_overlap_boxes": False,
            "consume_instance_masks": False,
        },
        "detections": lossless_detections(
            bbox_rows,
            original_width=original_width,
            original_height=original_height,
            threshold=DEFAULT_THRESHOLD,
        ),
    }


def collect_input_paths(
    explicit_inputs: list[Path] | None, input_directory: Path | None
) -> list[Path]:
    """Collect unique image inputs in stable basename order."""
    inputs = list(explicit_inputs or [])
    if input_directory is not None:
        if not input_directory.is_dir():
            raise OracleError(f"input directory not found: {input_directory}")
        inputs.extend(
            path
            for path in input_directory.iterdir()
            if path.is_file() and path.suffix.lower() == ".png"
        )
    if not inputs:
        raise OracleError("at least one --input or --input-dir PNG is required")
    by_basename: dict[str, Path] = {}
    for path in inputs:
        if path.name in by_basename:
            raise OracleError(f"duplicate input basename: {path.name}")
        by_basename[path.name] = path
    return [by_basename[basename] for basename in sorted(by_basename)]


def parse_args() -> argparse.Namespace:
    """Parses paths for single- or multi-input deterministic oracle generation."""
    parser = argparse.ArgumentParser(
        description="Generate the fixed PP-DocLayoutV3 Python oracle."
    )
    parser.add_argument("--model-dir", required=True, type=Path)
    parser.add_argument("--input", action="append", type=Path)
    parser.add_argument("--input-dir", type=Path)
    parser.add_argument("--dump-resized-dir", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def main() -> int:
    """Writes a single legacy oracle or a stable multi-sample collection."""
    arguments = parse_args()
    try:
        inputs = collect_input_paths(arguments.input, arguments.input_dir)
        samples = [
            generate_oracle(
                arguments.model_dir,
                input_path,
                arguments.dump_resized_dir,
            )
            for input_path in inputs
        ]
        oracle = (
            samples[0]
            if len(samples) == 1 and arguments.input_dir is None
            else {"schema_version": 1, "samples": samples}
        )
        payload = json.dumps(oracle, indent=2, sort_keys=True) + "\n"
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(payload, encoding="utf-8")
    except (OSError, OracleError) as error:
        print(f"error: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
