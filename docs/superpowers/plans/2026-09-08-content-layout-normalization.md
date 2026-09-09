# Content Layout Normalization

## Approved behavior

- `reference` is an empty visual annotation. It does not own text, obstruct XY-cut,
  request OCR, merge with content, or enter body reading order. Bibliography text
  belongs to `reference_content`.
- Ordinary layouts merge only when either bbox completely contains the other,
  including identical boxes. Shared edges count as containment. Proximity alone
  does not justify a merge. Recheck against the current union after every merge.
- A short superscript/subscript with an unambiguous parent joins that parent's text
  before model assignment. Preserve the parent's baseline and font metrics when
  rebuilding its line; do not pull the following row into the same layout.
- Partial crossings remain separate regardless of IoU. Emit one `ContentLayoutOverlap` warning
  per page and record each pair's IDs and IoU under `content.overlap.*` diagnostics.
  Validation rejects surviving mergeable pairs, not every positive intersection.
- Watermarks remain detached, retain their text and precise polygon, and are exempt
  from content merging and overlap warnings.
- Preserve all original text facts exactly once. Merged layouts retain a stable
  primary `source_region` and every contributing region in `source_regions`.

## Script attachment and limits

The shared line implementation uses font size, baseline displacement and local
geometry. It runs before assignment so clipped model boxes cannot orphan a script,
and again during local line assembly. The decision is independent of filenames,
page numbers, model labels and specific text strings.

The conservative candidate is an upright run of 1–4 non-whitespace characters with
a font size between 45% and 85% of the parent's size. Its width is at most two parent
em units and its height does not exceed the parent band's height. It must share at
least 20% of its own vertical band with the parent and have a horizontal gap of at
most 0.25 em. Baseline displacement must be 0.15–0.9 em, with at least 0.15 em center
displacement. Copied baselines take priority over bbox-bottom estimates. Baseline
threshold comparisons allow 0.002 pt for PDFium's 1000x integer-device mapping and
floating-point rounding; containment comparisons remain exact.

Select postfix parents from original geometry and writing direction. Equally plausible
candidates remain separate;
an attached script cannot expand the search band. Nested indices may follow the
original parent graph because each edge goes to a strictly larger font.
Missing font size, long notes, same-baseline small type and neighboring columns do
not qualify. This is a typography heuristic, not structured formula recognition.

Adjacent pieces of one short script (such as `f` and `+1`) may form a run before
assignment only when their font, baseline and combined geometry fit an existing
larger parent. Ordinary body lines are not assembled at this stage.

PDFium origins are copied for upright text as well as oblique text. Upright body
items are grouped by measured baseline and compatible font size before scripts are
attached. A multi-item script band is reconsidered item by item when an individual
item has a plausible parent, so unrelated indices are not emitted as one synthetic
line. After attachment, adjacent fragments on the same retained body baseline can
rejoin when script extents fill an apparent inline gap. Remaining fragments on a
shared upright baseline are ordered left-to-right by that body baseline rather
than their script-inflated bbox tops. Layout containment rules
remain unchanged.

The regression excerpt in `crates/core/tests/fixtures/line/math-scripts.json` retains
81 text/geometry facts from the two-line Lemma 2 paragraph on page 31 of the
user-provided `2403.01632v4.pdf`. Baselines are mapped from its unrotated, 792-point
PDF coordinate system. Tests assert literal row text, script placement, source
iteration independence, and translation independence. The 17-item
`nested-math-scripts.json` excerpt separately checks that a split nested index
remains with its base before assignment.

## Implementation

- Reuse the native/Web Rust pipeline without dependencies, new platform gates,
  frontend unit tests, worktrees or commits.
- Keep annotation partitioning in assignment/page assembly and script ownership in
  `line/scripts.rs`. Keep general bbox normalization in `semantic/normalize.rs`.
- Keep renderer/schema provenance support and the example's dashed reference outlines.
- Keep diagnostics outside text facts and log one meaningful summary per page.

## IoU merge rollback (2026-09-09)

Removed the IoU-threshold merge branch and its matching validation/browser acceptance
requirement. IoU remains an overlap diagnostic only. Containment, script ownership,
reference annotations and watermark isolation retain their independent behavior.
Regression tests now require high-IoU partial intersections and overlap chains to
remain separate, while identical and contained boxes still merge.

Rollback verification: 215 native tests pass (5 ignored), native/WASM Clippy pass,
and the production Web build and SDK TypeScript check pass. Native and real WebGPU
parsing complete all 47 pages without page errors. All 18,421 native source facts
match the preceding implementation except derived item order; the five target Web
pages retain their original source facts. Page 32 changes from 27 to 28 layouts in
both environments because a high-IoU partial overlap is now retained. Other page
layout counts remain unchanged. Current evidence: `native-no-iou-merge.json`,
`no-iou-merge-audit.json`, and browser run `no-iou-web-c586429d` in the local artifact
directories listed below.

## Math line verification (2026-09-09)

- Native tests: 224 passed, 5 ignored. Native/WASM Clippy and the production Web
  build pass. No frontend unit tests were added.
- Native and real WebGPU parsing complete all 47 pages without page errors. All
  18,421 source facts retain their original fields except derived order and newly
  copied baselines. Original non-null baselines are preserved.
- Page 31 now has 24 layouts in both environments. The selected Lemma 2 paragraph
  has two lines instead of 19, the opening nested index stays on its formula line,
  and the Proof prefix precedes the following clause. The actual example selection
  was inspected and captured in `math-lines-page31.png`.
- CPU and WebGPU lifecycle suites pass three repeat parses, cancellation/recovery,
  and strict embedded-font/CJK native comparisons. Non-embedded font differences
  are recorded separately.
- Complete 144-page column and 42-page watermark regressions pass; the column gutter
  and all 39 detached watermarks are preserved.
- Current consolidated evidence: `target/overlap-analysis/math-lines-verification.json`.
  Formula output remains plain text; layout merging still requires full containment.

## Prior verification (before the IoU merge rollback)

The measurements below describe the previous policy and are not acceptance evidence
for the rollback.

- Native tests: 215 passed, 5 ignored. Native and WASM Clippy pass.
- Web package build and SDK/example TypeScript checks pass.
- The two reviewed synthetic cases now retain three crossing layouts with a warning,
  or combine body and script into one line while leaving the following row separate.
- Native parsing completes all 47 pages of `2403.01632v4.pdf` without page errors.
  All 18,421 source facts match the pre-change native output except derived item order.
- Native/WebGPU content counts on pages 24, 26, 27 and 28 are 16, 14, 7 and 10,
  respectively, with zero content intersections on those pages.
- Page 31 retains 70 content layouts and 44 partial intersections on native; WebGPU
  retains 73 and 47. All remaining pairs fail the merge criteria and are diagnosed.
  This intentionally replaces the earlier aggressive 22-layout result.
- WebGPU completes all 47 pages without model fallback or page errors. Original Web
  text facts on the five target pages match the pre-change evidence exactly.
- CPU and WebGPU browser lifecycle suites pass three repeat parses, bad-PDF recovery,
  cancellation and recreation checks. Embedded-font and embedded-CJK native/Web
  comparisons have zero differences; non-embedded font differences are recorded.
- Full WebGPU regressions pass for the 144-page column document and 42-page watermark
  document. Page 7 preserves its gutter and text facts; all 39 watermarks remain
  detached with their original text and precise polygons.

Local evidence is stored under `target/overlap-analysis/` and
`packages/web/test-results/`. Results from the earlier unconditional-overlap merge
implementation are historical evidence and do not establish this policy's behavior.
