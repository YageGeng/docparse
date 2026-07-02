# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "huggingface-hub>=0.33",
# ]
# ///

"""Download PP-StructureV3 layout ONNX files into the local models directory."""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

from huggingface_hub import hf_hub_download
from huggingface_hub.errors import RepositoryNotFoundError

DEFAULT_REPO_ID = "PaddlePaddle/PP-DocLayoutV3_onnx"
DEFAULT_FILENAMES = ["inference.onnx", "inference.yml"]
DEFAULT_OUTPUT_DIR = (
    Path(__file__).resolve().parents[1] / "models" / "pp-structure-v3-onnx"
)


def parse_args() -> argparse.Namespace:
    """Parse model download options."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-id", default=DEFAULT_REPO_ID)
    parser.add_argument("--filename", action="append", dest="filenames")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    return parser.parse_args()


def download_file(repo_id: str, filename: str, output_dir: Path) -> Path:
    """Download one Hugging Face file and copy it into the local model tree."""
    try:
        cached_path = Path(hf_hub_download(repo_id=repo_id, filename=filename))
    except RepositoryNotFoundError as error:
        raise SystemExit(
            f"repository not found or not accessible: {repo_id}\n"
            "The default public repo is PaddlePaddle/PP-DocLayoutV3_onnx."
        ) from error

    output_path = output_dir / filename
    output_path.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cached_path, output_path)
    return output_path


def main() -> None:
    """Download every requested model file."""
    args = parse_args()
    filenames = args.filenames or DEFAULT_FILENAMES
    for filename in filenames:
        output_path = download_file(args.repo_id, filename, args.output_dir)
        print(output_path, file=sys.stdout)


if __name__ == "__main__":
    main()
