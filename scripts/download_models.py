"""Download and verify pinned layout, table, OCR and formula ONNX artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from urllib.request import Request, urlopen

MODEL_REPOSITORY = "PaddlePaddle/PP-DocLayoutV3_onnx"
MODEL_REVISION = "46bbdf188bb0a772c08aed74882ce7e51a8f1ea6"
MODEL_LICENSE = "Apache-2.0"
MODEL_BASE_URL = (
    f"https://huggingface.co/{MODEL_REPOSITORY}/resolve/{MODEL_REVISION}"
)
MODEL_NAMES = (
    # Provision the default formula engine alongside the existing model catalog.
    "texo",
    "pp-formulanet-plus-s",
    "pp-formulanet-plus-m",
    "pp-formulanet-plus-l",
    "pp-doclayout-v3",
    "slanet-plus",
    "tatr-v1.1-all",
    "slanext-wired",
    "slanext-wireless",
    "rtdetr-table-cell-wired",
    "rtdetr-table-cell-wireless",
    "pp-ocrv6-medium-det",
    "pp-ocrv6-medium-rec",
    "pp-lcnet-textline-ori",
)


@dataclass(frozen=True, slots=True)
class Artifact:
    """One immutable remote artifact and its expected digest."""

    filename: str
    url: str
    sha256: str


TATR_REPOSITORY = "microsoft/table-transformer-structure-recognition-v1.1-all"
TATR_REVISION = "7587a7ef111d9dcbf8ac695f1376ab7014340a0c"
TATR_BASE = f"https://huggingface.co/{TATR_REPOSITORY}/resolve/{TATR_REVISION}"
TATR_SOURCES = tuple(Artifact(name, f"{TATR_BASE}/{name}", digest) for name, digest in [
    ("model.safetensors", "9df416575a3a36ebd0129342d4f597f14d6e5170268f3d52d28584ab4466a501"),
    ("config.json", "17a8a6edfb9e394263fa6ba9b82176ebccdfcc5d6cd29121ec91572c7d6be22c"),
])


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


@dataclass(frozen=True, slots=True)
class Model:
    """One supported model identity and its immutable artifacts."""

    name: str
    repository: str
    revision: str
    artifacts: tuple[Artifact, ...]
    # Texo carries AGPL provenance; existing models retain their Apache default.
    license: str = MODEL_LICENSE

    @classmethod
    def from_name(cls, name: str) -> Model:
        """Selects a pinned contract for either complete or targeted provisioning."""
        if name == "tatr-v1.1-all":
            # No hosted ONNX export exists: an empty URL marks the verified CPU export step.
            return cls(name, TATR_REPOSITORY, TATR_REVISION, (
                Artifact("inference.onnx", "", "ef7b679634f4693f4c0b6eecd1cd255d9cc5b844a3e5802b5e5456c8b9f1820e"),
                Artifact("inference.yml", f"{TATR_BASE}/preprocessor_config.json", "eead409bb80e36ae85b8377642c54550f0504f65688ba3a4967950cafe461df2"),
            ), license="MIT")
        if name == "texo":
            # Pin the corrected output-shape export and match docparse-formula-texo's enforced hashes.
            repository = "alephpi/FormulaNet"
            revision = "b2668efe5112082846fde4d446b9bfaab3989533"
            base = f"https://huggingface.co/{repository}/resolve/{revision}/onnx"
            artifacts = tuple(
                Artifact(filename, f"{base}/{filename}?download=true", digest)
                for filename, digest in [
                    ("encoder_model.onnx", "fbd69cf63cf833db1e2ef40013d859b560671c1253278441a01bde4516b624ae"),
                    ("decoder_model_merged.onnx", "61d4e9e60e3caa62af3f28a15a22bc13567eb4e618c87917d9597461e54c46be"),
                    ("tokenizer.json", "1240f9d178e1ad2a0076fe95ba62e332871c702accdd5ce3ae3ef33ffd6c3a1e"),
                ]
            )
            return cls(name, repository, revision, artifacts, license="AGPL-3.0")
        if name in ("pp-formulanet-plus-s", "pp-formulanet-plus-m", "pp-formulanet-plus-l"):
            digest = {
                "pp-formulanet-plus-s": "449d205c8fb2fe0a9b134a5e4a0f2421c2e7812fd902ea67dfda4e9ef4588978",
                "pp-formulanet-plus-m": "9e3539c2b4eeed28f2d35e342fd5bb0bdaa7f6034a475fc7e890c92780910618",
                "pp-formulanet-plus-l": "b4924d69c731365048de3d11a5d1829f3dfd8b98b4dbfd82437f934c2611934f",
            }[name]
            return cls(name, "GreatV/oar-ocr", "7feb044d74be09e3e2078a89cec0f0f8688e942b", (
                Artifact("inference.onnx", f"https://github.com/GreatV/oar-ocr/releases/download/v0.3.0/pp-formulanet_plus-{name[-1]}.onnx", digest),
                Artifact("tokenizer.json", "https://www.modelscope.cn/api/v1/models/greatv/oar-ocr/repo?Revision=master&FilePath=pp-formulanet-tokenizer.json", "2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2"),
            ))
        if name == "pp-doclayout-v3":
            return cls(name, MODEL_REPOSITORY, MODEL_REVISION, ARTIFACTS)
        # OCR model/config pairs carry their dictionaries and preprocessing contract together.
        ocr_models = {
            "slanext-wired": (
                "SLANeXt_wired_onnx", "04356de883011f433f83e5098793f3a501a9af6e",
                "0a6e063b56e35a434eb6669eb2342113c6bd76a6ce5acaa0331f370c9e00732f",
                "abbbd1b4dc6b1a2e9cd34c035514da53a1a6b1ec267292b0b8802025650a33bf",
            ),
            "slanext-wireless": (
                "SLANeXt_wireless_onnx", "9207aaed01d1bbb0743af384bac5b0bd35869ba3",
                "5c79ee87cce6712f8f640394decce72157bd1df13c9bccf86d071bd07a6e9f97",
                "58d1d7fdffd3e58cfec98571b817ea012f2107d644bd4f8e4607fae84f1923a6",
            ),
            "rtdetr-table-cell-wired": (
                "RT-DETR-L_wired_table_cell_det_onnx", "b2c0720b5fe6f1c0dd40f8a7993a3f28e04252f8",
                "bf5490020512a31f43813d90feadae9526a2c3474ffe807571f2c23594f5958f",
                "edf6d6180f2b9e3e666c744ee5ded38a72c6ef9056cd193250e3e55ba268acef",
            ),
            "rtdetr-table-cell-wireless": (
                "RT-DETR-L_wireless_table_cell_det_onnx", "94c021be206064f0136ef1383fbd4b68b168fa61",
                "47515940ec5c37156e09aa9acb20c4e7e22456cad6ce49473661f1762fb46a78",
                "f2d0f00ea42aacc162f72a35cf54330e392a7d669e9a1b43896d3bd77a512621",
            ),
            "pp-ocrv6-medium-det": (
                "PP-OCRv6_medium_det_onnx",
                "61323801669c338b7891481ec7bac61ce31b576a",
                "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1",
                "7298d5ead546584af2504d03355f881ac7a7bc0eb1e282d3e159277c1d0af871",
            ),
            "pp-ocrv6-medium-rec": (
                "PP-OCRv6_medium_rec_onnx",
                "50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
                "9c09abf0957f7968c7586464b7397b84ad2387a0497a351af40e9acc71b673ba",
                "991b700facf5b50a7de193468207d5f4255b538dde0d312ae3b7c7a9b6873129",
            ),
            "pp-lcnet-textline-ori": (
                "PP-LCNet_x1_0_textline_ori_onnx",
                "7fdcf3cf7061163eda7183b224aa334bd33068f7",
                "38aa97cd4be591e0ad304e659f07ba30d946f27a63315433f6659c69c8778345",
                "8d5120d0e1a30a9df7ed46aa9119da3796ed066777089d1c1d705f132d5e90f9",
            ),
        }
        if name in ocr_models:
            repo, revision, model_hash, config_hash = ocr_models[name]
            repository = f"PaddlePaddle/{repo}"
            base = f"https://huggingface.co/{repository}/resolve/{revision}"
            artifacts = tuple(
                Artifact(filename, f"{base}/{filename}?download=true", digest)
                for filename, digest in [
                    ("inference.onnx", model_hash), ("inference.yml", config_hash)
                ]
            )
            return cls(name, repository, revision, artifacts)
        if name != "slanet-plus":
            raise ValueError(f"unsupported model {name}")
        repository = "PaddlePaddle/SLANet_plus_onnx"
        revision = "7dbe640e127602bf506815e822c09758de73c482"
        base = f"https://huggingface.co/{repository}/resolve/{revision}"
        artifacts = tuple(Artifact(filename, f"{base}/{filename}?download=true", sha256) for filename, sha256 in [
            ("inference.onnx", "7790c0c13ce064782c9d22ebeb16b4da8216f83d3ba576da962c106ef58386da"),
            ("inference.yml", "8a6372d3269a6f112fe13a2da7952a84da6e112c10a3146cbb43de5bd01d19fa"),
        ])
        return cls(name, repository, revision, artifacts)


class ModelDownloadError(RuntimeError):
    """Signals a missing, malformed, or hash-mismatched model installation."""


def sha256_file(path: Path) -> str:
    """Streams one file through SHA-256 without loading it into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def verify_installation(output: Path, model: Model | None = None) -> dict:
    """Verifies model-specific manifest provenance and all installed artifact digests."""
    model = model or Model.from_name("pp-doclayout-v3")
    manifest_path = output / "model-manifest.json"
    if not manifest_path.is_file():
        raise ModelDownloadError(f"model manifest not found: {manifest_path}")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ModelDownloadError(
            f"failed to read model manifest {manifest_path}: {error}"
        ) from error
    if not isinstance(manifest, dict):
        raise ModelDownloadError(f"model manifest must be an object: {manifest_path}")

    expected_files = {artifact.filename: artifact.sha256 for artifact in model.artifacts}
    expected_identity = {
        "repository": model.repository,
        "revision": model.revision,
        "license": model.license,
        "files": expected_files,
    }
    for field, expected in expected_identity.items():
        actual = manifest.get(field)
        if actual != expected:
            raise ModelDownloadError(
                f"model manifest mismatch at {field}: expected {expected!r}, got {actual!r}"
            )

    for artifact in model.artifacts:
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


