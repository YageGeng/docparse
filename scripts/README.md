# Python tools

All retained Python tools share the root `pyproject.toml`, `uv.lock` and Python 3.12
selection in `.python-version`. Run them through uv; standalone script metadata and
per-script lockfiles are no longer used. Python subprocesses reuse uv's interpreter.

## Models

```sh
rtk uv run --locked scripts/download_models.py
```

This checks all five pinned models under the repository's `models/` directory:
PP-DocLayoutV3, SLANet_plus, PP-OCRv6 detection, PP-OCRv6 recognition, and text-line
orientation. Valid files are left untouched, missing or corrupt artifacts are
downloaded to temporary files and SHA-256 verified, and the manifest is published
last. A missing manifest is rebuilt from verified local artifacts without fetching
the weights again. Any failed model makes the command fail, while other models are
still attempted. Existing models can be checked without network access:

```sh
rtk uv run --locked scripts/download_models.py --verify-only
rtk uv run --locked scripts/download_models.py --model slanet-plus
rtk uv run --locked scripts/download_models.py --models-dir /srv/docparse/models
```

`--force` downloads every selected artifact again. `--output` is retained only for
an explicitly selected single model; use `--models-dir` for an alternative root.

## Retained tools

| Tool | Purpose | Dependency group |
| --- | --- | --- |
| `download_models.py` | Provision and verify pinned artifacts | Standard library |
| `check_wasm_compat.py` | Enforce Rust platform boundaries; used by pre-commit | Standard library |
| `compare_e2e_runs.py` | Compare canonical native E2E hashes and document identities | Standard library |
| `run_real_pdf_e2e.py` | Validate the exact corpus and run production native E2E | `dev` |
| `reference_layout.py` | Generate an independent PaddleX/ONNX layout oracle | `reference` |

The native E2E runner verifies layout and the sibling `slanet-plus` installation
before starting Rust, and writes absolute artifact paths into its temporary config.
Its canonical fingerprint excludes model-file locations so different run directories
and hosts do not create false comparison failures.

`dev` contains PDF fixture/E2E dependencies and pre-commit. `reference` retains the
existing pinned NumPy, ONNX Runtime, OpenCV and PaddleOCR versions. Neither group is
installed by default, so model provisioning and compatibility checks need no
third-party Python packages. [uv dependency groups](https://docs.astral.sh/uv/concepts/projects/dependencies/)
allow these tools to share one lockfile while keeping heavy oracle dependencies optional.

```sh
rtk uv sync --locked                         # Minimal environment
rtk uv sync --locked --group dev             # Fixture and E2E tools
rtk uv run --locked --group dev scripts/run_real_pdf_e2e.py --help
rtk uv run --locked --group reference scripts/reference_layout.py --help
rtk uv run --locked --group dev pre-commit run --all-files
```

Regression PDF/OCR generators remain beside their fixtures under `crates/*/tests/`;
run them with `uv run --locked --group dev <generator.py>`. The layout RGB generator
now lives at `crates/layout/tests/fixtures/model/generate.py` and needs only the standard
library. `packages/web/tests/serve.py` remains the real browser-acceptance server;
the JavaScript E2E launcher starts it through uv.

The old static visual-review generator and its test were removed in favor of the
production WebUI. The old oblique-overlay audit used retired result fields and was
removed. The unused legal-file synchronization script, whose crate list was stale,
was also removed. Model oracles and fixture generators are retained for reproducibility.

## Checks

```sh
rtk uv run --locked crates/layout/tests/python/download_models_test.py
rtk uv run --locked crates/core/tests/python/wasm_compat_test.py
rtk uv run --locked crates/core/tests/python/compare_e2e_runs_test.py
rtk uv run --locked --group dev crates/core/tests/python/run_real_pdf_e2e_test.py
rtk uv run --locked --group reference crates/layout/tests/python/reference_layout_test.py
rtk uv run --locked crates/core/tests/python/wasm_types.py
```

The last two checks use the real pinned model or compile both Rust targets. Ordinary
fixture-based Rust tests do not regenerate PDFs, download models, or run the oracle.
