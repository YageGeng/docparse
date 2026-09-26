"""Check the release's image transform, prompt, and visual splice without loading CUDA."""

import sys
import unittest
from pathlib import Path

import numpy as np
import torch
import torchvision
from PIL import Image
from torchvision.transforms.functional import InterpolationMode

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from preprocessing import (
    IMAGE_SIZE,
    IMAGE_TOKEN_LEN,
    SYSTEM_PROMPT,
    build_prompt,
    preprocess,
    splice_image_features,
)

# The released chat() applies OneChartImageEvalProcessor(image_size=1024).
RELEASE_PROCESSOR = torchvision.transforms.Compose(
    [
        torchvision.transforms.Resize(
            (IMAGE_SIZE, IMAGE_SIZE), interpolation=InterpolationMode.BICUBIC
        ),
        torchvision.transforms.ToTensor(),
        torchvision.transforms.Normalize((0.0, 0.0, 0.0), (1.0, 1.0, 1.0)),
    ]
)


def patterned_image(width: int = 640, height: int = 480) -> Image.Image:
    """Build a deterministic gradient so any interpolation change would be visible."""
    axis_x = np.linspace(0.0, 255.0, width, dtype=np.float32)
    axis_y = np.linspace(0.0, 255.0, height, dtype=np.float32)
    grid = (axis_y[:, None] + axis_x[None, :]) % 256.0
    return Image.fromarray(
        np.stack([grid, grid[::-1], 255.0 - grid], axis=-1).astype(np.uint8), "RGB"
    )


def placeholder_prompt(start_id: int = 11, patch_id: int = 12, end_id: int = 13):
    """Build the smallest input that carries one complete image placeholder run."""
    return np.asarray([[0, start_id] + [patch_id] * IMAGE_TOKEN_LEN + [end_id, 0]])


class PreprocessTests(unittest.TestCase):
    """Keep the released 1024x1024 bicubic transform byte-stable for every chart size."""

    def test_matches_the_released_image_processor(self):
        """Match OneChartImageEvalProcessor exactly, including its 0..1 scaling."""
        for size in [(640, 480), (1643, 686), (200, 900)]:
            with self.subTest(size=size):
                image = patterned_image(*size)
                reference = RELEASE_PROCESSOR(image)
                assert isinstance(reference, torch.Tensor)
                actual = preprocess(image)
                self.assertEqual(actual.shape, (1, 3, IMAGE_SIZE, IMAGE_SIZE))
                self.assertEqual(actual.dtype, np.float32)
                np.testing.assert_array_equal(actual[0], reference.numpy())

    def test_prompt_is_the_released_template(self):
        """Reproduce the v1 conversation framing around the fixed chart question."""
        prompt = build_prompt()
        self.assertTrue(prompt.startswith(SYSTEM_PROMPT + " USER: "))
        self.assertTrue(prompt.endswith(" ASSISTANT:"))
        self.assertEqual(prompt.count("<img>"), 1)
        self.assertEqual(prompt.count("<imgpad>"), IMAGE_TOKEN_LEN)
        self.assertEqual(prompt.count("</img>"), 1)
        self.assertIn(
            "Convert the key information of the chart to a python dict:", prompt
        )

    def test_splice_replaces_only_the_placeholder_run(self):
        """Overwrite the `<imgpad>` rows and leave the `<img>` and `</img>` rows untouched."""
        input_ids = placeholder_prompt()
        embeds = np.zeros((1, input_ids.shape[1], 4), dtype=np.float32)
        embeds[0, 1] = 1.0
        embeds[0, IMAGE_TOKEN_LEN + 2] = 2.0
        features = np.full((1, IMAGE_TOKEN_LEN, 4), 7.0, dtype=np.float32)
        spliced = splice_image_features(embeds, input_ids, features, 11, 13)
        # The source array must survive, because callers keep the embedding output on hand.
        self.assertEqual(float(embeds[0, 2, 0]), 0.0)
        self.assertEqual(float(spliced[0, 1, 0]), 1.0)
        self.assertEqual(float(spliced[0, IMAGE_TOKEN_LEN + 2, 0]), 2.0)
        np.testing.assert_array_equal(spliced[0, 2 : 2 + IMAGE_TOKEN_LEN], features[0])

    def test_splice_rejects_a_prompt_without_the_image_end(self):
        """Refuse a prompt whose placeholder run is not closed by `</img>`."""
        input_ids = np.asarray([[0, 11] + [12] * IMAGE_TOKEN_LEN + [0, 0]])
        embeds = np.zeros((1, input_ids.shape[1], 4), dtype=np.float32)
        features = np.zeros((1, IMAGE_TOKEN_LEN, 4), dtype=np.float32)
        with self.assertRaisesRegex(ValueError, "image end token"):
            splice_image_features(embeds, input_ids, features, 11, 13)

    def test_batch_rows_are_spliced_independently(self):
        """Give every row its own visual features, as a batched owner requires."""
        input_ids = np.repeat(placeholder_prompt(), 2, axis=0)
        embeds = np.zeros((2, input_ids.shape[1], 2), dtype=np.float32)
        features = np.stack(
            [
                np.full((IMAGE_TOKEN_LEN, 2), 3.0, dtype=np.float32),
                np.full((IMAGE_TOKEN_LEN, 2), 5.0, dtype=np.float32),
            ]
        )
        spliced = splice_image_features(embeds, input_ids, features, 11, 13)
        self.assertEqual(float(spliced[0, 2, 0]), 3.0)
        self.assertEqual(float(spliced[1, 2, 0]), 5.0)


class ReleaseTokenizerTests(unittest.TestCase):
    """Pin the token ids the released prompt depends on, when the snapshot is available."""

    def test_special_token_ids_and_no_leading_bos(self):
        """Keep `<img>`, `<imgpad>`, `</img>`, and `<Number>` aligned with the checkpoint."""
        directory = Path(__file__).resolve().parents[1] / "models/OneChart"
        if not directory.exists():
            self.skipTest("the OneChart snapshot has not been downloaded")
        from transformers import AutoTokenizer

        tokenizer = AutoTokenizer.from_pretrained(
            directory, trust_remote_code=True, use_fast=False, padding_side="right"
        )
        self.assertEqual(tokenizer.convert_tokens_to_ids("<imgpad>"), 50265)
        self.assertEqual(tokenizer.convert_tokens_to_ids("<img>"), 50266)
        self.assertEqual(tokenizer.convert_tokens_to_ids("</img>"), 50267)
        self.assertEqual(tokenizer.convert_tokens_to_ids("<Number>"), 50268)
        self.assertEqual(len(tokenizer), 50269)
        image_start = tokenizer.convert_tokens_to_ids("<img>")
        image_end = tokenizer.convert_tokens_to_ids("</img>")
        # The snapshot records add_bos_token, and later transformers act on it, which breaks
        # the release, so the deployment clears it before tokenizing.
        tokenizer.add_bos_token = False
        encoded = np.asarray(tokenizer([build_prompt()]).input_ids[0])
        self.assertNotEqual(encoded[0], tokenizer.bos_token_id)
        positions = np.flatnonzero(encoded == image_start)
        self.assertEqual(len(positions), 1)
        self.assertEqual(
            encoded[positions[0] + 1], tokenizer.convert_tokens_to_ids("<imgpad>")
        )
        self.assertEqual(encoded[positions[0] + 1 + IMAGE_TOKEN_LEN], image_end)


if __name__ == "__main__":
    unittest.main()
