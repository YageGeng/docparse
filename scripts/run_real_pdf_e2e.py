# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "psutil==7.0.0",
#   "pypdf==6.0.0",
# ]
# ///
"""Run strict production-model E2E tests against an exact local PDF corpus."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path

import psutil
from pypdf import PdfReader


@dataclass(frozen=True, slots=True)
class CorpusDocument:
    """One validated manifest row for a real local PDF."""

    logical_id: str
    basename: str
    sha256: str
    size_bytes: int
    page_count: int


class E2ePreflightError(RuntimeError):
    """Signals an unsafe, incomplete, or identity-mismatched E2E input."""


def sha256_file(path: Path) -> str:
    """Stream one file through SHA-256 without retaining its bytes."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def load_manifest(path: Path) -> list[CorpusDocument]:
    """Load and validate the tracked schema-one corpus manifest."""
    try:
        payload = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise E2ePreflightError(f"failed to load corpus manifest {path}: {error}") from error
    if payload.get("schema_version") != 1:
        raise E2ePreflightError(
            f"unsupported corpus schema_version {payload.get('schema_version')!r}"
        )
    rows = payload.get("documents")
    if not isinstance(rows, list) or not rows:
        raise E2ePreflightError("corpus manifest documents must be a non-empty list")
    documents: list[CorpusDocument] = []
    logical_ids: set[str] = set()
    basenames: set[str] = set()
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise E2ePreflightError(f"documents[{index}] must be a table")
        try:
            document = CorpusDocument(
                logical_id=row["logical_id"],
                basename=row["basename"],
                sha256=row["sha256"],
                size_bytes=row["size_bytes"],
                page_count=row["page_count"],
            )
        except (KeyError, TypeError) as error:
            raise E2ePreflightError(
                f"documents[{index}] has missing or invalid fields: {error}"
            ) from error
        if not document.logical_id or document.logical_id in logical_ids:
            raise E2ePreflightError(
                f"duplicate or empty logical_id {document.logical_id!r}"
            )
        basename_path = Path(document.basename)
        if (
            not document.basename
            or basename_path.name != document.basename
            or len(basename_path.parts) != 1
            or basename_path.suffix.lower() != ".pdf"
        ):
            raise E2ePreflightError(
                f"invalid top-level PDF basename {document.basename!r}"
            )
        if document.basename in basenames:
            raise E2ePreflightError(f"duplicate basename {document.basename!r}")
        if len(document.sha256) != 64 or any(
            character not in "0123456789abcdef" for character in document.sha256
        ):
            raise E2ePreflightError(
                f"invalid lowercase SHA-256 for {document.logical_id}"
            )
        if document.size_bytes <= 0 or document.page_count <= 0:
            raise E2ePreflightError(
                f"non-positive size/page count for {document.logical_id}"
            )
        logical_ids.add(document.logical_id)
        basenames.add(document.basename)
        documents.append(document)
    return sorted(documents, key=lambda document: document.logical_id)


def discover_pdfs(pdf_dir: Path) -> dict[str, Path]:
    """Discover every top-level regular PDF using an ASCII-insensitive suffix."""
    try:
        entries = list(os.scandir(pdf_dir))
    except OSError as error:
        raise E2ePreflightError(f"failed to scan PDF directory {pdf_dir}: {error}") from error
    discovered: dict[str, Path] = {}
    for entry in entries:
        if entry.is_file(follow_symlinks=False) and entry.name.lower().endswith(".pdf"):
            discovered[entry.name] = Path(entry.path)
    return dict(sorted(discovered.items()))


def verify_pdf_corpus(
    documents: list[CorpusDocument], pdf_dir: Path
) -> list[tuple[CorpusDocument, Path]]:
    """Require exact membership and verify size, hash, and decoded page count."""
    discovered = discover_pdfs(pdf_dir)
    expected_names = {document.basename for document in documents}
    actual_names = set(discovered)
    if expected_names != actual_names:
        missing = sorted(expected_names - actual_names)
        extra = sorted(actual_names - expected_names)
        raise E2ePreflightError(
            f"PDF corpus set mismatch: missing={missing}, extra={extra}"
        )
    verified: list[tuple[CorpusDocument, Path]] = []
    for document in documents:
        path = discovered[document.basename]
        actual_size = path.stat().st_size
        if actual_size != document.size_bytes:
            raise E2ePreflightError(
                f"size mismatch for {document.basename}: "
                f"expected {document.size_bytes}, got {actual_size}"
            )
        actual_hash = sha256_file(path)
        if actual_hash != document.sha256:
            raise E2ePreflightError(
                f"SHA-256 mismatch for {document.basename}: "
                f"expected {document.sha256}, got {actual_hash}"
            )
        try:
            actual_pages = len(PdfReader(path, strict=True).pages)
        except Exception as error:
            raise E2ePreflightError(
                f"failed to decode {document.basename}: {error}"
            ) from error
        if actual_pages != document.page_count:
            raise E2ePreflightError(
                f"page count mismatch for {document.basename}: "
                f"expected {document.page_count}, got {actual_pages}"
            )
        verified.append((document, path))
    return verified


