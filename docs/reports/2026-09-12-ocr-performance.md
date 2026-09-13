# OCR performance and organization review

## Findings and fixes

1. Page preparation repeatedly classified the same native string inside the
   detection-by-text loop, including when OCR was disabled. `OcrPlan` now owns
   region policy and classifies native facts once. Disabled OCR retains coverage
   diagnostics but skips character totals and missing-region search.
2. Single-box overlap tests allocated clipping vectors, and each OCR candidate
   scanned every native item. Shared scalar `Bbox::intersection_area` removes those
   allocations. `covered_area` now allocates only for multi-box unions.
   A borrowed `TextIndex` narrows queries using sorted top edges and prefix maximum
   bottom edges, preserving tall spans. Planning, healthy-native overlap, reliable
   replacement coverage and unreplaced-native filtering reuse the same index.
3. OCR word spacing repeatedly scanned strings and reprojected every left-hand
   candidate. The dedicated spacing module caches Unicode/punctuation decisions
   and lazily indexes baselines for the four cardinal frames. Oblique directions
   keep the exact scan with cached boundaries. An independent reference test checks
   mixed scripts, heights, direction jitter and translated geometry.
4. OCR policy was embedded in page assembly and spacing in the merger. The modules
   now separate planning, indexing, ownership reconciliation, text alignment and
   spacing. Geometry predicates are shared, duplicate helpers were removed, and
   comments explain correctness boundaries and optimization limits. Browser session
   code also spells out input lifetime and GPU readback ownership.

No runtime dependencies or model/provider behavior were added. Native text,
rotation, font estimates, source IDs and table byte-range behavior remain covered
by the existing regressions.

## Release benchmarks

Command: `rtk cargo test -p docparse-core --release --lib ocr_perf_ -- --ignored --nocapture --test-threads=1`.
The measurements are seven-batch medians. Preparation uses 600 native items and
60 detections (20 calls per batch); spacing/merge use 1000 items (5 calls per batch).
Input cloning is included. No ONNX model initialization or inference is included.

| CPU operation | Before ms/page | After ms/page | Ratio |
| --- | ---: | ---: | ---: |
| spacing | 4.987 | 0.288 | 17.3x |
| merge | 13.526 | 0.408 | 33.1x |
| preparation Disabled | 3.305 | 0.166 | 19.9x |
| preparation MissingRegions | 3.280 | 0.106 | 30.9x |

These are synthetic CPU microbenchmarks, not whole-document speedup claims.
Sub-millisecond timings vary with scheduling and CPU frequency. Logs and a machine-
readable summary are under `packages/wasm-web/test-results/ocr-performance-2026-09-12/`.

## Verification

- Native workspace: 349 tests pass, 14 opt-in tests ignored. The two benchmark tests
  were separately executed in release mode.
- Native/WASM Clippy, pre-commit and example TypeScript checks pass.
- Production Web build: 7,889,562-byte optimized parser WASM, 145 verified imports.
- Indexed spacing matches the previous scan; rectangle union and interval-query
  tests cover duplicate, clipped, tall, nested and touching bounds.

## Real browser acceptance

All four inputs completed through the production example and its configured WebGPU
backend: **152 pages parsed**, with no page errors. Uploaded files were processed
by the real Worker and pinned ONNX models; no fixture backend was used.

| Input | Parsed pages | Regions | UI-inspected pages | UI elapsed time |
| --- | ---: | ---: | ---: | ---: |
| native-ocr-overlap.pdf | 1 | 4 | 1 | 22.0s |
| scanned-rotations.pdf | 4 | 34 | 4 | 3.7s |
| 2303.18223v16.pdf | 144 | 2643 | 3 | 90.4s |
| 2604.21959v1.pdf | 3 | 46 | 3 | 4.0s |

Small inputs were inspected on every page. Long-document UI inspection sampled
pages 1, 72 and 144, while the parser processed all 144 pages. Page 1 retains two
generic parser warnings; the inspected pages show no OCR failure. This is not a
claim that every uninspected page is warning-free or a transcription accuracy benchmark.

The visible mixed-row outputs remain `Total: 100`, `Pay USD 20` and `Hello, world`.
The sideways scan retains numbered lines 1 through 10 in order, and the Chinese
scan retains `中文文档解析测试`. Source labels confirm actual OCR output.

An additional OCR-disabled run of the real 3-page PDF completes on WebGPU in 4.1 s
with 46 regions and no OCR inference stages. Its timing table is in
`ocr-disabled.json`. This checks the disabled planning path through the actual UI.

`browser-manifest.json` records input paths, page counts and SHA-256 values.
`corpus-results.json` contains sampled visible text, warnings and timing rows.
The first mixed fixture includes model initialization; subsequent corpus inputs
reuse the same Worker. These end-to-end timings were not a controlled before/after
benchmark and must not be interpreted as the microbenchmark speedup ratios.
