#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "numpy==2.3.5",
#   "onnxruntime==1.29.0",
#   "opencv-contrib-python==4.10.0.84",
#   "paddleocr==3.6.0",
# ]
# ///
"""Contract tests for the fixed Python layout oracle."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


def load_script_module():
    """Loads the reference script from the repository scripts directory."""
    script_path = Path(__file__).parents[4] / "scripts" / "reference_layout.py"
    spec = importlib.util.spec_from_file_location("reference_layout", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("failed to create the reference_layout module spec")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ReferenceLayoutTest(unittest.TestCase):
    """Runs the real fixed ONNX model through the official preprocessing path."""

    @classmethod
    def setUpClass(cls):
        """Generates the oracle twice for deterministic comparison."""
        cls.module = load_script_module()
        repository = Path(__file__).parents[4]
        cls.input_path = repository / "crates/layout/tests/fixtures/model/input.png"
        model_directory = repository / "models/pp-doclayout-v3"
        cls.first = cls.module.generate_oracle(model_directory, cls.input_path)
        cls.second = cls.module.generate_oracle(model_directory, cls.input_path)

    def test_preprocessing_contract_is_exact(self):
        """The official processors produce the fixed tensor shape and scales."""
        self.assertEqual(self.first["input"]["width"], 1024)
        self.assertEqual(self.first["input"]["height"], 640)
        self.assertEqual(self.first["input"]["color_mode"], "RGB")
        self.assertEqual(self.first["tensor"]["dtype"], "float32")
        self.assertEqual(self.first["tensor"]["shape"], [1, 3, 800, 800])
        self.assertEqual(self.first["image_size"], [800.0, 800.0])
        self.assertEqual(self.first["scale_factor"], [1.25, 0.78125])
        self.assertEqual(
            self.first["opencv_profile"],
            {"threads": 1, "optimized": False, "ipp": False},
        )

    def test_fixed_onnx_schema_and_mask_contract_are_recorded(self):
        """The oracle records all three real outputs without inventing votes."""
        outputs = self.first["raw_outputs"]
        self.assertEqual(outputs["bbox"]["dtype"], "float32")
        self.assertEqual(outputs["bbox"]["shape"], [300, 7])
        self.assertEqual(outputs["bbox_num"]["dtype"], "int32")
        self.assertEqual(outputs["bbox_num"]["values"], [300])
        self.assertEqual(outputs["masks"]["dtype"], "int32")
        self.assertEqual(outputs["masks"]["shape"], [300, 200, 200])
        self.assertNotIn("order_votes", json.dumps(self.first, sort_keys=True))

    def test_lossless_detections_retain_source_indices_and_order(self):
        """Thresholding retains stable raw row indices and exported order values."""
        detections = self.first["detections"]
        self.assertGreater(len(detections), 0)
        self.assertTrue(all(item["score"] > 0.5 for item in detections))
        self.assertTrue(all("source_detection_index" in item for item in detections))
        self.assertTrue(all("order_seq" in item for item in detections))
        self.assertEqual(
            detections,
            sorted(
                detections,
                key=lambda item: (item["order_seq"], item["source_detection_index"]),
            ),
        )

    def test_oracle_is_deterministic_and_path_independent(self):
        """Repeated oracle generation is byte-stable and omits absolute paths."""
        first = json.dumps(self.first, indent=2, sort_keys=True) + "\n"
        second = json.dumps(self.second, indent=2, sort_keys=True) + "\n"
        self.assertEqual(first, second)
        self.assertNotIn(str(self.input_path.parent), first)


if __name__ == "__main__":
    unittest.main()
