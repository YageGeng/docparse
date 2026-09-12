# OCR review fixes

## Changes

- Healthy native text suppresses OCR only when its union covers the OCR result.
  Partial overlaps align normalized native text with original UTF-8 OCR ranges.
  Prefixes, suffixes and internal native spans remain unchanged; unmatched OCR
  fragments receive sliced reading quads and stable source-result subindices.
  Unalignable partial overlap retains content with an `OcrPartialOverlap` warning.
  Matched native baselines and font measurements calibrate the residual OCR hints
  while retaining their estimated status and original polygon geometry.
- Confident OCR boxes jointly replace unusable native facts. Union coverage is
  clipped to the native box and does not double-count repeated predictions.
  Incomplete or weak replacement leaves the original text as its sole owner.
- 180-degree line ordering uses the reading axis, including RTL reversal. Measured
  half-turn baselines retain their source direction.
- Numeric OCR font hints retain `font_size_estimated` through line metrics, so
  paragraph decisions use the configured estimated-size tolerance.
- OCR spacing accepts closing punctuation after a word while preserving existing
  whitespace and CJK boundaries.

Text alignment lives in `core/ocr/overlap.rs`, separate from merger policy and
model inference. Shared rectangle union uses the existing `geo` dependency in
`docparse-layout`; no new dependency or backend was introduced.

## Verification

Ten permanent regression tests cover the five review findings plus internal
fragment IDs/geometry, native suffixes, half turns, UTF-8, explicit spaces,
fragmented PDF glyph runs, and duplicate/weak replacement evidence. The original
five tests failed before fixes. All ten now pass.

The native workspace passes 345 tests with 12 opt-in tests ignored. Native and
WASM Clippy pass with warnings denied. All pre-commit hooks pass. The production
Web build passes TypeScript and emits 7,856,523 byte optimized parser WASM with
145 verified imports.

A first real-browser inline fixture exposed one additional grouping case:
`Pay USD 20` became `USD Pay 20`. Native CPU parsing reproduced the same result.
The native label used a measured 20-point font and baseline y=157; the OCR box
included detection padding, producing a 25-point hint and baseline y=161.5.
Those differences separated one physical row into three lines. A permanent
regression reproduces that input, and matched native measurements now calibrate
the OCR fragments without changing native facts. Real CPU parsing now returns
`Total: 100`, `Pay USD 20`, and `Hello, world` exactly.

The final production WebGPU run returns `Total: 100`, `Pay USD 20` and
`Hello, world` exactly, with native labels present once. It completes in 22.7 s
including model initialization. The internal-label row owns two OCR fragments.
Evidence is in `packages/web/test-results/ocr-review-fixes-2026-09-12/`, including
`native-ocr-overlap-fixed.json` and `native-ocr-overlap-fixed.jpg`.

The four-page English/Chinese rotation corpus passes on WebGPU in 3.6 s with
34 regions and no page warnings. Its sideways paragraph retains numbered lines
1 through 10 in order; Chinese text is recovered. Evidence: `rotations-fixed.json`.

The real `~/Downloads/docs/2604.21959v1.pdf` also passes: 3 pages, 46 regions,
3.9 s, OCR invoked on 2 pages, and no page warnings. Its visible page/text and
timing evidence is in `corpus-results.json` in the same directory. This follow-up
uses focused regression inputs; the earlier complete 368-page corpus run is
recorded separately in the initial integration report.

The browser is left displaying `Pay USD 20` with two OCR fragments and its original
native label. No commit was created.
