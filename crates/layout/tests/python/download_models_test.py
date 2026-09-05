"""Behavior tests for the reproducible model download script."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


def load_script_module():
    """Loads the repository script as a normal Python module."""
    script_path = Path(__file__).parents[4] / "scripts" / "download_models.py"
    spec = importlib.util.spec_from_file_location("download_models", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("failed to create the download_models module spec")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class DownloadModelsTest(unittest.TestCase):
    """Exercises downloads without accessing the network."""

    def setUp(self):
        """Creates deterministic artifact contracts for each test."""
        self.module = load_script_module()
        self.model_bytes = b"model-content"
        self.config_bytes = b"config-content"
        self.artifacts = (
            self.module.Artifact(
                filename="inference.onnx",
                url="https://example.invalid/inference.onnx",
                sha256=hashlib.sha256(self.model_bytes).hexdigest(),
            ),
            self.module.Artifact(
                filename="inference.yml",
                url="https://example.invalid/inference.yml",
                sha256=hashlib.sha256(self.config_bytes).hexdigest(),
            ),
        )

    def fake_urlopen(self, request, timeout):
        """Returns bytes for a fixed request URL and validates the timeout."""
        self.assertEqual(timeout, 120)
        payloads = {
            self.artifacts[0].url: self.model_bytes,
            self.artifacts[1].url: self.config_bytes,
        }
        return io.BytesIO(payloads[request.full_url])

    def test_install_verifies_files_and_skips_an_identical_second_run(self):
        """A valid install is atomic and a second run performs no downloads."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "model"
            with (
                mock.patch.object(self.module, "ARTIFACTS", self.artifacts),
                mock.patch.object(
                    self.module, "urlopen", side_effect=self.fake_urlopen
                ) as opener,
            ):
                changed = self.module.install_model(output, force=False)
                unchanged = self.module.install_model(output, force=False)
                manifest = self.module.verify_installation(output)

            self.assertTrue(changed)
            self.assertFalse(unchanged)
            self.assertEqual(opener.call_count, 2)
            self.assertEqual((output / "inference.onnx").read_bytes(), self.model_bytes)
            self.assertEqual((output / "inference.yml").read_bytes(), self.config_bytes)
            self.assertEqual(manifest["revision"], self.module.MODEL_REVISION)

    def test_hash_mismatch_does_not_publish_partial_files(self):
        """Invalid download bytes leave no model artifacts in the output directory."""
        invalid = (
            self.module.Artifact(
                filename="inference.onnx",
                url="https://example.invalid/inference.onnx",
                sha256="0" * 64,
            ),
        )
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "model"
            with (
                mock.patch.object(self.module, "ARTIFACTS", invalid),
                mock.patch.object(
                    self.module,
                    "urlopen",
                    return_value=io.BytesIO(self.model_bytes),
                ),
                self.assertRaises(self.module.ModelDownloadError),
            ):
                self.module.install_model(output, force=False)

            self.assertFalse((output / "inference.onnx").exists())
            self.assertFalse((output / "model-manifest.json").exists())

    def test_verify_only_rejects_a_missing_installation(self):
        """Verification fails when any required artifact is absent."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "missing"
            with (
                mock.patch.object(self.module, "ARTIFACTS", self.artifacts),
                self.assertRaises(self.module.ModelDownloadError),
            ):
                self.module.verify_installation(output)


if __name__ == "__main__":
    unittest.main()
