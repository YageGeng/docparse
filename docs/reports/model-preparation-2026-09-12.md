# Model preparation acceptance — 2026-09-12

The Web SDK exposes `prepareModels(options)`, which returns a ready reusable
parser after runtime loading, artifact validation, and enabled ONNX session
creation. `createParser` delegates to that same path. The example provides
**Prepare models** without requiring a PDF and prepares automatically on parse.
Existing Rust builder initialization and provider selection are reused.

## Real browser checks

Acceptance used the production example, PDFium Worker, and pinned ONNX models
in Chrome. No requests, inference sessions, or parsing backend were mocked.
The mixed native/raster regression PDF was parsed twice per configuration:

| Provider | Enabled models | Model initialization | Accepted parses |
| --- | --- | ---: | ---: |
| WebGPU | Layout + TSR + OCR | 20,794.9 ms | 2 |
| WebGPU | Layout | 3,528.0 ms | 2 |
| WebGPU | Layout + TSR | 4,437.3 ms | 2 |
| WebGPU | Layout + OCR | 18,742.4 ms | 2 |
| CPU/WASM | Layout + TSR + OCR | 18,487.1 ms | 2 |

These are individual observations, not benchmark averages. Every preparation
completed with zero parsed pages and exactly one `model_init` observation.
Preparation timings remained identical after file selection and both parses.
OCR-enabled cases recovered `Total: 100`, `Pay USD 20`, and `Hello, world` while
preserving the native title. Disabled OCR produced no OCR inference observations.
Canceling preparation and immediately retrying also passed.

Two additional flows used the real three-page `2604.21959v1.pdf` from
`~/Downloads/docs`. Selecting it during preparation preserved initialization;
the subsequent parse reused those sessions and produced 46 regions. Changing
to all-page OCR and TSR-only mode, then clicking **Parse document** directly,
prepared once and produced 46 regions with three OCR detection runs.
The completed checks cover 12 parses and 16 pages.

An earlier CPU automation step exceeded its host time limit and interrupted
browser control. CPU acceptance was completed again in a fresh tab with a
longer host deadline: both parses passed, at 13.7 s and 13.2 s. Interrupted
attempts are excluded from the accepted counts. The final tab reported no
console errors. CoreML and CUDA were not retested; this change does not alter
native session construction. Preparation does not run dummy inference, so the
first inference may still compile provider kernels lazily.

## Reproduction and evidence

Generate the mixed PDF with
`crates/ocr/tests/fixtures/generate_browser_corpus.py`, then run
`runPreparation` from `crates/web/tests/browser_prepare_acceptance.mjs` on a
fresh example tab. The Web package README documents the runner and CPU resume
option. Retained local artifacts are under
`packages/web/test-results/model-prepare-2026-09-12/`: the combined matrix is
`all-preparation-results.json`, additional flows are `lifecycle-results.json`,
and screenshots are `final.jpg` and `timings.jpg`.

SDK and example TypeScript checks, the production SDK/WASM build, example
build, JavaScript syntax check, and `git diff --check` passed. The configured
pre-commit hooks were checked; all are Rust-only and skipped these Web changes.