def verify_model(workspace: Path, model_dir: Path) -> None:
    """Invoke the fixed downloader's offline verification mode without a shell."""
    subprocess.run(
        [
            sys.executable,
            str(workspace / "scripts/download_models.py"),
            "--output",
            str(model_dir),
            "--verify-only",
        ],
        cwd=workspace,
        check=True,
    )


def write_config(
    path: Path,
    model_dir: Path,
    page_concurrency: int,
    execution_provider: str,
) -> None:
    """Write a complete temporary config whose only variable policy is concurrency."""
    queue_capacity = max(1, min(2, page_concurrency))
    # This 124 MB model reserves roughly 4.2 GB per CUDA session on an 8 GB GPU.
    session_pool_size = 1 if execution_provider == "cuda" else page_concurrency
    payload = f'''[layout]
model_path = "{(model_dir / 'inference.onnx').as_posix()}"
model_config_path = "{(model_dir / 'inference.yml').as_posix()}"
model_manifest_path = "{(model_dir / 'model-manifest.json').as_posix()}"
score_threshold = 0.5
execution_provider = "{execution_provider}"
session_pool_size = {session_pool_size}

[runtime]
page_concurrency = {page_concurrency}
render_queue_capacity = {queue_capacity}
blocking_task_limit = {page_concurrency}
continue_on_page_error = true

[render]
dpi = 144
max_long_edge_pixels = 2400

[fusion]
minimum_line_coverage = 0.30
center_minimum_line_coverage = 0.10
assignment_coverage_weight = 0.55
assignment_center_weight = 0.20
assignment_baseline_weight = 0.10
assignment_confidence_weight = 0.10
assignment_specificity_weight = 0.05
paragraph_gap_multiplier = 1.5
indent_tolerance_points = 6.0
font_size_tolerance_points = 0.5
estimated_font_size_tolerance_points = 1.5

[ocr]
policy = "disabled"

[output]
formula_placeholder = "[formula]"
include_evidence = true
include_diagnostics = true
'''
    path.write_text(payload, encoding="utf-8")


def cargo_build_command(execution_provider: str, cargo_profile: str) -> list[str]:
    """Return the feature-aware command that builds but does not run the harness."""
    command = [
        "cargo",
        "test",
        "-p",
        "docparse-core",
    ]
    if execution_provider == "cuda":
        command.extend(["--features", "layout-cuda"])
    command.extend([
        "--test",
        "real_pdfs",
        "--no-run",
        "--message-format=json",
    ])
    if cargo_profile == "release":
        command.append("--release")
    elif cargo_profile != "dev":
        raise E2ePreflightError(f"unsupported Cargo profile {cargo_profile!r}")
    return command


def parse_harness_executable(messages: list[str]) -> Path:
    """Extract the unique real-PDF test executable from Cargo JSON messages."""
    executables: set[Path] = set()
    for line in messages:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = message.get("target")
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and isinstance(target, dict)
            and target.get("name") == "real_pdfs"
            and isinstance(executable, str)
        ):
            executables.add(Path(executable))
    if len(executables) != 1:
        raise E2ePreflightError(
            f"expected one real_pdfs executable from Cargo, got {sorted(executables)}"
        )
    return next(iter(executables))


def build_harness(
    workspace: Path,
    environment: dict[str, str],
    execution_provider: str,
    cargo_profile: str,
) -> Path:
    """Build the harness outside the measured interval and return its executable path."""
    process = subprocess.Popen(
        cargo_build_command(execution_provider, cargo_profile),
        cwd=workspace,
        env=environment,
        stdout=subprocess.PIPE,
        text=True,
    )
    if process.stdout is None:
        process.kill()
        raise E2ePreflightError("Cargo build stdout pipe was not created")
    messages = list(process.stdout)
    return_code = process.wait()
    if return_code != 0:
        raise E2ePreflightError(
            f"Rust E2E harness build exited with status {return_code}"
        )
    return parse_harness_executable(messages)


def harness_command(executable: Path) -> list[str]:
    """Return the direct Rust test-binary command used for measured execution."""
    return [str(executable), "--ignored", "--nocapture"]


def harness_environment(
    environment: dict[str, str], executable: Path
) -> dict[str, str]:
    """Expose Cargo's adjacent copied runtime libraries to the direct test process."""
    prepared = environment.copy()
    if os.name == "nt":
        variable = "PATH"
    elif sys.platform == "darwin":
        variable = "DYLD_LIBRARY_PATH"
    else:
        variable = "LD_LIBRARY_PATH"
    existing = prepared.get(variable)
    values = [str(executable.parent)]
    if existing:
        values.append(existing)
    prepared[variable] = os.pathsep.join(values)
    return prepared


def process_tree_rss(process: psutil.Process) -> int:
    """Return best-effort resident bytes for one process and all descendants."""
    processes = [process]
    try:
        processes.extend(process.children(recursive=True))
    except (psutil.Error, OSError):
        pass
    total = 0
    for child in processes:
        try:
            total += child.memory_info().rss
        except (psutil.Error, OSError):
            continue
    return total


