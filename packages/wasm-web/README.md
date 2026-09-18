# DocParse WASM Web

Run Rust DocParse, PDFium, and the pinned PP-DocLayoutV3, SLANet_plus and PaddleOCR models inside a dedicated module Worker. By default, PDFs and models are processed locally in the browser, producing the same `DocumentResult` schema as native builds without a parsing server.

Run every shell command in this guide from the repository root. Select this
package with `--prefix packages/wasm-web`; use its npm scripts for builds,
checks, and the interactive example.

## Build

Prepare the pinned model and tools from the repository root:

```sh
rtk rustup target add wasm32-unknown-unknown
rtk cargo install wasm-bindgen-cli --version 0.2.125 --locked
rtk uv run --locked scripts/download_models.py
```

Install and build the browser package:

```sh
rtk npm ci --prefix packages/wasm-web --ignore-scripts
rtk npm run build --prefix packages/wasm-web
```

`npm run build --prefix packages/wasm-web` uses Cargo's release profile, runs wasm-bindgen, then runs the pinned npm Binaryen 132.0.0 `wasm-opt -O4`. No system `wasm-opt` installation is required. Optimization preserves SIMD, bulk memory, reference types, and PDFium exception handling; invalid output fails the build. The manifest records optimization flags, before/after byte sizes, elapsed optimization time, and the hash of the optimized artifact. ORT's own prebuilt WASM files are copied unchanged.

`packages/wasm-web/dist/` contains the ES module API, Worker, Rust WASM, ORT 1.27.0 assets, WASI adapter, and licenses. Its `build-manifest.json` records versions, file SHA-256 values, PDFium library checksums, and final WASM imports. The build verifies the pinned PDFium chromium/8028 libraries and real setjmp runtime; arbitrary replacement SDKs are rejected.

The SDK build excludes the example and its presentation dependencies. `src/types.ts` contains public data and option contracts; `src/protocol.ts` contains the private typed Worker messages. The example lives under `packages/wasm-web/example/src`, consumes the built public SDK, and emits only to `packages/wasm-web/example/dist`.

Check the SDK and example from the repository root after building the SDK:

```sh
rtk npm run check --prefix packages/wasm-web
rtk npm run check:example --prefix packages/wasm-web
```

Copy the complete `packages/wasm-web/dist/` directory to any static-site directory while preserving internal relative paths. Deploy the ONNX model, inference.yml, and model-manifest.json separately. Serve correct JavaScript/WASM MIME types and permit CORS for cross-origin model/runtime resources. The explicit single-threaded CPU/WASM path requires no cross-origin isolation. ORT telemetry is disabled. WebGPU additionally requires a supported secure context and compatible browser/device.

## Usage

```javascript
import { prepareModels } from "/assets/docparse/index.js";

// Prepare the runtime and enabled sessions before a PDF is selected.
const parser = await prepareModels({
  artifacts: {
    kind: "urls",
    model: "/models/pp-doclayout-v3/inference.onnx",
    config: "/models/pp-doclayout-v3/inference.yml",
    manifest: "/models/pp-doclayout-v3/model-manifest.json",
  },
  tsrArtifacts: {
    kind: "urls",
    model: "/models/slanet-plus/inference.onnx",
    config: "/models/slanet-plus/inference.yml",
    manifest: "/models/slanet-plus/model-manifest.json",
  },
  tsrCellArtifacts: {
    kind: "urls",
    model: "/models/rtdetr-table-cell-wireless/inference.onnx",
    config: "/models/rtdetr-table-cell-wireless/inference.yml",
    manifest: "/models/rtdetr-table-cell-wireless/model-manifest.json",
  },
  config: { formula: { engine: { type: "texo" } } },
});

try {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const document = await parser.parse(bytes);
  const markdown = await parser.render(document, "markdown");
  console.log(document.pages.length, markdown);
} finally {
  await parser.close();
}
```

Table recovery defaults to `config.tsr.mode = "fallback"`: local rules run first,
then the built-in TSR processes unresolved tables. Use `"tsr_only"` to send
all tables to TSR, or `"rules_only"` to omit TSR artifacts and model loading.
The default combination is SLANet+ with wireless RT-DETR cell detection.
`tsrArtifacts` and `tsrCellArtifacts` use the same URLs/bytes contract as `artifacts`.
Set `config.tsr.cell_detection.enabled = false` to use SLANet+ alone and omit
`tsrCellArtifacts`. All models use the same selected backend, defaulting to WebGPU.

Here, `file` is a caller-selected File. To manage authentication or caching, obtain the model yourself and pass `{kind: "bytes", model, config, manifest}` with three Uint8Array values. The library does not detach caller-owned PDF or model buffers.

