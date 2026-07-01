# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "huggingface-hub>=0.33",
# ]
# ///

"""Download PP-DocLayoutV3 files for the browser WASM demo."""

from __future__ import annotations

import argparse
import shutil
from pathlib import Path

from huggingface_hub import hf_hub_download

DEFAULT_REPO_ID = "PaddlePaddle/PP-DocLayoutV3_safetensors"
DEFAULT_FILES = ("config.json", "preprocessor_config.json", "model.safetensors")
DEFAULT_OUTPUT_DIR = (
    Path(__file__).resolve().parents[1] / "wasm" / "models" / "pp-doclayout-v3"
)


def parse_args() -> argparse.Namespace:
    """Parse model download options."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-id", default=DEFAULT_REPO_ID)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    return parser.parse_args()


def download_file(repo_id: str, filename: str, output_dir: Path) -> Path:
    """Download one Hugging Face file and copy it into the served WASM tree."""
    cached_path = Path(hf_hub_download(repo_id=repo_id, filename=filename))
    output_path = output_dir / filename
    output_path.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cached_path, output_path)
    return output_path


def main() -> None:
    """Download every model file required by the browser runtime."""
    args = parse_args()
    for filename in DEFAULT_FILES:
        output_path = download_file(args.repo_id, filename, args.output_dir)
        print(output_path)


if __name__ == "__main__":
    main()