def run_harness(
    workspace: Path,
    environment: dict[str, str],
    executable: Path,
) -> tuple[float, int]:
    """Run the prebuilt harness without a shell and sample its process-tree RSS."""
    started = time.monotonic()
    process = subprocess.Popen(
        harness_command(executable),
        cwd=workspace,
        env=harness_environment(environment, executable),
    )
    monitor = psutil.Process(process.pid)
    peak_rss = 0
    try:
        while process.poll() is None:
            peak_rss = max(peak_rss, process_tree_rss(monitor))
            time.sleep(0.1)
    except KeyboardInterrupt:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
        raise
    return_code = process.wait()
    elapsed = time.monotonic() - started
    if return_code != 0:
        raise E2ePreflightError(f"Rust E2E harness exited with status {return_code}")
    return elapsed, peak_rss


def update_summary(
    output_dir: Path,
    elapsed_seconds: float,
    peak_rss_bytes: int,
    page_concurrency: int,
    write_overlays: bool,
    execution_provider: str,
    cargo_profile: str,
) -> None:
    """Add explicitly volatile resource and host metrics to the non-canonical summary."""
    path = output_dir / "summary.json"
    try:
        summary = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise E2ePreflightError(f"failed to read harness summary {path}: {error}") from error
    summary["runtime"] = {
        "elapsed_seconds": elapsed_seconds,
        "peak_rss_bytes": peak_rss_bytes,
        "page_concurrency": page_concurrency,
        "execution_provider": execution_provider,
        "cargo_profile": cargo_profile,
        "platform": platform.platform(),
        "overlays_requested": write_overlays,
    }
    path.write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def parse_args() -> argparse.Namespace:
    """Parse strict corpus, model, concurrency, and output options."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pdf-dir", type=Path, required=True)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument(
        "--manifest", type=Path, default=Path("tests/e2e-corpus.toml")
    )
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--run-id", default="run")
    parser.add_argument("--page-concurrency", type=int, default=4)
    parser.add_argument(
        "--execution-provider", choices=["cpu", "cuda"], default="cpu"
    )
    parser.add_argument(
        "--cargo-profile", choices=["dev", "release"], default="release"
    )
    parser.add_argument("--only")
    parser.add_argument("--write-overlays", action="store_true")
    return parser.parse_args()


def main() -> int:
    """Preflight every PDF and model artifact, then run the production Rust harness."""
    arguments = parse_args()
    workspace = Path(__file__).resolve().parent.parent
    pdf_dir = arguments.pdf_dir.expanduser().resolve()
    model_dir = (workspace / arguments.model_dir).resolve() if not arguments.model_dir.is_absolute() else arguments.model_dir.resolve()
    manifest_path = (workspace / arguments.manifest).resolve() if not arguments.manifest.is_absolute() else arguments.manifest.resolve()
    if arguments.page_concurrency <= 0:
        print("error: --page-concurrency must be greater than zero", file=sys.stderr)
        return 2
    try:
        documents = load_manifest(manifest_path)
        verified = verify_pdf_corpus(documents, pdf_dir)
        if arguments.only and not any(
            document.logical_id == arguments.only for document, _ in verified
        ):
            raise E2ePreflightError(f"unknown --only logical ID {arguments.only!r}")
        verify_model(workspace, model_dir)
        environment = os.environ.copy()
        # Cargo owns the target tree and may garbage-collect non-artifact directories during a
        # rebuild, so compile before publishing this run's target/docparse-e2e output directory.
        executable = build_harness(
            workspace,
            environment,
            arguments.execution_provider,
            arguments.cargo_profile,
        )
        output_base = (
            arguments.output_dir.expanduser().resolve()
            if arguments.output_dir
            else workspace / "target/docparse-e2e"
        )
        output_dir = output_base / arguments.run_id
        output_dir.mkdir(parents=True, exist_ok=True)
        config_path = output_dir / ".docparse-e2e-config.toml"
        write_config(
            config_path,
            model_dir,
            arguments.page_concurrency,
            arguments.execution_provider,
        )
        environment.update(
            {
                "DOCPARSE_E2E_PDF_DIR": str(pdf_dir),
                "DOCPARSE_E2E_MANIFEST": str(manifest_path),
                "DOCPARSE_E2E_OUTPUT_DIR": str(output_dir),
                "DOCPARSE_E2E_CONFIG": str(config_path),
            }
        )
        if arguments.only:
            environment["DOCPARSE_E2E_ONLY"] = arguments.only
        if arguments.write_overlays:
            environment["DOCPARSE_E2E_WRITE_OVERLAYS"] = "1"
        elapsed, peak_rss = run_harness(
            workspace, environment, executable
        )
        update_summary(
            output_dir,
            elapsed,
            peak_rss,
            arguments.page_concurrency,
            arguments.write_overlays,
            arguments.execution_provider,
            arguments.cargo_profile,
        )
        print(
            f"E2E passed: {len(verified)} verified PDFs, "
            f"{sum(document.page_count for document, _ in verified)} verified pages; "
            f"results at {output_dir}"
        )
    except (E2ePreflightError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
