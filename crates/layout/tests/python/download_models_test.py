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

    def test_default_command_selects_all_models(self):
        """The ordinary provisioning command must include layout, TSR and all three OCR models."""
        with mock.patch.object(sys, "argv", ["download_models.py"]):
            self.assertEqual(self.module.parse_args().model, "all")

    def test_default_command_checks_every_model_and_reports_partial_failure(self):
        """All models are attempted under the chosen root, while a single failure keeps the exit status nonzero."""
        with tempfile.TemporaryDirectory() as directory:
            with (
                mock.patch.object(sys, "argv", ["download_models.py", "--models-dir", directory]),
                mock.patch.object(sys, "stdout", io.StringIO()),
                mock.patch.object(sys, "stderr", io.StringIO()),
                mock.patch.object(self.module, "install_model", side_effect=[
                    self.module.ModelDownloadError("download failed"), False, False, False, False,
                ]) as installer,
            ):
                self.assertEqual(self.module.main(), 1)
            self.assertEqual([call.args[0] for call in installer.call_args_list], [
                Path(directory) / name for name in self.module.MODEL_NAMES
            ])
            self.assertEqual([call.kwargs["model"].name for call in installer.call_args_list], list(self.module.MODEL_NAMES))

    def test_manifest_and_corrupt_yaml_are_repaired_without_refetching_weights(self):
        """Metadata-only damage needs no network, and a corrupt YAML downloads only that artifact."""
        with (
            tempfile.TemporaryDirectory() as directory,
            mock.patch.object(self.module, "ARTIFACTS", self.artifacts),
            mock.patch.object(self.module, "urlopen", side_effect=self.fake_urlopen) as opener,
        ):
            output = Path(directory) / "model"
            self.module.install_model(output, force=False)
            weights = output / "inference.onnx"
            original = weights.stat().st_mtime_ns
            for content in ["{broken", "[]"]:
                (output / "model-manifest.json").write_text(content)
                opener.reset_mock()
                self.assertTrue(self.module.install_model(output, force=False))
                opener.assert_not_called()
                self.module.verify_installation(output)
            (output / "inference.yml").write_bytes(b"corrupt")
            opener.reset_mock()
            self.assertTrue(self.module.install_model(output, force=False))
            self.assertEqual(opener.call_count, 1)
            self.assertEqual((output / "inference.yml").read_bytes(), self.config_bytes)
            self.assertEqual(weights.stat().st_mtime_ns, original)

    def test_force_downloads_even_valid_artifacts(self):
        """Explicit force bypasses the local-validity shortcut for the selected model."""
        with (
            tempfile.TemporaryDirectory() as directory,
            mock.patch.object(self.module, "ARTIFACTS", self.artifacts),
            mock.patch.object(self.module, "urlopen", side_effect=self.fake_urlopen) as opener,
        ):
            output = Path(directory) / "model"
            self.module.install_model(output, force=False)
            opener.reset_mock()
            self.assertTrue(self.module.install_model(output, force=True))
            self.assertEqual(opener.call_count, 2)
            self.module.verify_installation(output)

    def test_partial_install_downloads_only_the_missing_file(self):
        """Existing verified weights survive a missing YAML file without another weight download."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "model"
            output.mkdir()
            weights = output / "inference.onnx"
            weights.write_bytes(self.model_bytes)
            original = weights.stat().st_mtime_ns
            with (
                mock.patch.object(self.module, "ARTIFACTS", self.artifacts),
                mock.patch.object(self.module, "urlopen", side_effect=self.fake_urlopen) as opener,
            ):
                self.assertTrue(self.module.install_model(output, force=False))
                self.module.verify_installation(output)
            self.assertEqual(opener.call_count, 1)
            self.assertEqual(weights.stat().st_mtime_ns, original)


if __name__ == "__main__":
    unittest.main()
