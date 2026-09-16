# Plus-M formula evaluation — 2026-09-16

Plus-M is selectable by setting its artifact paths directly in `docparse.toml`. Plus-S remains the default.
Pix2Text was not installed or integrated, following the updated request.

## Result

The same long inline formula from page 4 of `2604.18583v1.pdf` was recognized by both
models. Plus-M recovered the barred mu and its plain `uv` superscript more faithfully
than Plus-S, but still added an incorrect check accent over `gs` and a circle over
alpha. Neither result is fully correct; these observations are not an aggregate
accuracy score or proof that every formula improves.

The production parser also replayed pages 4 and 5 with Plus-M: all 41 and 20 formula
requests respectively completed without inference failures. The corrected page-4
motion-descriptor overbar remained present without duplicate prose; the page-5
caption formula stayed in its right-column caption and out of the left paragraph.
These targeted regressions do not certify every generated formula.

## Input contract and timing

The official Plus-M configuration uses 384x384 inputs, like Plus-S. The verified OAR
ONNX graph accepts the existing grayscale batch path. Plus-L's 768x768 preprocessing
must not be used for Medium. The graph and shared tokenizer digests are pinned by the
download script and checked again before loading the model.

For one identical 422x29-pixel production crop, release CPU inference after warmup:

| Model | Warm inference median | Warm samples |
| --- | ---: | --- |
| Plus-S | 321.012 ms | 326.927, 321.012, 316.723 ms |
| Plus-M | 1542.193 ms | 1508.933, 1542.193, 1551.776 ms |

Each model used a fresh process and reusable session. Initial single inference,
batches of two and three identical crops, cancellation, and post-cancellation reuse
preceded three measured single-crop calls. Those batch outputs matched their own
single-crop output exactly. Compilation and model loading are excluded from these
inference intervals. The two final measurements ran sequentially after other checks;
OS file caches were not reset and system-wide contention was not controlled. The
numbers describe this one warmed crop, not document latency or general throughput.

The initial functional Plus-M probe overlapped a separate build; its timings are
excluded from this table. `metrics.json` retains the final phase timings and raw
LaTeX. Local crops and page outputs remain under `target/formula-plus-m/`.

## Usage

Update the existing `[formula]` section in `docparse.toml`:

```toml
[formula]
inline_enabled = true
display_enabled = true
model_path = "models/pp-formulanet-plus-m/inference.onnx"
tokenizer_path = "models/pp-formulanet-plus-m/tokenizer.json"
model_manifest_path = "models/pp-formulanet-plus-m/model-manifest.json"
```

Run from the repository root:

```sh
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-m
rtk cargo run --release -p docparse-server --features coreml
```

Stop the existing server before starting a replacement on the same port. Use the
unified `cuda` feature on a CUDA host. This run did not restart the HTTP service.
For the model-only reproduction, set `FORMULA_TEST_MODEL=pp-formulanet-plus-m`,
`FORMULA_TEST_CROP` to the retained crop, and `FORMULA_BENCH_OUTPUT` to a fresh JSON
path, then run the ignored `real_formula_batches_preserve_cardinality_and_content`
test in `docparse-formula` with `--release --features coreml`.

## Validation and limits

- 30 formula/configuration tests and 10 download-script tests passed.
- The ignored real-model batch/cancellation checks passed separately for S and M.
- Real page-4 and page-5 regressions passed with the Medium model settings.
- Native CoreML and WASM warnings-as-errors Clippy passed.
- Browser runtime execution and CUDA hardware performance for Medium were not measured.
- The existing model-session lifecycle is reused; Python runtime dependencies are unchanged.

## Sources

- [Official Plus-M architecture and 384-pixel input configuration](https://github.com/PaddlePaddle/PaddleOCR/blob/main/configs/rec/PP-FormuaNet/PP-FormulaNet_plus-M.yaml)
- [OAR model release](https://github.com/GreatV/oar-ocr/releases/tag/v0.3.0)
- Graph SHA-256: `9e3539c2b4eeed28f2d35e342fd5bb0bdaa7f6034a475fc7e890c92780910618`.