def export_tatr(destination: Path) -> None:
    """Download verified checkpoint inputs and export in uv's isolated pinned CPU environment."""
    with tempfile.TemporaryDirectory(prefix="tatr-source-", dir=destination.parent) as temporary:
        directory = Path(temporary)
        for artifact in TATR_SOURCES:
            path = directory / artifact.filename
            download_artifact(artifact, path)
            if sha256_file(path) != artifact.sha256:
                raise ModelDownloadError(f"TATR source hash mismatch: {artifact.filename}")
        exporter = Path(__file__).with_name("export_tatr.py")
        try:
            subprocess.run(["uv", "run", "--script", str(exporter),
                            str(directory), str(destination)], check=True)
        except (OSError, subprocess.CalledProcessError) as error:
            raise ModelDownloadError(f"TATR CPU export failed (uv is required): {error}") from error


def write_manifest(directory: Path, model: Model | None = None) -> None:
    """Writes model-specific provenance plus an informational UTC generation time."""
    model = model or Model.from_name("pp-doclayout-v3")
    manifest = {
        "repository": model.repository,
        "revision": model.revision,
        "license": model.license,
        "generated_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "files": {artifact.filename: artifact.sha256 for artifact in model.artifacts},
    }
    payload = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    (directory / "model-manifest.json").write_text(payload, encoding="utf-8")