`prepareModels(options)` initializes ONNX Runtime and returns a ready, reusable
parser without opening a PDF or running dummy inference. `createParser(options)`
remains compatible and delegates to the same preparation path. Reuse the returned
parser across documents; call `close()` and prepare again when model settings or
the execution provider change. Native callers already prepare their sessions in
`DocParser::from_config` or `DocParserBuilder::build`.

| Model | Preparation policy |
| --- | --- |
| Layout | Always initialized; there is no layout-disable setting. |
| TSR | Initialized by default and for `tsr_only`; skipped for `config.tsr.mode = "rules_only"`. |
| TSR cell detection | Initialized alongside TSR unless `config.tsr.cell_detection.enabled = false`. |
| OCR detection and recognition | Initialized for `missing_regions` and `always`; skipped for `config.ocr.policy = "disabled"`. The SDK defaults to disabled unless a policy is selected; the example explicitly selects automatic OCR. |
| OCR orientation | Initialized only when OCR is enabled and `config.ocr.classify_orientation` is not `false`. |
| Formula recognition | Inline and display recognition have independent switches, both defaulting to on. Choose `config.formula.engine.type`: local `texo` (default) or `pp` loads same-origin `/models/` presets; `mineru` sends formula crops to the configured `server_url`. `formulaArtifacts` overrides local resources only. |

Disabled models do not require artifact sources and are not downloaded or
initialized, even if sources are supplied. Preparation accepts `signal`,
`onProgress`, and `onTiming` for cancellation and observation. First inference
can still include provider-specific lazy kernel compilation.

`config` uses native business groups and snake_case fields, such as `{render: {dpi: 144}, layout: {score_threshold: 0.5}}`. Filesystem paths (including the nested `ocr.detection`, `ocr.recognition`, and `ocr.orientation` groups), profiles, environment variables, and per-model `execution_provider` fields are excluded. Use the Worker's `executionProvider` option for all browser models together. All four concurrency settings must equal 1 in the initial Web implementation; invalid settings fail explicitly.

`runtimeBaseUrl` can select a self-hosted ORT directory containing the same JS/mjs/wasm versions as the build manifest. Relative model URLs resolve against the calling page. Default runtime resources follow the SDK deployment location.

## Stage timings

`prepareModels({ onTiming })`, its compatible `createParser` entry, and `parser.parse(bytes, { onTiming })` accept a callback with `{stage, page_number, duration_ms}`. Page numbers are one-based; `null` denotes document-wide work. Timings never enter `DocumentResult`, so native/Web comparison and stored JSON stay deterministic. Rust callers use `ParseObserver::on_timing`; `RUST_LOG=debug` also reports elapsed stages.

```javascript
const timings = [];
const document = await parser.parse(bytes, {
  onTiming: event => timings.push(event),
});
console.table(timings);
```

| Stage | Measured interval |
| --- | --- |
| `runtime_load`, `model_download`, `model_init` | Worker WASM initialization, parallel artifact downloads, then ORT/model initialization (including an allowed fallback attempt). Downloads are omitted for caller-supplied bytes. |
| `pdf_open`, `text_extract`, `pdf_render` | PDFium actor turnaround, including dispatch and native executor waits; extraction/rendering are attributed to each page. |
| `document_context` | Watermark classification and document statistics. |
| `layout_preprocess` | CPU resize/normalization/tensor preparation; excludes native blocking-executor wait. |
| `layout_queue` | Session availability and native inference-executor wait, or browser actor queue wait. |
| `layout_inference` | Input binding and the synchronous ORT run or asynchronous ORT Promise. Includes runtime transfers/lazy compilation performed inside that call; this is not GPU kernel time. |
| `layout_readback` | Web output synchronization plus conversion of the two consumed outputs; native output conversion. Native provider-internal transfers remain in `layout_inference`. |
| `layout_postprocess` | Detection filtering and coordinate conversion. |
| `tsr_preprocess`, `tsr_queue`, `tsr_inference`, `tsr_postprocess` | Table crop preparation, single-session wait, selected-backend model execution, and token/location decoding. |
| `text_prepare`, `ocr`, `text_finish` | Native text/layout preparation, an actual OCR call when needed, then final text/layout composition. |
| `link_validate` | Cross-page linking, result assembly, and validation. |
| `result_serialize`, `preview_encode` | Rust result conversion to JavaScript; each Worker PNG encoding operation, including asynchronous waiting. |
| `parse_total`, `worker_total` | Inclusive Rust parse time; inclusive Worker initialization or parse time, respectively. Worker parse time includes serialization and preview completion, but excludes request/result transfer and UI rendering. |

