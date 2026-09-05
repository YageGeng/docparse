# /// script
# requires-python = ">=3.11"
# dependencies = ["psutil==7.0.0", "pypdf==6.0.0"]
# ///
"""Behavior tests for strict real-PDF E2E preflight."""

from __future__ import annotations

import hashlib
import importlib.util
import sys
import tempfile
from pathlib import Path

from pypdf import PdfWriter


def load_runner():
    """Import the repository runner without executing its CLI entrypoint."""
    root = Path(__file__).resolve().parents[4]
    path = root / "scripts/run_real_pdf_e2e.py"
    spec = importlib.util.spec_from_file_location("run_real_pdf_e2e", path)
    if spec is None or spec.loader is None:
        raise AssertionError("runner module spec could not be created")
    module = importlib.util.module_from_spec(spec)
    # Dataclasses resolve postponed annotations through the registered module namespace.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def write_pdf(path: Path, page_count: int) -> None:
    """Create a small valid PDF with an exact number of blank pages."""
    writer = PdfWriter()
    for _ in range(page_count):
        writer.add_blank_page(width=100, height=100)
    with path.open("wb") as output:
        writer.write(output)


def write_manifest(path: Path, pdf_path: Path, *, page_count: int = 1) -> None:
    """Write one exact schema-one manifest for a generated test PDF."""
    payload = pdf_path.read_bytes()
    path.write_text(
        "\n".join(
            [
                "schema_version = 1",
                "",
                "[[documents]]",
                'logical_id = "sample"',
                f'basename = "{pdf_path.name}"',
                f'sha256 = "{hashlib.sha256(payload).hexdigest()}"',
                f"size_bytes = {len(payload)}",
                f"page_count = {page_count}",
                "",
            ]
        ),
        encoding="utf-8",
    )


def assert_raises(callable_value, expected_text: str) -> None:
    """Require one preflight call to fail with a diagnostic substring."""
    try:
        callable_value()
    except Exception as error:
        if expected_text not in str(error):
            raise AssertionError(
                f"expected {expected_text!r} in {error!r}"
            ) from error
    else:
        raise AssertionError("expected preflight failure")


def main() -> None:
    """Exercise success plus missing, extra, size, hash, page, and duplicate failures."""
    runner = load_runner()
    with tempfile.TemporaryDirectory(prefix="docparse e2e ") as temporary:
        root = Path(temporary)
        pdf_dir = root / "PDF files with spaces"
        pdf_dir.mkdir()
        pdf_path = pdf_dir / "sample with spaces.PDF"
        write_pdf(pdf_path, 1)
        manifest_path = root / "corpus.toml"
        write_manifest(manifest_path, pdf_path)
        documents = runner.load_manifest(manifest_path)
        verified = runner.verify_pdf_corpus(documents, pdf_dir)
        assert verified[0][1] == pdf_path
        build_command = runner.cargo_build_command("cuda", "release")
        assert isinstance(build_command, list)
        assert "layout-cuda" in build_command
        assert "--release" in build_command
        assert "--no-run" in build_command
        assert "--message-format=json" in build_command
        executable = runner.parse_harness_executable(
            [
                '{"reason":"compiler-artifact","target":{"name":"real_pdfs"},'
                '"profile":{"test":true},"executable":"/tmp/real_pdfs-test"}'
            ]
        )
        assert executable == Path("/tmp/real_pdfs-test")
        assert runner.harness_command(executable) == [
            "/tmp/real_pdfs-test",
            "--ignored",
            "--nocapture",
        ]

        extra = pdf_dir / "extra.pdf"
        write_pdf(extra, 1)
        assert_raises(
            lambda: runner.verify_pdf_corpus(documents, pdf_dir), "extra.pdf"
        )
        extra.unlink()

        pdf_path.unlink()
        assert_raises(
            lambda: runner.verify_pdf_corpus(documents, pdf_dir), "missing="
        )
        write_pdf(pdf_path, 1)

        original = manifest_path.read_text(encoding="utf-8")
        manifest_path.write_text(
            original.replace(f"size_bytes = {pdf_path.stat().st_size}", "size_bytes = 1"),
            encoding="utf-8",
        )
        assert_raises(
            lambda: runner.verify_pdf_corpus(
                runner.load_manifest(manifest_path), pdf_dir
            ),
            "size mismatch",
        )
        write_manifest(manifest_path, pdf_path, page_count=2)
        assert_raises(
            lambda: runner.verify_pdf_corpus(
                runner.load_manifest(manifest_path), pdf_dir
            ),
            "page count mismatch",
        )

        write_manifest(manifest_path, pdf_path)
        text = manifest_path.read_text(encoding="utf-8")
        manifest_path.write_text(
            text.replace(hashlib.sha256(pdf_path.read_bytes()).hexdigest(), "0" * 64),
            encoding="utf-8",
        )
        assert_raises(
            lambda: runner.verify_pdf_corpus(
                runner.load_manifest(manifest_path), pdf_dir
            ),
            "SHA-256 mismatch",
        )

        write_manifest(manifest_path, pdf_path)
        manifest_path.write_text(
            manifest_path.read_text(encoding="utf-8")
            + manifest_path.read_text(encoding="utf-8").split("[[documents]]", 1)[1].join(
                ["[[documents]]", ""]
            ),
            encoding="utf-8",
        )
        assert_raises(lambda: runner.load_manifest(manifest_path), "duplicate")
    print("run_real_pdf_e2e preflight tests passed")


if __name__ == "__main__":
    main()
