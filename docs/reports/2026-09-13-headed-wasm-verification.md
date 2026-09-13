# Headed browser WASM verification

Date: 2026-09-13. Final acceptance ran in the user's existing, visible Chrome browser through the browser-control extension. The production release SDK, Worker, PDFium and pinned ONNX models were used throughout. No parsing backend or inference output was replaced.

## Environment and build

- Chrome 153 on Linux, with the production example at `http://127.0.0.1:8768/example/`.
- The actual headed browser returned a WebGPU adapter with vendor `nvidia`, architecture `lovelace`, and `isFallbackAdapter = false`.
- The release SDK rebuilt successfully, including wasm-bindgen and Binaryen validation. The optimized module was 7,916,226 bytes, with 145 verified imports.
- The initial headless run was stopped when the user requested direct control of the headed browser. Its incomplete results are not counted below. No flags or settings were changed in the user's browser.

## SDK acceptance

| Backend | Result | Evidence |
| --- | --- | --- |
| CPU WebAssembly | Passed all 14 checks | [CPU report](../../packages/web/test-results/headed-wasm-2026-09-13-d9cf1e77.json) |
| WebGPU | Passed suite: 13 checks passed, one diagnostic comparison recorded differences | [GPU report](../../packages/web/test-results/headed-webgpu-2026-09-13-c5d02962.json) |

Both runs exercised real PDF bytes, offset views, Busy handling, three repeat parses, malformed-PDF recovery, active cancellation, parser recreation, idempotent close, native/Web text parity, embedded fonts, rotated Chinese text, CropBox/UserUnit geometry, table spans and empty cells, renderers, per-page timings, progress, and PNG page images. Forced WASM memory growth preserved borrowed inference inputs without intermediate copies. Completed repeated parses retained zero live runtime tensors. The WebGPU test also observed actual GPU command submissions.

The GPU comparison recorded two differences on the unembedded-font multipage fixture: `.pages.2.blocks.5.model_order` and `.pages.2.blocks.5.source_region.model_order` were 128 instead of the native reference's 129. Raw text was preserved. Embedded-font, rotated-Chinese and structured-table strict comparisons had zero differences. The existing diagnostic comparison policy was retained.

## Visible production UI checks

- Prepared layout, TSR and all three OCR models using **Prepare models**; the UI reported the actual backend.
- Uploaded `mixed-native-scan.pdf` through the real file chooser. WebGPU returned 14 regions; its warmed repeat returned identical text, retaining the same single model-initialization observation.
- CPU WebAssembly ran detection once, orientation 14 times and recognition 14 times on the same PDF. All extracted region text matched the WebGPU run.
- Forced **TSR model only** on a real table page. WebGPU produced an 8-by-4 table with `Source: TSR input`, preserving merged headers, blank cells, and values including `32.5`, `28.1`, and `25.4`.
- Repeated the forced-model table check on CPU WebAssembly. It executed one real TSR inference and produced exactly the same cell text, row spans and column spans as WebGPU.
- Exported and decoded the actual table-page PNG at 1224 by 1584 pixels.

## Real long PDF

Input: `/home/isbest/Downloads/docs/2303.18223v16.pdf`, 144 pages, default rules-first TSR and automatic OCR, WebGPU. Models were prepared before parsing.

The UI reported **144 pages, 2643 regions, 137.6 seconds** for the whole PDF. All 144 page images were present and all 144 pages were individually opened through the UI. Page 8 exposed a 58-by-13 table recovered from rules and separators.

| Measured document stage | Calls | Total |
| --- | ---: | ---: |
| Layout inference | 144 | 43,953.9 ms |
| OCR detection inference | 16 | 9,169.5 ms |
| OCR orientation inference | 717 | 5,535.1 ms |
| OCR recognition inference | 717 | 18,290.7 ms |
| TSR inference | 5 | 40,500.2 ms |
| Rust parse total | 1 | 135,963.1 ms |

Preparation recorded one model initialization of 22,341.7 ms, separately from parsing. These are acceptance-run observations, not an isolated throughput benchmark: the CPU SDK acceptance overlapped part of the long-PDF run, and stage totals include overlapping/nested work.

Seven pages retained 19 visible parser warnings: page 1 had 2; page 23 had 12; pages 17, 39, 67, 110 and 139 had 1 each. Their UI message indicated general parser warnings and potentially absent native text. The document completed successfully; the warnings were not suppressed or treated as proof of full semantic accuracy.

## Test correction

Updated `packages/web/tests/browser.js` to expect two `text_finish` observations per successfully analyzed page. The current pipeline calls `PageAnalyzer::compose` before table inference and `PageAnalyzer::complete` afterward, with each section measured separately. The old test still required one observation. The corrected assertion remains exact and reports expected/actual counts; production timing and parser behavior were not changed.

The corrected test passed in both headed backends. JavaScript syntax and `git diff --check` passed.
