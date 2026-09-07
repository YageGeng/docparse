"""Compile the same non-Send engine on native and browser targets without changing production code."""
import subprocess
import tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[4]
with tempfile.TemporaryDirectory(prefix="docparse-types-") as temporary:
    directory = Path(temporary)
    (directory / "src").mkdir()
    (directory / "src/lib.rs").write_bytes((root / "crates/core/tests/fixtures/wasm_types.rs").read_bytes())
    (directory / "Cargo.toml").write_text(f'''[workspace]
[package]
name = "docparse-type-contract"
version = "0.0.0"
edition = "2024"
[features]
wasm = ["docparse-layout/wasm"]
[dependencies]
docparse-layout = {{ path = "{root / 'crates/layout'}" }}
[patch.crates-io]
ort-web = {{ path = "{root / 'vendor/ort-web'}" }}
''')
    base = ["rtk", "proxy", "cargo", "check", "--manifest-path", str(directory / "Cargo.toml"), "--target-dir", str(root / "target/type-contract")]
    for name, arguments, succeeds in [
        ("native", [], False),
        ("native with wasm feature", ["--features", "wasm"], False),
        ("browser", ["--features", "wasm", "--target", "wasm32-unknown-unknown"], True),
    ]:
        result = subprocess.run(base + arguments, capture_output=True, text=True)
        if (result.returncode == 0) != succeeds or (not succeeds and ("Rc" not in result.stderr or "Send" not in result.stderr)):
            raise SystemExit(f"{name} type contract failed unexpectedly\n{result.stdout}\n{result.stderr}")
        print(f"PASS: {name} {'accepts' if succeeds else 'rejects'} the non-Send engine")