All values use monotonic wall clocks, including `performance.now()` in the dedicated Worker. Concurrent stages overlap, and totals include nested work: **do not add all stage durations to compute elapsed parse time**. The example's status timer additionally includes main-thread setup, file reading, and message delivery. Initializing a model and parsing a document are separate callback scopes. The first real inference can include lazy runtime/GPU compilation; no explicit dummy warmup run is added.

Observations describe elapsed attempts, not success. Error/cancellation can leave a partial set of events; stages that never run have no record. SDK callbacks are isolated from parser failures and scoped to the active request. Rust callbacks are delivered serially by the parse future, potentially after a stage has ended.

## Interactive example

The example selects WebGPU, automatic OCR, and rules-first TSR fallback by default. The OCR selector offers automatic missing-region recovery, all pages, and off. Its table selector also provides TSR-only and rules-only modes. The example explicitly allows CPU fallback for all enabled models on unsupported devices. Its engine selector also supports CPU/WASM, releasing the old Worker when changed. The engine status shows the initialized backend, including CPU fallback, rather than only the requested preference.

After preparing the models and building the SDK, start the example from the repository root:

```sh
rtk npm run example --prefix packages/wasm-web
```

This type-checks `packages/wasm-web/example/src/main.ts`, bundles its presentation libraries and KaTeX fonts locally with esbuild, and starts the static server. Use `rtk npm run build:example --prefix packages/wasm-web` to build the example without starting a server. Changes to the UI do not require rebuilding Rust/WASM. Keep this terminal running and use a separate terminal for acceptance commands.

Open <http://127.0.0.1:8768/example/>. Select **Prepare models** to initialize the selected models before choosing a PDF. Choosing a PDF during preparation keeps that preparation running. **Parse document** also prepares automatically when needed and reuses ready sessions. Changing the engine, table mode, or OCR policy releases those sessions and enables preparation again. The example displays actual model-download, text-extraction, and page-analysis progress. Browse the PDFium page thumbnails, zoom the page, and toggle overlays without changing the parsed geometry. Click an overlay, or use the region menu, to inspect and copy its text. On narrow screens the selected text opens in a dismissible floating inspector.

The workspace fills the remaining viewport height and fits the entire PDF page by
default. The introduction collapses after file selection. Zoom percentages are relative
to this fitted size; click **Fit entire page** to reset both scale and scroll position.
Window resizing refits the image and overlays together. Thumbnails, enlarged pages, and
long extracted text scroll within their own panels. Exported PNG resolution is unchanged.

**Stage timings** opens a snapshot with counts, cumulative time, mean, and first measurement for each stage. **Preparation** timings describe the last preparation attempt and survive PDF selection and repeated parses; **Document** timings reset for each PDF. Changing model settings clears both scopes. Reusing a ready parser does not record another initialization. The dialog preserves the fitted workspace height.

**Export PNG** opens the generated image for inspection; **Save PNG** then downloads it. Some embedded browsers cancel file downloads, but the image remains available in the preview. Cancel stops the Worker; the next parse creates a fresh parser. A ready model is reused when choosing another PDF. Returning through browser history does not revoke a cached document's preview URLs.

The static server exposes only the example, built SDK, and model directory. It does not receive PDF uploads or perform parsing. Set `PORT` to use another local port:

```sh
rtk proxy env PORT=8769 rtk npm run example --prefix packages/wasm-web
```

The example uses PDFium's inference raster and SVG hit targets; it has no pdf.js dependency. Native PDF text is extracted; image-only and outlined text still require OCR.

### UI end-to-end acceptance

`crates/web/tests/browser_prepare_acceptance.mjs` exports the Browser-skill
generator `runPreparation(tab, pdf, outputDirectory)`. Run it on a fresh example
tab with the generated `native-ocr-overlap.pdf` regression input. It checks
preparation without a PDF, OCR/TSR switch combinations, cancellation, text output,
and two parses per prepared session on WebGPU and CPU. Pass
`{ providers: ["wasm"] }` as the fourth argument to resume only CPU checks. Allow
at least 60 seconds for each generator step in browser-control hosts; CPU parsing
and subsequent UI assertions can exceed a 30-second host limit.

The unified entry builds the SDK and example, regenerates native references, starts owned servers on free ports, and runs SDK acceptance, UI flows, and export-race checks against the real model:

```sh
rtk npm exec --prefix packages/wasm-web -- playwright install chromium
rtk npm run test:e2e --prefix packages/wasm-web
```

