# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Download and verify the fixed PP-DocLayoutV3 ONNX artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from urllib.request import Request, urlopen

MODEL_REPOSITORY = "PaddlePaddle/PP-DocLayoutV3_onnx"
MODEL_REVISION = "46bbdf188bb0a772c08aed74882ce7e51a8f1ea6"
MODEL_LICENSE = "Apache-2.0"
MODEL_BASE_URL = (
    f"https://huggingface.co/{MODEL_REPOSITORY}/resolve/{MODEL_REVISION}"
)


@dataclass(frozen=True, slots=True)
class Artifact:
    """One immutable remote artifact and its expected digest."""

    filename: str
    url: str
    sha256: str


ARTIFACTS = (
    Artifact(
        filename="inference.onnx",
        url=f"{MODEL_BASE_URL}/inference.onnx?download=true",
        sha256="45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba",
    ),
    Artifact(
        filename="inference.yml",
        url=f"{MODEL_BASE_URL}/inference.yml?download=true",
        sha256="506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc",
    ),
)


class ModelDownloadError(RuntimeError):
    """Signals a missing, malformed, or hash-mismatched model installation."""


def sha256_file(path: Path) -> str:
    """Streams one file through SHA-256 without loading it into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def verify_installation(output: Path) -> dict:
    """Verifies manifest provenance and all installed artifact digests."""
    manifest_path = output / "model-manifest.json"
    if not manifest_path.is_file():
        raise ModelDownloadError(f"model manifest not found: {manifest_path}")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ModelDownloadError(
            f"failed to read model manifest {manifest_path}: {error}"
        ) from error

    expected_files = {artifact.filename: artifact.sha256 for artifact in ARTIFACTS}
    expected_identity = {
        "repository": MODEL_REPOSITORY,
        "revision": MODEL_REVISION,
        "license": MODEL_LICENSE,
        "files": expected_files,
    }
    for field, expected in expected_identity.items():
        actual = manifest.get(field)
        if actual != expected:
            raise ModelDownloadError(
                f"model manifest mismatch at {field}: expected {expected!r}, got {actual!r}"
            )

    for artifact in ARTIFACTS:
        path = output / artifact.filename
        if not path.is_file():
            raise ModelDownloadError(f"model artifact not found: {path}")
        actual_hash = sha256_file(path)
        if actual_hash != artifact.sha256:
            raise ModelDownloadError(
                f"model artifact hash mismatch for {path}: "
                f"expected {artifact.sha256}, got {actual_hash}"
            )
    return manifest


def download_artifact(artifact: Artifact, destination: Path) -> None:
    """Downloads one artifact to a temporary destination."""
    request = Request(artifact.url, headers={"User-Agent": "docparse-model-fetch/1"})
    try:
        with urlopen(request, timeout=120) as response, destination.open("wb") as sink:
            while chunk := response.read(1024 * 1024):
                sink.write(chunk)
    except OSError as error:
        raise ModelDownloadError(
            f"failed to download {artifact.filename}: {error}"
        ) from error


def write_manifest(directory: Path) -> None:
    """Writes deterministic provenance plus an informational UTC generation time."""
    manifest = {
        "repository": MODEL_REPOSITORY,
        "revision": MODEL_REVISION,
        "license": MODEL_LICENSE,
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "files": {artifact.filename: artifact.sha256 for artifact in ARTIFACTS},
    }
    payload = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    (directory / "model-manifest.json").write_text(payload, encoding="utf-8")


def install_model(output: Path, force: bool) -> bool:
    """Installs verified artifacts atomically and returns whether files changed."""
    if not force:
        try:
            verify_installation(output)
            return False
        except ModelDownloadError:
            pass

    output_parent = output.parent
    output_parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=".docparse-model-", dir=output_parent
    ) as temporary:
        temporary_path = Path(temporary)
        for artifact in ARTIFACTS:
            destination = temporary_path / artifact.filename
            download_artifact(artifact, destination)
            actual_hash = sha256_file(destination)
            if actual_hash != artifact.sha256:
                raise ModelDownloadError(
                    f"downloaded hash mismatch for {artifact.filename}: "
                    f"expected {artifact.sha256}, got {actual_hash}"
                )
        write_manifest(temporary_path)
        verify_installation(temporary_path)

        output.mkdir(parents=True, exist_ok=True)
        for artifact in ARTIFACTS:
            os.replace(temporary_path / artifact.filename, output / artifact.filename)
        os.replace(
            temporary_path / "model-manifest.json",
            output / "model-manifest.json",
        )
    return True


def parse_args() -> argparse.Namespace:
    """Parses command-line arguments for install or verification mode."""
    parser = argparse.ArgumentParser(
        description="Download the fixed PP-DocLayoutV3 ONNX artifacts."
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("models/pp-doclayout-v3"),
        help="installation directory",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--force", action="store_true", help="download even if valid")
    mode.add_argument(
        "--verify-only",
        action="store_true",
        help="verify existing files without downloading",
    )
    return parser.parse_args()


def main() -> int:
    """Runs model installation or verification and returns a process exit code."""
    arguments = parse_args()
    try:
        if arguments.verify_only:
            verify_installation(arguments.output)
            print(f"verified PP-DocLayoutV3 at {arguments.output}")
        else:
            changed = install_model(arguments.output, force=arguments.force)
            action = "installed" if changed else "already verified"
            print(f"{action} PP-DocLayoutV3 at {arguments.output}")
    except ModelDownloadError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
