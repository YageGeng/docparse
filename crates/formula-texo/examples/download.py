"""Download the three pinned Texo assets into a caller-selected directory."""
import hashlib
from pathlib import Path
import sys
import urllib.request

# Pin the corrected output-shape export, matching native and browser artifact verification.
REVISION = "b2668efe5112082846fde4d446b9bfaab3989533"
FILES = {
    "encoder_model.onnx": "fbd69cf63cf833db1e2ef40013d859b560671c1253278441a01bde4516b624ae",
    "decoder_model_merged.onnx": "61d4e9e60e3caa62af3f28a15a22bc13567eb4e618c87917d9597461e54c46be",
    "tokenizer.json": "1240f9d178e1ad2a0076fe95ba62e332871c702accdd5ce3ae3ef33ffd6c3a1e",
}


def main():
    """Verify existing files and atomically install newly downloaded assets."""
    directory = Path(sys.argv[1])
    directory.mkdir(parents=True, exist_ok=True)
    for name, digest in FILES.items():
        destination = directory / name
        if destination.exists() and hashlib.sha256(destination.read_bytes()).hexdigest() == digest:
            print(f"verified {destination}")
            continue
        temporary = destination.with_suffix(destination.suffix + ".tmp")
        try:
            urllib.request.urlretrieve(f"https://huggingface.co/alephpi/FormulaNet/resolve/{REVISION}/onnx/{name}", temporary)
            if hashlib.sha256(temporary.read_bytes()).hexdigest() != digest:
                raise ValueError(f"{name}: SHA-256 mismatch")
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)
        print(f"downloaded {destination}")


if __name__ == "__main__":
    main()