Pass script arguments after `--`, keeping `--prefix` before it. For example,
`rtk npm run test:e2e --prefix packages/wasm-web -- --headed` shows the browser.
Use `--channel chrome` to select an installed Chrome or `--cycles 20` for the
longer SDK repeat matrix. Reports and the final screenshot are written to
`packages/wasm-web/test-results/e2e/`. Playwright is a development-only E2E
dependency; there are no frontend unit tests.

`tests/run.e2e.mjs` owns command-line preparation and browser startup. Its exported `prepareE2E()` can also prepare servers from a Node host. The browser-controller-independent `tests/suite.e2e.mjs` exports `runE2E(driver, environment)` for both the CLI and Browser Use. A Browser Use driver supplies `page: tab.playwright`, `navigate: url => tab.goto(url)`, and the tab's CDP capability. Exhaust the generator and close the prepared environment when finished.

`tests/example.e2e.mjs` exports an async generator that drives the real example through a Playwright-compatible page. It uses file choosers, clicks actual SVG overlays, inspects the visible text, checks zoom/navigation, decodes the export preview, cancels an active parse, and verifies bad-PDF recovery. It never intercepts requests or replaces the production Worker or model.

Start the example server, navigate a fresh browser page to `/example/`, and supply absolute paths to the three-page `multipage_layout.pdf`, an invalid PDF, and a multipage PDF for cancellation:

```javascript
import { runExampleE2E } from "./tests/example.e2e.mjs";

for await (const event of runExampleE2E(page, {
  pdf: multipageFixturePath,
  invalidPdf: invalidFixturePath,
  cancellationPdf: longPdfPath,
})) {
  console.log(event);
}
```

With Codex Browser Use, pass `tab.playwright` as `page`. The generator yields progress between assertions so a long model initialization does not conceal test status. Run the UI checks in a visible tab, verify both wide and narrow layouts, and retain the resulting check records in `test-results/`. A tab reclaimed by the host is an interrupted run, not a pass. The SDK-level real-model matrix below separately verifies numerical parity and Worker resource cleanup.

`tests/export-race.e2e.mjs` adds controlled PNG callback timing to a real parsed three-page example. Pass the page and a CDP session to `runExportRaceE2E(page, cdp)`. It checks both completion orders, stale failures, Escape cancellation, and blob URL cleanup. Encoding itself remains real, and the test restores its instrumentation in `finally`.

## Progress and page images

```typescript
const parser = await createParser({
  artifacts, tsrArtifacts, tsrCellArtifacts, formulaArtifacts,
  onProgress: event => console.log(event.stage),
});
const document = await parser.parse(pdfBytes, {
  signal: abortController.signal,
  onProgress: event => console.log(event),
  onPageImage: ({ pageNumber, width, height, blob }) => {
    // The PNG is the same PDFium raster used by layout inference.
    // Create an object URL for display, and revoke it when the document is released.
    displayPageImage(pageNumber, width, height, blob);
  },
});
```

Initialization reports `loading_runtime`, `downloading`, and `initializing_model`. Parsing reports `opening`, `scanning`, `analyzing`, `linking`, and `complete`. Scan/analysis events contain actual `completed` and `total` page counts. Download `loaded` counts decoded bytes; `total` is provided only when an unencoded content length can be established. Compressed responses, unknown lengths, and CORS responses with hidden encoding metadata report bytes without a percentage. Cross-origin hosts can explicitly expose `Content-Encoding: identity` when they serve uncompressed artifacts. Observers are optional and their exceptions are logged without settling the parse request. Progress does not release the parser's busy state.

`onPageImage` receives a PNG `Blob` and its pixel dimensions. Use `PageResult.width`, `PageResult.height`, and block `bbox` values for overlay coordinates, scaling the raster to that same viewport. Images can arrive before the final `DocumentResult`, and the parse promise resolves only after every requested image has been delivered. Cancel/close rejects pending work and ignores late events. Pages whose rendering fails may have no image; inspect the result's page warnings and errors. Without `onPageImage`, no preview pixels are copied out of WASM or PNG-encoded.

## Watermarks and geometry

Draw `Block.polygon` when present, falling back to `Block.bbox`. Polygon vertices are
in the same viewport point space as the bbox. The example uses this contour for SVG
hit testing and PNG export, including slanted watermarks. `label: "watermark"` denotes
an independent block excluded from body fusion and reading-order constraints; its
text remains inspectable and retained in text/Markdown output.

## Lifecycle

- `prepareModels` and `createParser` resolve only after fixed artifact validation and actual initialization of every enabled model.
- Each parser accepts one parse/render operation at a time; concurrent calls fail with `ParserBusy`.
- `parse(bytes, {signal})` accepts an AbortSignal. An already-aborted request leaves a ready instance usable. Active cancellation terminates the Worker and requires a new `createParser` call.
- `close()` is idempotent, rejects pending work, and makes the instance unusable.
- Fatal Worker/WASM failures settle pending requests instead of leaving Promises suspended.
- Errors contain a stable `code` and readable message; Worker error stacks are retained for diagnostics.

