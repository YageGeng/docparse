"""Behavior tests for the reproducible model download script."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from dataclasses import replace
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

    def test_tatr_export_install_repair_and_hash_failure(self):
        """TATR uses export only when needed and never publishes a mismatched graph."""
        self.assertIn("tatr-v1.1-all", self.module.MODEL_NAMES)
        model = self.module.Model.from_name("tatr-v1.1-all")
        self.assertEqual(model.license, "MIT")
        payloads = {"inference.onnx": b"exported-graph", "inference.yml": b"preprocessor"}
        model = replace(model, artifacts=tuple(replace(artifact,
            sha256=hashlib.sha256(payloads[artifact.filename]).hexdigest()) for artifact in model.artifacts))
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "tatr"
            with mock.patch.object(self.module, "export_tatr", side_effect=lambda path: path.write_bytes(payloads["inference.onnx"])) as export, mock.patch.object(self.module, "urlopen", return_value=io.BytesIO(payloads["inference.yml"])):
                self.assertTrue(self.module.install_model(output, False, model))
                export.assert_called_once()
            self.module.verify_installation(output, model)
            (output / "model-manifest.json").unlink()
            with mock.patch.object(self.module, "export_tatr", side_effect=AssertionError("unexpected export")), mock.patch.object(self.module, "urlopen", side_effect=AssertionError("unexpected network")):
                self.assertTrue(self.module.install_model(output, False, model))
                self.assertFalse(self.module.install_model(output, False, model))
            with mock.patch.object(self.module, "export_tatr", side_effect=lambda path: path.write_bytes(b"wrong")):
                with self.assertRaisesRegex(self.module.ModelDownloadError, "hash mismatch"):
                    self.module.install_model(output, True, model)
            self.module.verify_installation(output, model)

    def test_texo_command_installs_and_verifies_its_three_assets(self):
        """Texo provisioning includes both graphs and its tokenizer with model-specific provenance."""
        self.assertIn("texo", self.module.MODEL_NAMES)
        model = self.module.Model.from_name("texo")
        payloads = {
            "encoder_model.onnx": b"texo-encoder",
            "decoder_model_merged.onnx": b"texo-decoder",
            "tokenizer.json": b"texo-tokenizer",
        }
        # Keep the real catalog identity while replacing large remote assets with small payloads.
        model = replace(model, artifacts=tuple(
            replace(artifact, sha256=hashlib.sha256(payloads[artifact.filename]).hexdigest())
            for artifact in model.artifacts
        ))
        with (
            tempfile.TemporaryDirectory() as directory,
            mock.patch.object(self.module.Model, "from_name", return_value=model),
            mock.patch.object(sys, "argv", [
                "download_models.py", "--model", "texo", "--models-dir", directory,
            ]),
            mock.patch.object(sys, "stdout", io.StringIO()),
        ):
            output = Path(directory) / "texo"
            with mock.patch.object(self.module, "urlopen", side_effect=[
                io.BytesIO(payloads[artifact.filename]) for artifact in model.artifacts
            ]):
                self.assertEqual(self.module.main(), 0)
            for filename, payload in payloads.items():
                self.assertEqual((output / filename).read_bytes(), payload)
            manifest = self.module.verify_installation(output, model)
            self.assertEqual(manifest["repository"], "alephpi/FormulaNet")
            self.assertEqual(manifest["license"], "AGPL-3.0")
            with mock.patch.object(self.module, "urlopen", side_effect=AssertionError("unexpected download")):
                self.assertEqual(self.module.main(), 0)
                sys.argv.append("--verify-only")
                self.assertEqual(self.module.main(), 0)
            # A global Apache license must not silently validate a Texo manifest.
            manifest["license"] = "Apache-2.0"
            (output / "model-manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(self.module.ModelDownloadError, "license"):
                self.module.verify_installation(output, model)

    def test_formula_model_has_matching_pinned_tokenizer(self):
        """All supported formula variants retain their own graph and the matching BPE tokenizer."""
        for name, digest in [
            ("pp-formulanet-plus-s", "449d205c8fb2fe0a9b134a5e4a0f2421c2e7812fd902ea67dfda4e9ef4588978"),
            ("pp-formulanet-plus-m", "9e3539c2b4eeed28f2d35e342fd5bb0bdaa7f6034a475fc7e890c92780910618"),
            ("pp-formulanet-plus-l", "b4924d69c731365048de3d11a5d1829f3dfd8b98b4dbfd82437f934c2611934f"),
        ]:
            self.assertIn(name, self.module.MODEL_NAMES)
            model = self.module.Model.from_name(name)
            self.assertEqual([artifact.filename for artifact in model.artifacts], ["inference.onnx", "tokenizer.json"])
            self.assertEqual(model.artifacts[0].sha256, digest)
            self.assertEqual(model.artifacts[1].sha256, "2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2")

    def test_table_comparison_models_have_pinned_artifact_pairs(self):
        """Every selectable table model can be provisioned with immutable model and YAML identities."""
        for name in ("slanext-wired", "slanext-wireless", "rtdetr-table-cell-wired", "rtdetr-table-cell-wireless"):
            model = self.module.Model.from_name(name)
            self.assertIn(name, self.module.MODEL_NAMES)
            self.assertEqual(len(model.revision), 40)
            self.assertEqual([artifact.filename for artifact in model.artifacts], ["inference.onnx", "inference.yml"])
            self.assertTrue(all(len(artifact.sha256) == 64 for artifact in model.artifacts))

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
                    self.module.ModelDownloadError("download failed"),
                    *([False] * (len(self.module.MODEL_NAMES) - 1)),
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