def install_model(output: Path, force: bool, model: Model | None = None) -> bool:
    """Repairs missing or invalid files while preserving valid local files and returns whether the install changed."""
    model = model or Model.from_name("pp-doclayout-v3")
    if not force:
        try:
            verify_installation(output, model)
            return False
        except ModelDownloadError:
            pass

    output_parent = output.parent
    output_parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=".docparse-model-", dir=output_parent
    ) as temporary:
        temporary_path = Path(temporary)
        downloaded = []
        for artifact in model.artifacts:
            existing = output / artifact.filename
            # A missing manifest or sibling artifact must not cause intact weights to be downloaded or rewritten.
            if not force and existing.is_file() and sha256_file(existing) == artifact.sha256:
                continue
            destination = temporary_path / artifact.filename
            if model.name == "tatr-v1.1-all" and artifact.filename == "inference.onnx":
                export_tatr(destination)
            else:
                download_artifact(artifact, destination)
            actual_hash = sha256_file(destination)
            if actual_hash != artifact.sha256:
                raise ModelDownloadError(
                    f"downloaded hash mismatch for {artifact.filename}: "
                    f"expected {artifact.sha256}, got {actual_hash}"
                )
            downloaded.append(artifact)
        write_manifest(temporary_path, model)

        output.mkdir(parents=True, exist_ok=True)
        # Every pending download is verified before replacing files; publish the manifest last.
        for artifact in downloaded:
            os.replace(temporary_path / artifact.filename, output / artifact.filename)
        os.replace(
            temporary_path / "model-manifest.json",
            output / "model-manifest.json",
        )
    verify_installation(output, model)
    return True


def parse_args() -> argparse.Namespace:
    """Parses command-line arguments for install or verification mode."""
    parser = argparse.ArgumentParser(
        description="Download pinned layout, table, OCR and formula ONNX artifacts."
    )
    parser.add_argument("--model", choices=("all", *MODEL_NAMES), default="all", help="model to provision (default: all registered models)")
    location = parser.add_mutually_exclusive_group()
    location.add_argument(
        "--models-dir",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "models",
        help="model root directory (default: the repository's models directory)",
    )
    location.add_argument(
        "--output",
        type=Path,
        default=None,
        help="exact installation directory for one explicitly selected model",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--force", action="store_true", help="download even if valid")
    mode.add_argument(
        "--verify-only",
        action="store_true",
        help="verify existing files without downloading",
    )
    arguments = parser.parse_args()
    if arguments.output is not None and arguments.model == "all":
        parser.error("--output requires a single --model; use --models-dir for all models")
    return arguments


def main() -> int:
    """Runs model installation or verification and returns a process exit code."""
    arguments = parse_args()
    names = MODEL_NAMES if arguments.model == "all" else (arguments.model,)
    failed = False
    for name in names:
        model = Model.from_name(name)
        output = arguments.output or arguments.models_dir / name
        try:
            if arguments.verify_only:
                verify_installation(output, model)
                print(f"verified {name} at {output}")
            else:
                changed = install_model(output, force=arguments.force, model=model)
                action = "installed" if changed else "skipped (already verified)"
                print(f"{action} {name} at {output}")
        except (ModelDownloadError, OSError) as error:
            # Other missing models can still be provisioned, but the command must report partial failure.
            print(f"error: {name}: {error}", file=sys.stderr)
            failed = True
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