Set `{executionProvider: "webgpu"}` to request WebGPU explicitly. The WASM dependency graph enables `ort/webgpu`; the host loads the pinned WebGPU distribution and registers its execution provider. WebGPU is the SDK default for layout, TSR and OCR; select `executionProvider: "wasm"` for CPU. PDFium rendering, text extraction, and fusion still run on CPU.

GPU-to-CPU fallback is disabled by default. Only `allowCpuFallback: true` permits CPU fallback when GPU capability or provider initialization is unavailable. Model hash/schema and inference-data failures do not trigger fallback. The read-only `parser.executionProvider` reports the initialized backend, including `"wasm"` after fallback. ORT may still execute unsupported operators on CPU within a WebGPU session. Browser layout, TSR and OCR serialize actual inference through output readback to avoid cross-session GPU buffer reuse after a caller timeout. See the [ONNX Runtime WebGPU guide](https://onnxruntime.ai/docs/tutorials/web/ep-webgpu.html).

Run `rtk npm run test:e2e --prefix packages/wasm-web -- --provider webgpu` for strict GPU acceptance. The real-model test records actual GPU queue submissions and inference durations in addition to provider registration. A GPU request with no observed GPU commands fails acceptance.

## Content layouts and reference annotations

`reference` blocks retain model geometry with empty `text` and `lines`. They are
annotation-only outlines and do not participate in text ownership, XY-cut, content
merging, or body reading order. The example draws them dashed and makes their interior
transparent to content selection. Bibliography content is labeled `reference_content`.

Ordinary content bboxes merge only when one completely contains the other, including
identical boxes. Partial intersections remain separate regardless of IoU and
produce a `ContentLayoutOverlap` page warning with `content.overlap.*` diagnostics.
Enable `output.include_diagnostics` to include per-pair details in serialized results.
Short superscripts/subscripts join an unambiguous parent using font size, baseline
shift and position before model assignment. Upright PDFium baselines keep body rows
in order; nested indices follow the original typography graph, and adjacent fragments
on the same body baseline reconnect after scripts fill their inline gap. Nearby rows
remain separate. Optional PP-FormulaNet_plus-S recognition adds LaTeX and Markdown
to `pages[].formulas` without changing source text ownership. Unmatched inline
formula detections retain their diagnostics and independent recognition metadata.

A merged block retains a stable primary identity and `source_region`. Its optional
`source_regions` array records every contributing model/fallback region, including
original labels and geometry. Raw source regions may overlap. `reference` and
`watermark` annotations are exempt from content merge validation and overlap warnings.

## Native/Web result parity

Native font selection is preserved. Embedded-font PDFs provide strict parity fixtures. For unembedded fonts, host substitution can change glyph metrics, rendered images, confidence scores, coordinates, and derived IDs. Record these differences separately while checking text preservation and deterministic results within each environment.

The model is approximately 124.46 MiB. The browser also holds Rust/PDFium, ORT, image, and tensor buffers. A single WASM memory capacity is not total process memory, and arbitrary document sizes are not guaranteed. Inference requests only boxes and count; the unused mask is not returned.

## Real-browser acceptance

The unified E2E suite grows the actual Rust WASM heap after input tensor creation.
ORT must still run the real model without `LayoutUnavailable` degradation. Run the
standalone browser harness with `growMemory=1` to exercise the same regression.
When `WebAssembly.Memory.toResizableBuffer()` is available, input tensors borrow Rust
memory through views that survive growth. The linked module declares the wasm32 4 GiB
ceiling; this does not preallocate 4 GiB. Older engines use JavaScript-owned input
snapshots. This removes an intermediate tensor copy on supporting engines, not ORT's
copy into its separate WASM heap or GPU uploads. The build adapts wasm-bindgen's UTF-8
codec boundaries because browser text codecs require fixed buffers; string conversion
can still copy bytes.

Acceptance records borrowed and copied input bytes and requires zero intermediate
input copies when the engine supports growable buffers. Add `expectViews=1` to require
that capability explicitly, or `legacyMemory=1&growMemory=1` to disable it only in the
test Worker and validate the compatibility path with actual inference.

From the repository root, generate native references and start the static/report server:

```sh
rtk proxy env DOCPARSE_WEB_REFERENCE_DIR=packages/wasm-web/test-results/native rtk cargo test -p docparse-core --test web_reference -- --ignored --nocapture
rtk uv run --locked packages/wasm-web/tests/serve.py --port 8767
```

Open `/packages/wasm-web/tests/browser.html?cycles=20&relocated=1&run=cpu-stress`. The page uses the production build, real model, and PDF fixtures. `relocated=1` checks deployment under a renamed directory. Reports are saved to `packages/wasm-web/test-results/cpu-stress-<run-id>.json`, including parity, actual fetches, live tensors, WASM capacities, and cancellation/close results.

Use `provider=webgpu` for a separate GPU run. `provider=webgpu&fallback=1&noGpu=1` disables GPU capability only in the test Worker and validates explicit fallback with the real CPU model. Replace `noGpu=1` with `failGpuInit=1` to reject GPU session creation at the ORT boundary; omit `fallback=1` to require an explicit failure. Diagnostic edge weights use the same numeric tolerance, while edge IDs, source, and reason remain exact; raw diagnostic differences are recorded. Successful parsing always uses real PDFium and inference. The report server performs no parsing.

Fixture generators require reportlab, and the Chinese generator also requires pypdf and the pinned Noto font. Embedded-font licenses accompany the PDFs. Generators and acceptance reports are not production runtime dependencies.

## Structured tables

Final `table` blocks may include a `table` object with zero-based rows/columns, positive `row_span`/`column_span`, header flags, and cell-local lines. Header inference considers explicit tags, separator bands, and available font weight/style evidence. `table.source` identifies tagged PDF structure, vector-rule guidance, or text alignment. Plain text uses row-major tabs/newlines; ordinary single-header tables render as Markdown, while merged or multi-level headers render as escaped HTML. The configured JSON renderer retains the complete table structure even when generic evidence is hidden.

Set `config.tsr.batch_size` and `config.tsr.cell_detection.batch_size` independently
to cap structure and detector crops per ONNX invocation (1–32, default 1).
Ready requests from same-page tables can share a batch; partial batches run
immediately. `table_jobs` remains the admission limit, and the browser inference
guard still serializes different model runs through output readback.

Original text remains owned once by `block.lines[].text_items`. Cell lines contain non-owning references (`text_item_id`, UTF-8 `byte_range`, measured `bbox`). Do not use JavaScript string offsets directly with these byte ranges. Tagged empty cells may have a null bbox; populated tagged cells expose measured content bounds, while geometry-based cells expose inferred grid bounds. Validation checks occupancy, source coverage, byte boundaries, geometry, and cached text. Existing schema-2 documents without the optional structure remain readable.

The example displays a selected table in the text inspector with its rows and merged cells. Copy uses the table's plain-text projection. The page overlay stays one parent table region. Per-page `table_structure` measures the default local reconstruction stage; `text_finish` covers composition and final validation around that stage. `text_extract` includes the PDFium word, vector, and tag evidence scan.

Recovery is limited to already-detected table layouts. It uses existing native text or supplied OCR facts; the built-in TSR predicts structure, while OCR remains a separate extension. It does not invent missing scan text or merge tables across pages. Ambiguous grids retain source lines with `TableStructureUnavailable`. Sparse-rule and borderless reconstruction use geometry-based heuristics, so complex/irregular structures still require inspection.

## Caller-supplied table structure

Parsing uses the local SLANet_plus model by default. A per-call `onTableStructure`
callback can override it for regions already identified by layout. For callback-only
integrations, initialize with `config.tsr.mode = "rules_only"` to omit built-in
artifacts, then select `fallback` or `tsr_only` on the parse call.

```javascript
const document = await parser.parse(bytes, {
  table: { mode: "fallback", table_jobs: 2, timeout_ms: 60000 },
  onTableStructure: async (request, signal) => {
    // Invoke your own adapter, returning the original request ID and crop-pixel boxes.
    const result = await yourTableProvider(request.image.blob, { signal });
    return {
      request_id: request.request_id,
      structure_tokens: result.structure_tokens,
      cell_bboxes: result.cell_bboxes,
    };
  },
});
```

Use `tsr_only` to bypass local topology inference for all layout tables,
or `rules_only` to disable TSR. Without a callback, the built-in model handles
TSR when initialized; a missing built-in and missing callback fail explicitly. A failed provider retains the original text and emits table warnings;
external-only mode does not silently substitute a local structure.

The callback receives a PNG Blob, its actual width/height, one-based page number,
block/request IDs, canonical `crop_bbox`, `crop_to_viewport`, and the reason for
requesting external structure. The PNG has no UI overlays. Each returned box is
`[left, top, right, bottom]` or four perimeter vertices, in the original crop's
pixel coordinates. Undo any provider resize/padding before returning. Structural
tokens and spans are validated in Rust; the returned data cannot add page regions
or replace native text. Raster rounding can sample beyond the original block;
Rust trims that margin from decoded cells and keeps the block bounds unchanged.
Cells that become empty or cannot account for the original text reject only that
table's structure. Successful structures use `Table.source = "external_tsr"`.

The callback's AbortSignal is canceled on deadline, parse cancellation, or close.
Honor it in your adapter. Duplicate/late replies cannot settle another parse.
Callbacks remain on the calling thread; the private Worker control channel stays
responsive while Rust awaits a result. `table_rules`, `table_external`, and
`table_fill` timing events distinguish local attempts, external waits, and final
source population. Model/service accuracy must be evaluated separately from the
SDK's input and transport contracts.

After building the package and serving the example, run the supplied-input SDK
integration check from the repository root (installed Chrome is required):

```sh
rtk proxy node crates/web/tests/table_input.mjs /absolute/path/to/tables.pdf
```

Use a PDF containing multiple tables on one page and at least one locally
unresolved table. The check uses the production SDK and layout model to verify
fallback, external-only input, partial failures, timeout, and cancellation. Its
declared test topology evaluates the input contract, not a TSR model's accuracy.


Run real-model table acceptance after building the SDK and example and starting
the production example server:

```sh
rtk proxy node crates/web/tests/paddle_tsr.mjs /absolute/path/to/document.pdf
rtk proxy node crates/web/tests/paddle_tsr.mjs --mode fallback --output packages/wasm-web/test-results/paddle-tsr/browser-fallback.json /absolute/path/to/document.pdf
```

Reports distinguish layout tables, actual model calls, accepted structures, and
source-validation warnings. The default test requires model evidence in every
accepted table and never substitutes caller-supplied or local-rule results.

The JSON `table.source` identifies the accepted structure: `tagged_pdf`, `ruled`,
and `text_alignment` are local reconstruction; `external_tsr` is structure supplied
by the built-in TSR model or an external caller. The inspector labels these paths
Rules and TSR input, independently of the layout model that detected the region.

## OCR artifacts and recovery

Enable OCR explicitly in SDK callers (the WebUI already enables it):

```javascript
/** Resolves one pinned, self-hosted model triple. */
const modelSource = name => ({
  kind: "urls",
  model: `/models/${name}/inference.onnx`,
  config: `/models/${name}/inference.yml`,
  manifest: `/models/${name}/model-manifest.json`,
});
const parser = await createParser({
  artifacts: modelSource("pp-doclayout-v3"),
  tsrArtifacts: modelSource("slanet-plus"),
  tsrCellArtifacts: modelSource("rtdetr-table-cell-wireless"),
  ocrArtifacts: {
    detection: modelSource("pp-ocrv6-medium-det"),
    recognition: modelSource("pp-ocrv6-medium-rec"),
    orientation: modelSource("pp-lcnet-textline-ori"),
  },
  config: { ocr: { policy: "missing_regions" }, formula: { inline_enabled: false, display_enabled: false } },
  executionProvider: "webgpu",
});
```

Use `policy: "always"` to OCR every page or `"disabled"` to omit the OCR models.
`classify_orientation: false` permits omitting orientation artifacts. OCR paths
and backend overrides are rejected in Web configuration; use model sources and
the shared `executionProvider`. Buffer sources are copied before Worker transfer.

The built-in pipeline validates all three manifests, runs DB detection, rectifies
quadrilaterals, corrects upside-down lines, and decodes recognition probabilities
with the pinned CTC dictionary. Healthy native text is preserved. Replaced invalid
native facts are archived in `replaced_native_text`; canonical OCR facts carry
`source: "Ocr"`, confidence, polygon, rotation and estimated font size. OCR-only
word gaps are resolved before table byte ranges are assigned.

Additional timing stages are `ocr_detection_preprocess`, `ocr_detection_inference`,
`ocr_detection_postprocess`, `ocr_queue`, `ocr_orientation_inference`,
`ocr_recognition_preprocess`, `ocr_recognition_inference`, and `ocr_decode`.
Inference timings include output readback. Failed OCR produces page warnings;
initialization failure never masquerades as successful text recovery.


## Formula artifacts and output

The example exposes a **Formula model** selector: **Texo** (default),
**PP-FormulaNet**, or **MinerU · external service**. No local formula paths need to be entered. Changing the selection
closes the prepared parser and clears the previous result; prepare or parse again
to load the selected model. Independent inline/display switches are retained.

Selecting MinerU reveals **MinerU server URL** and **Concurrency** fields.
Changing either setting disposes the prepared parser so the next preparation
uses the new values. Formula crops are sent to the selected service; the PDF,
layout processing, and other local stages stay in the Worker. Local formula
models are neither downloaded nor loaded for MinerU.

```ts
const parser = await createParser({
  artifacts: layoutArtifacts,
  config: {
    tsr: { mode: "rules_only" },
    formula: {
      engine: { type: "mineru", server_url: "https://mineru.example.com/v1", concurrency: 8 },
      batch_size: 8,
      timeout_ms: 120000,
    },
  },
});
```

Use an absolute HTTP(S) service root or `/v1` base without credentials, query,
or fragment. Concurrency defaults to 8 (range 1–1024), shared by all calls through
the engine; it is independent of browser-local ONNX session limits.
The existing batch deadline includes admission and network time, aborts Fetch
on timeout, and releases request permits. The service must permit CORS from
the page origin; HTTPS pages need a service compatible with the browser's
mixed-content rules. For gpuhub, forward port 8000 over SSH and enter the local
forwarded URL when using the local example page.

Turning off both formula switches skips unused MinerU service settings, even if
the address or concurrency input was cleared. No formula requests or local
formula model downloads occur. Enabling either switch requires valid settings
again.

Validate the production browser UI and external service with a formula PDF:

```sh
rtk node crates/web/tests/mineru.mjs /absolute/path/to/formulas.pdf http://127.0.0.1:18000/v1
```

SDK callers can also select a preset without providing any formula paths:

```ts
const parser = await createParser({
  artifacts: layoutArtifacts,
  config: {
    tsr: { mode: "rules_only" },
    formula: { engine: { type: "texo" }, batch_size: 4, timeout_ms: 120000 },
  },
});
// Select PP with engine: { type: "pp" } when creating the next parser.
const document = await parser.parse(pdfBytes);
const markdown = await parser.render(document, "markdown");
```

The fixed presets fetch these same-origin assets:

| Selection | Resource directory | Files |
|---|---|---|
| `texo` | `/models/texo/` | `encoder_model.onnx`, `decoder_model_merged.onnx`, `tokenizer.json` |
| `pp` | `/models/pp-formulanet-plus-s/` | `inference.onnx`, `tokenizer.json`, `model-manifest.json` |

Provision the server-side files once; the example server exposes both routes:

```sh
rtk python3 crates/formula-texo/examples/download.py models/texo
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-s
```

For custom hosting or caller-owned memory, `formulaArtifacts` remains available:

```ts
formulaArtifacts: { type: "texo", kind: "bytes", encoder, decoder, tokenizer }
// URL override: { type: "texo", kind: "urls", encoder, decoder, tokenizer }
// PP override: { type: "pp", kind: "urls", model, tokenizer, manifest }
```

URLs resolve against the calling page. Byte arrays are copied before Worker
transfer, so the caller retains its buffers. Omitting `type` in the legacy PP
artifact shape remains supported. When an explicit `config.formula.engine.type`
is supplied, it must match the artifact type; mismatches fail with `InvalidConfig`.
Without explicit selection, a custom artifact's type selects its model. With
neither, Texo is the default. Filesystem path keys are rejected in Web config.

Only detected inline/display regions enter recognition; formula numbers remain
source text. Each `document.pages[].formulas[]` entry reports the actual engine,
LaTeX, Markdown, and source anchors. Failures retain their error and page warning.
`formula_queue`, `formula_preprocess`, `formula_inference`, and `formula_decode`
remain separate observations. GPU-resident Texo caches are preserved in WebGPU.

Run the actual SDK/preset/byte-transfer and selector regression with a formula PDF:

```sh
rtk node packages/wasm-web/tests/formula-engines.e2e.mjs /absolute/path/to/formulas.pdf
```

Inline formulas are rendered directly inside paragraph text using the optional
`block.markdown` projection produced by Rust. This preserves native UTF-8 span
mapping without duplicating byte-offset logic in JavaScript. Paragraph copy
returns Markdown source; per-formula LaTeX/Markdown copy remains in the collapsed
formula details. Invalid math leaves the surrounding prose visible. Original
`block.text` and `text_items[].raw_text` remain unchanged in JSON.

### Independent formula switches

`formula.inline_enabled` and `formula.display_enabled` both default to `true`.
They control recognition independently; neither is a master switch. For display
formulas only, use `config: { formula: { inline_enabled: false, display_enabled: true } }`.
Set both to `false` to omit formula artifacts and skip model loading. Disabled
kinds retain native text and layout evidence but do not produce recognized entries
in `pages[].formulas`. The example exposes separate Inline formulas and Display
formulas switches; changing either invalidates the prepared parser.

The former `formula.enabled` key has been removed. Replace `enabled: false` with
both switches set to false; replace `enabled: true` with both set to true or omit
them to use the defaults.
