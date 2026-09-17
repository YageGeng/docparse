"""Download the three pinned Texo assets into a caller-selected directory."""
import hashlib
from pathlib import Path
import sys
import urllib.request

REVISION = "63e04c86fc96c2324811114351eeea8118bf6b28"
FILES = {
    "encoder_model.onnx": "95cccef463e5ed3623282f1541c0011a00b8a5d0828ea2cd57d6953ad4310b5b",
    "decoder_model_merged.onnx": "10be29b751f6de5f9900c3658551020dc865257eb2c3034bc4c1e016e4d0e35d",
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
