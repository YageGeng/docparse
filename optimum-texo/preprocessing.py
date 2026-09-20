"""Texo preprocessing copied from the pinned DocParse Python reference transform."""

import numpy as np
from numpy.typing import NDArray
from PIL import Image, ImageOps


def preprocess(image: Image.Image) -> NDArray[np.float32]:
    """Follow upstream EvalMERImageProcessor's Pillow and OpenCV rounding contract."""
    image = image.convert("RGB")
    gray = np.asarray(image.convert("L"))
    lo, hi = int(gray.min()), int(gray.max())
    if hi > lo:
        y, x = np.where((gray.astype(np.float64) - lo) / (hi - lo) * 255 < 200)
        if len(x):
            image = image.crop(
                (int(x.min()), int(y.min()), int(x.max()) + 1, int(y.max()) + 1)
            )
    w, h = image.size
    size = (384, int(384 * h / w)) if w < h else (int(384 * w / h), 384)
    # Match Rust's intermediate allocation limit after margin cropping, which can expose very thin ink.
    if size[0] * size[1] > 16_777_216:
        raise ValueError(
            "Texo crop aspect ratio exceeds the preprocessing allocation limit"
        )
    image = image.resize(size, Image.Resampling.BILINEAR)
    image.thumbnail((384, 384), Image.Resampling.BICUBIC, reducing_gap=2.0)
    w, h = image.size
    left, top = (384 - w) // 2, (384 - h) // 2
    image = ImageOps.expand(image, (left, top, 384 - w - left, 384 - h - top), fill=0)
    rgb = np.asarray(image).astype(np.uint32)
    gray = (
        (rgb[..., 0] * 4899 + rgb[..., 1] * 9617 + rgb[..., 2] * 1868 + 8192) >> 14
    ).astype(np.float32)
    normalized = (gray - np.float32(202.2405)) * np.float32(0.022563687)
    return np.repeat(normalized[None, None], 3, axis=1)
