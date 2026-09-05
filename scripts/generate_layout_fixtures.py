#!/usr/bin/env python3
"""Generate deterministic synthetic RGB PNGs for layout parity tests."""

from __future__ import annotations

import argparse
import struct
import zlib
from pathlib import Path


def rectangle(
    pixels: bytearray,
    width: int,
    height: int,
    bounds: tuple[int, int, int, int],
    color: tuple[int, int, int],
) -> None:
    """Fill one clipped axis-aligned RGB rectangle."""
    left, top, right, bottom = bounds
    for y in range(max(0, top), min(height, bottom)):
        for x in range(max(0, left), min(width, right)):
            offset = (y * width + x) * 3
            pixels[offset : offset + 3] = bytes(color)


def png_chunk(kind: bytes, payload: bytes) -> bytes:
    """Encode one PNG chunk with its deterministic CRC."""
    body = kind + payload
    return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))


def write_png(path: Path, width: int, height: int, pixels: bytearray) -> None:
    """Write a filter-zero, compression-level-nine RGB PNG."""
    rows = b"".join(
        b"\x00" + bytes(pixels[y * width * 3 : (y + 1) * width * 3])
        for y in range(height)
    )
    payload = (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(
            b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
        )
        + png_chunk(b"IDAT", zlib.compress(rows, level=9))
        + png_chunk(b"IEND", b"")
    )
    path.write_bytes(payload)


def canvas(width: int, height: int) -> bytearray:
    """Create an opaque white RGB canvas."""
    return bytearray([255]) * width * height * 3


def portrait() -> tuple[int, int, bytearray]:
    """Create a portrait page with a title, two columns, and a visual object."""
    width, height = 640, 900
    pixels = canvas(width, height)
    rectangle(pixels, width, height, (70, 45, 570, 78), (25, 25, 25))
    for column_left in (55, 335):
        for row in range(24):
            top = 125 + row * 25
            line_width = 230 - (row % 4) * 18
            rectangle(
                pixels,
                width,
                height,
                (column_left, top, column_left + line_width, top + 7),
                (45, 45, 45),
            )
    rectangle(pixels, width, height, (350, 420, 575, 610), (180, 200, 220))
    return width, height, pixels


def landscape() -> tuple[int, int, bytearray]:
    """Create a landscape page with three columns and a full-width banner."""
    width, height = 1200, 700
    pixels = canvas(width, height)
    rectangle(pixels, width, height, (80, 40, 1120, 78), (20, 20, 20))
    for column_left in (60, 420, 780):
        for row in range(20):
            top = 120 + row * 25
            rectangle(
                pixels,
                width,
                height,
                (column_left, top, column_left + 300 - row % 5 * 20, top + 8),
                (55, 55, 55),
            )
    return width, height, pixels


def blank() -> tuple[int, int, bytearray]:
    """Create a completely blank square page."""
    width, height = 800, 800
    return width, height, canvas(width, height)


def dense_overlap() -> tuple[int, int, bytearray]:
    """Create dense intersecting visual and text-like regions."""
    width, height = 900, 900
    pixels = canvas(width, height)
    for index in range(28):
        offset = index * 24
        rectangle(
            pixels,
            width,
            height,
            (35 + offset // 3, 35 + offset, 850 - offset // 4, 48 + offset),
            (30 + index % 4 * 30, 45, 70),
        )
    for index in range(10):
        left = 80 + index * 65
        rectangle(
            pixels,
            width,
            height,
            (left, 180, left + 180, 720),
            (200, 210 - index * 6, 220),
        )
    return width, height, pixels


def generate(output_dir: Path) -> None:
    """Write all named fixtures in stable basename order."""
    output_dir.mkdir(parents=True, exist_ok=True)
    fixtures = {
        "blank.png": blank(),
        "dense-overlap.png": dense_overlap(),
        "landscape.png": landscape(),
        "portrait.png": portrait(),
    }
    for basename, (width, height, pixels) in fixtures.items():
        write_png(output_dir / basename, width, height, pixels)


def parse_args() -> argparse.Namespace:
    """Parse the fixture output directory."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def main() -> None:
    """Generate every fixture requested by the parity suite."""
    arguments = parse_args()
    generate(arguments.output_dir)


if __name__ == "__main__":
    main()
