"""Check allocation limits and reference parity without loading CUDA models."""

import hashlib
import json
import sys
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from preprocessing import preprocess
from runtime import generate


class PreprocessingTests(unittest.TestCase):
    """Keep unsafe intermediate allocations out of the reference transform."""

    def test_rejects_extreme_crops_before_resize(self):
        """Reject both axes and thin ink inside an otherwise ordinary upload."""
        cropped = Image.new("RGB", (1000, 1000), "white")
        cropped.paste("black", (0, 500, 1000, 501))
        for image in [
            Image.new("RGB", (10000, 1)),
            Image.new("RGB", (1, 10000)),
            cropped,
        ]:
            # Enter all guards together while retaining the allocation interception.
            with (
                self.subTest(size=image.size),
                patch.object(
                    Image.Image,
                    "resize",
                    side_effect=AssertionError("unsafe resize reached"),
                ),
                self.assertRaisesRegex(ValueError, "allocation limit"),
            ):
                preprocess(image)

    def test_allows_bounded_wide_crop(self):
        """Keep ordinary wide formulas supported by the existing transform."""
        pixels = preprocess(Image.new("RGB", (100, 1), "white"))
        self.assertEqual(pixels.shape, (1, 3, 384, 384))
        self.assertTrue(np.isfinite(pixels).all())

    def test_reference_pixels_are_unchanged(self):
        """Match the independent fixture hashes used by the Rust implementation."""
        fixtures = (
            Path(__file__).resolve().parents[2] / "crates/formula-texo/tests/fixtures"
        )
        for case in json.loads((fixtures / "reference.json").read_text())["cases"]:
            with (
                self.subTest(image=case["image"]),
                Image.open(fixtures / case["image"]) as image,
            ):
                digest = hashlib.sha256(
                    preprocess(image).astype("<f4").tobytes()
                ).hexdigest()
                self.assertEqual(digest, case["preprocess_sha256"])

    def test_invalid_crop_does_not_fail_batch_peers(self):
        """Exercise real preprocessing and result ordering with CUDA dependencies replaced."""
        # Runtime imports are now side-effect free, so test the function without dynamic module loading.
        torch = MagicMock()
        model, tokenizer = MagicMock(), MagicMock()
        model.generate.return_value.cpu.return_value.tolist.return_value = [
            [0, 7, 2],
            [0, 8, 2],
        ]
        tokenizer.eos_token_id = 2
        tokenizer.decode.side_effect = ["x", "y"]
        good = Image.new("RGB", (32, 16), "white")
        bad = Image.new("RGB", (10000, 1), "white")
        # Never allocate the unsafe intermediate even when checking a regressed implementation.
        resize = Image.Image.resize

        def bounded_resize(image, size, *args, **kwargs):
            """Stop accidental multi-gigabyte allocation before invoking Pillow."""
            if size[0] * size[1] > 16_777_216:
                raise AssertionError("unsafe resize reached")
            return resize(image, size, *args, **kwargs)

        with (
            patch.object(Image.Image, "resize", bounded_resize),
            patch.dict(sys.modules, {"torch": torch}),
        ):
            results = generate(model, tokenizer, [bad, good, bad, good])
            # Check the response value type before applying string-specific assertions.
            for index in (0, 2):
                error = results[index]["error"]
                assert isinstance(error, str)
                self.assertIn("allocation limit", error)
            self.assertEqual(results[1]["text"], "x")
            self.assertEqual(results[3]["text"], "y")
            pixels = torch.from_numpy.call_args.args[0]
            self.assertEqual(pixels.shape, (2, 3, 384, 384))
            model.generate.side_effect = AssertionError(
                "invalid-only batch reached CUDA"
            )
            error = generate(model, tokenizer, [bad])[0]["error"]
            assert isinstance(error, str)
            self.assertIn("allocation limit", error)


if __name__ == "__main__":
    unittest.main()
