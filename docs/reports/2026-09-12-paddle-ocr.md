# PaddleOCR integration and acceptance

## Implementation

The independent `docparse-ocr` crate implements BGR detection preprocessing,
DB connected contours/minimum rectangles/scoring/unclip, cubic perspective crops,
RGB line-orientation classification, recognition resizing/padding and CTC decoding.
The dictionary retains complete Unicode strings. Model/config bytes are pinned
and SHA-256 verified; OAR is a reference implementation, not a dependency.

The parser adapts pixel quadrilaterals using the actual render transform. It
requests OCR for sparse pages, missing model regions, embedded image/chart
regions and strongly invalid mappings. Healthy native text suppresses overlapping
OCR even when spelling differs. Confident replacements archive original native
facts in `replaced_native_text`; validation counts those original IDs exactly once.
OCR geometry and estimated font metrics feed ordinary line, paragraph and table
composition. OCR-only word spacing precedes canonical byte-range assignment.

WebUI offers Automatic, All pages and Off. A single selected WebGPU/CPU backend
serves layout, tables and OCR. Worker actors retain tensors until actual ONNX
execution and output readback finish, including caller cancellation. CPU/CUDA/
CoreML/Metal/OpenVINO use the existing native backend boundary.

## Checks completed

- Native workspace: 335 tests passed, 12 opt-in tests ignored.
- Real OCR model test executed separately on CPU and CUDA; both passed title,
  body and confidence assertions. Initial elapsed test times: 10.08 s CPU,
  5.55 s CUDA (including model initialization).
- Native CoreML and OpenVINO OCR features and CLI `ocr-metal` compiled on Linux.
  These are compile checks, not hardware execution evidence for macOS or OpenVINO.
- Native and WASM Clippy passed with warnings denied.
- Production Web release built with Binaryen `-O4`; 7,620,445 byte parser WASM,
  145 verified imports. SDK/example TypeScript checks passed.
- All pre-commit hooks passed, including the platform boundary, rustfmt and Clippy.

## Real browser acceptance

Tests use the production example at `http://127.0.0.1:8768/example/` in the user's
Chrome session. Files are uploaded with the visible file chooser and parsed by
the production Worker and downloaded models. No fixture backend or substituted
predictions are used. Assertions inspect rendered UI text, source labels and
stage timing tables rather than hidden JavaScript application state.

The raster corpus removes the PDF text layer from real embedded-font fixtures:
upright English, 180-degree English, sideways English and rotated Chinese.
All four pages complete on WebGPU with 34 regions and 43 recognized OCR items.
The expected English lines and Chinese strings are recovered. A browser-found
sideways paragraph ordering bug was reproduced by a failing Rust regression test
and fixed by ordering lines along their reading frame with cardinal angle tolerance.
The final browser run verifies numbered lines 1 through 10 in order.

A mixed native/raster page retains `Native text must remain exactly once.` as
native text and recovers 13 OCR regions. CPU WASM and WebGPU produce identical
visible text on this input. CPU WASM takes 45.3 s including initialization;
its OCR stage takes 19.65 s. A first CPU attempt overlapped rebuilding served
artifacts and stopped its Worker; the stable-build rerun passes. This does not
constitute a general Worker-recovery guarantee.

The browser artifact directory is
`packages/wasm-web/test-results/paddle-ocr-2026-09-12/` (ignored by Git). It contains
raster inputs, visible extracted text, timing snapshots and a Chinese OCR screenshot.
`crates/ocr/tests/fixtures/generate_browser_corpus.py` regenerates the inputs;
`crates/web/tests/browser_ocr_acceptance.mjs` runs corpus checks with an already
claimed Browser skill tab.

## Corpus run

All 12 PDFs in `~/Downloads/docs` completed on the same WebGPU Worker: 368 pages,
5,907 regions, zero page errors, and no visible OCR-failure warnings. The detector
ran on 111 pages; the recognizer executed 6,146 times. Every page's visible warning
summary was checked. All regions on 36 sampled pages (first, middle and last per
PDF) were inspected, exposing 202 accepted OCR text items in those samples.

The UI-reported parse durations total 325.3 seconds. Models were initialized
before this corpus run; this total excludes model initialization and interactive
inspection time. The run reused its Worker without restarting or reloading models.

| PDF | Pages | Regions | OCR pages | Warning pages | Parse seconds |
| --- | ---: | ---: | ---: | ---: | ---: |
| 2303.18223v16.pdf | 144 | 2643 | 16 | 5 | 85.6 |
| 2403.01632v4.pdf | 47 | 612 | 13 | 8 | 22.9 |
| 2410.05779v3.pdf | 16 | 189 | 5 | 0 | 15.8 |
| 2412.05210v1.pdf | 12 | 164 | 6 | 2 | 14.4 |
| 2603.01919v2.pdf | 23 | 389 | 10 | 1 | 54.3 |
| 2604.18583v1.pdf | 15 | 310 | 8 | 2 | 10.3 |
| 2604.18584v1.pdf | 32 | 327 | 13 | 5 | 34.6 |
| 2604.21959v1.pdf | 3 | 46 | 2 | 0 | 3.7 |
| 2606.17056v1.pdf | 25 | 311 | 15 | 0 | 23.5 |
| 2609.04180v1.pdf | 23 | 418 | 9 | 3 | 21.9 |
| 2609.04184v1.pdf | 8 | 148 | 5 | 1 | 10.4 |
| 2609.04203v1.pdf | 20 | 350 | 9 | 5 | 27.9 |
| **Total** | **368** | **5907** | **111** | **32** | **325.3** |

The 32 warning pages are not counted as clean transcriptions. Two expose table
structure warnings: page 15 of `2603.01919v2.pdf` and page 19 of `2604.18584v1.pdf`.
Their original text remains available. The remaining warnings are presented by
the UI as generic parser warnings; the UI summary does not expose every underlying
warning code. No layout-inference failure message was observed.

`corpus-manifest.json` records current paths, page counts, sizes and SHA-256 values.
`corpus-results.json` contains all visible page warning summaries, sampled region
text/source labels and per-file timing tables. `summary.json` contains the compact
per-file totals. The directory was re-listed to confirm all PDFs were included.

After the complete corpus, the mixed page was parsed again on the same Worker.
It still retained its native title exactly once plus 13 OCR regions. The final
run took 1.1 seconds, with 724.4 ms in OCR (14 line classifications/recognitions,
including the subsequently suppressed native title). Evidence is in
`mixed-webgpu-final.json`, `mixed-webgpu-final-timings.json` and
`mixed-webgpu-final.jpg`. The browser is left displaying its OCR title region.

## Scope and limitations

OCR confidence is model evidence, not a measured character-accuracy guarantee.
Corpus completion checks page handling and real model invocation; it is not a
transcription benchmark with human ground truth. Layout/model reading order still
controls separate blocks on globally rotated pages; line orientation correction
is not document orientation classification. CoreML execution requires macOS
hardware and has not been run on this Linux host. Additional model families must
have an explicit verified preprocessing/dictionary contract before use.

Follow-up correctness fixes and targeted browser verification are recorded in
[OCR review fixes](./2026-09-12-ocr-review-fixes.md).
