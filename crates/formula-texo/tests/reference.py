"""Regenerate golden outputs using Python ONNX Runtime and Pillow, without Rust code.

Run from the repository root:
uv run --no-project --with onnxruntime --with pillow tests/reference.py MODEL_DIRECTORY
Use this file's full path for tests/reference.py. Model files are downloaded separately.
"""
import hashlib
import json
from pathlib import Path
import sys

import numpy as np
import onnxruntime as ort
from PIL import Image, ImageOps


def preprocess(path):
    """Follow upstream EvalMERImageProcessor's Pillow and OpenCV rounding contract."""
    image = Image.open(path).convert("RGB")
    gray = np.asarray(image.convert("L"))
    lo, hi = int(gray.min()), int(gray.max())
    if hi > lo:
        y, x = np.where((gray.astype(np.float64) - lo) / (hi - lo) * 255 < 200)
        if len(x):
            image = image.crop((int(x.min()), int(y.min()), int(x.max()) + 1, int(y.max()) + 1))
    w, h = image.size
    size = (384, int(384 * h / w)) if w < h else (int(384 * w / h), 384)
    image = image.resize(size, Image.Resampling.BILINEAR)
    image.thumbnail((384, 384), Image.Resampling.BICUBIC, reducing_gap=2.0)
    w, h = image.size
    left, top = (384 - w) // 2, (384 - h) // 2
    image = ImageOps.expand(image, (left, top, 384 - w - left, 384 - h - top), fill=0)
    rgb = np.asarray(image).astype(np.uint32)
    gray = ((rgb[..., 0] * 4899 + rgb[..., 1] * 9617 + rgb[..., 2] * 1868 + 8192) >> 14).astype(np.float32)
    normalized = (gray - np.float32(202.2405)) * np.float32(0.022563687)
    return np.repeat(normalized[None, None], 3, axis=1)


def main():
    """Write token sequences and preprocessing hashes for the pinned author-published graphs."""
    root = Path(sys.argv[1])
    fixtures = Path(__file__).parent / "fixtures"
    options = ort.SessionOptions()
    options.intra_op_num_threads = 1
    # Keep shape warnings visible when validating updated exports against the golden outputs.
    options.log_severity_level = 2
    encoder = ort.InferenceSession(str(root / "encoder_model.onnx"), options, providers=["CPUExecutionProvider"])
    decoder = ort.InferenceSession(str(root / "decoder_model_merged.onnx"), options, providers=["CPUExecutionProvider"])
    tokenizer = json.loads((root / "tokenizer.json").read_text())
    vocabulary = {value: key for key, value in tokenizer["model"]["vocab"].items()}
    cases = []
    for path in sorted(fixtures.glob("*.png")):
        pixels = preprocess(path)
        hidden = encoder.run(None, {"pixel_values": pixels})[0]
        cache = {value.name: np.zeros((1, 16, 0, 24), dtype=np.float32) for value in decoder.get_inputs() if value.name.startswith("past_key_values")}
        ids = [0]
        for step in range(1023):
            inputs = dict(cache, input_ids=np.array([[ids[-1]]], dtype=np.int64), encoder_hidden_states=hidden, use_cache_branch=np.array([step > 0]))
            outputs = dict(zip([value.name for value in decoder.get_outputs()], decoder.run(None, inputs)))
            ids.append(int(outputs["logits"][0, -1].argmax()))
            if ids[-1] == 2:
                break
            for name in cache:
                if step == 0 or ".decoder." in name:
                    cache[name] = outputs[name.replace("past_key_values", "present")]
        assert ids[-1] == 2, f"{path.name} did not terminate"
        cases.append({"image": path.name, "preprocess_sha256": hashlib.sha256(pixels.astype("<f4").tobytes()).hexdigest(), "tokens": ids, "latex": " ".join(vocabulary[i] for i in ids if i > 3)})
        print(path.name, len(ids), cases[-1]["latex"])
    (fixtures / "reference.json").write_text(json.dumps({"revision": "b2668efe5112082846fde4d446b9bfaab3989533", "runtime": ort.__version__, "cases": cases}, indent=2) + "\n")


if __name__ == "__main__":
    main()
