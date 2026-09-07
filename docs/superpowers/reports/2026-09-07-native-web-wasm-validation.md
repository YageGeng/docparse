# Native/Web implementation and validation record

Date: 2026-09-07. This report records the verified scope, results, and reproduction commands for native/Web support.

## Delivered behavior

- Native and browser builds share PDFium extraction/rendering, Rust preprocessing, pinned PP-DocLayoutV3 inference, postprocessing, fusion, and DocumentResult.
- Platform conditions are restricted to five wasm_compat.rs entry points and the explicit core task_set/pdfium_worker/pdf_input and layout session_pool submodules. A scanner and pre-commit hook enforce this boundary.
- docparse-web exports Web functionality directly and is excluded from native default-members.
- WasmCompatSend/Sync and WasmBoxedFuture retain native thread bounds while allowing local browser futures.
- ModelArtifacts and DocParser::from_artifacts provide verified byte-based initialization.
- The Web SDK implements createParser, parse, render, close, Busy handling, AbortSignal, terminal failures, and opt-in CPU fallback.
- The pinned ort-web patch fixes module Worker loading, output selection, and JS session/tensor release.
- PDFium libraries, setjmp support, and runtime assets have recorded checksums; distribution packages include required licenses.

## Verification results

| Check | Result |
|---|---|
| cargo test --locked | Six default native crates: 165 passed, 5 ignored; resource-dependent tests are run separately |
| Native Clippy with tests/examples and -D warnings | Passed |
| Web-target Clippy with -D warnings | Passed |
| Formatting and diff whitespace checks | Passed |
| Cfg boundary and six behavior tests | Passed |
| Non-Send engine on native and native+wasm | Rc engine rejected on both |
| Non-Send engine on Web | Accepted |
| Shared wasm32 crate without wasm feature | Rejected as required |
| Web with native provider features | Rejected as required |
| Native default, wasm, CoreML/CUDA feature checks | Passed; compilation does not establish accelerator runtime acceptance |
| Fixed-model schema and session-lease regression | Passed |
| Five-image Python oracle | Passed on CPU, covering inference and postprocessing |
| Artifact-backed native PDF regression | Passed for one page, multiple pages, embedded fonts, and Chinese/rotation/CropBox/UserUnit |
| Chromium CPU/WASM acceptance | Passed, including 20 repeated parses and relocated SDK deployment |
| Chromium WebGPU fixture acceptance | Passed |
| GPU unavailable with explicitly allowed CPU fallback | Passed with the real CPU model |
| Direct native docparse-web build | Rejected by its target-specific build diagnostic |
| Release Web SDK build | Passed, including wasm-bindgen, packaging, and 134 verified WASM imports |

The browser environment was Chromium 152 on macOS. Firefox and Safari remain unverified. The full CUDA PDF corpus was not run in this validation.

## Browser run records

- CPU: 20 repeated parses, bad-PDF recovery, Busy, caller-buffer ownership, two actual fetches, geometry, cancellation, idempotent close, and recreation. The complete recorded run took about 108.2 seconds.
- WebGPU: actual webgpu provider, fixed fixtures, and lifecycle checks. The recorded run took about 6.3 seconds. This is not a performance SLA and does not measure GPU memory.
- Fallback: only GPU capability was disabled in the test Worker; the real model and CPU parser remained in use.
- Build manifest: pinned versions, seven PDFium library checksums, Rust WASM and ten ORT runtime asset hashes, and 134 final WASM imports.

The test server writes raw reports to `packages/web/test-results/<runId>.json`. The build writes `packages/web/dist/build-manifest.json`. Both are ignored generated artifacts. This document retains conclusions and reproduction instructions. Reports preserve substitute-font and diagnostic-weight differences without altering production results to match the oracle.

## Memory and ownership

During the recorded CPU repetition window, the two nonzero WASM memories remained at 272,171,008 and 721,682,432 bytes, approximately 947.8 MiB combined. A further observed memory had zero capacity. Capacity did not grow across 20 repeated parses. Each completed request left zero live output tensors and one active model session.

Larger documents or new fonts may increase allocator high-water capacity. These measurements are WASM linear-memory capacities, not process RSS, total JS heap, GPU allocations, or a mobile-device memory guarantee. A post-GC whole-process retained-memory threshold was not established by this observation.

JavaScript requested and returned only fetch_name_0 and fetch_name_1. The approximately 48 MB mask was not returned across the boundary; this does not establish pruning of the model's internal mask computation.

## Fonts, numeric parity, and input identity

Native system-font behavior is preserved. Unembedded-font samples produced 395 and 638 differing fields, primarily glyph metrics, boxes, confidence, and derived IDs, while preserving original text. The two-page embedded Vera sample passed strict comparison.

The Chinese fixture embeds Noto Sans SC and fixes Rotate=90, CropBox `[20,20,592,772]`, and UserUnit 2. Both platforms produced a 1504-by-1144 viewport and preserved Chinese characters. Full-field comparison passed with the established 1e-3 numeric tolerance. Two diagnostic model-edge weights differed by approximately 1e-6; only that known numeric component uses the tolerance, while IDs, source, reason, and other text remain exact.

`embedded_cjk_90.pdf` is the fixed geometry input. Native references include the PDF SHA-256, and browser comparison rejects changed inputs before comparing results.

## Reproduction

See the [Web package guide](../../../packages/web/README.md) for the full build and browser flow. Run these commands from the repository root:

```sh
rtk proxy python3 scripts/check_wasm_compat.py
rtk proxy python3 crates/core/tests/python/wasm_compat_test.py
rtk proxy python3 crates/core/tests/python/wasm_types.py
rtk cargo test --locked
rtk cargo clippy --tests --examples -- -D warnings
rtk cargo clippy -p docparse-web --target wasm32-unknown-unknown -- -D warnings
rtk proxy env DOCPARSE_LAYOUT__EXECUTION_PROVIDER=cpu rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
rtk proxy env DOCPARSE_WEB_REFERENCE_DIR=packages/web/test-results/native rtk cargo test -p docparse-core --test web_reference -- --ignored --nocapture
rtk proxy python3 packages/web/tests/serve.py --port 8767
```

Open `/packages/web/tests/browser.html?cycles=20&relocated=1&run=cpu-verified`. Add `provider=webgpu` for GPU validation. Reports use independent run IDs so failed runs are not overwritten. The report endpoint serves files and stores observations; it performs no parsing.
