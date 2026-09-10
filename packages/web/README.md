# DocParse Web

Run Rust DocParse, PDFium, and the pinned PP-DocLayoutV3 model inside a dedicated module Worker. By default, PDFs and models are processed locally in the browser, producing the same `DocumentResult` schema as native builds without a parsing server.

## Build

Prepare the pinned model and tools from the repository root:

```sh
rtk rustup target add wasm32-unknown-unknown
rtk cargo install wasm-bindgen-cli --version 0.2.125 --locked
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3
```

Run these commands in `packages/web`:

```sh
rtk npm ci --ignore-scripts
rtk npm run build
```

`npm run build` uses Cargo's release profile, runs wasm-bindgen, then runs the pinned npm Binaryen 132.0.0 `wasm-opt -O4`. No system `wasm-opt` installation is required. Optimization preserves SIMD, bulk memory, reference types, and PDFium exception handling; invalid output fails the build. The manifest records optimization flags, before/after byte sizes, elapsed optimization time, and the hash of the optimized artifact. ORT's own prebuilt WASM files are copied unchanged.

`dist/` contains the ES module API, Worker, Rust WASM, ORT 1.27.0 assets, WASI adapter, and licenses. `build-manifest.json` records versions, file SHA-256 values, PDFium library checksums, and final WASM imports. The build verifies the pinned PDFium chromium/8028 libraries and real setjmp runtime; arbitrary replacement SDKs are rejected.

The SDK build excludes the example. `src/types.ts` contains public data and option contracts; `src/protocol.ts` contains the private typed Worker messages. The example lives under `example/src`, consumes the built public SDK, and emits only to `example/dist`. `rtk npm run check` checks the SDK; `rtk npm run check:example` checks the example after the SDK has been built.

Copy the complete `dist/` directory to any static-site directory while preserving internal relative paths. Deploy the ONNX model, inference.yml, and model-manifest.json separately. Serve correct JavaScript/WASM MIME types and permit CORS for cross-origin model/runtime resources. The default single-threaded CPU/WASM path requires no cross-origin isolation. ORT telemetry is disabled. WebGPU additionally requires a supported secure context and compatible browser/device.

## Usage

```javascript
import { createParser } from "/assets/docparse/index.js";

const parser = await createParser({
  artifacts: {
    kind: "urls",
    model: "/models/pp-doclayout-v3/inference.onnx",
    config: "/models/pp-doclayout-v3/inference.yml",
    manifest: "/models/pp-doclayout-v3/model-manifest.json",
  },
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

Here, `file` is a caller-selected File. To manage authentication or caching, obtain the model yourself and pass `{kind: "bytes", model, config, manifest}` with three Uint8Array values. The library does not detach caller-owned PDF or model buffers.

`config` uses native business groups and snake_case fields, such as `{render: {dpi: 144}, layout: {score_threshold: 0.5}}`. Filesystem paths, profiles, environment variables, and layout.execution_provider are excluded. All four concurrency settings must equal 1 in the initial Web implementation; invalid settings fail explicitly.

`runtimeBaseUrl` can select a self-hosted ORT directory containing the same JS/mjs/wasm versions as the build manifest. Relative model URLs resolve against the calling page. Default runtime resources follow the SDK deployment location.

## Stage timings

Both `createParser({ onTiming })` and `parser.parse(bytes, { onTiming })` accept a callback with `{stage, page_number, duration_ms}`. Page numbers are one-based; `null` denotes document-wide work. Timings never enter `DocumentResult`, so native/Web comparison and stored JSON stay deterministic. Rust callers use `ParseObserver::on_timing`; `RUST_LOG=debug` also reports elapsed stages.

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
| `text_prepare`, `ocr`, `text_finish` | Native text/layout preparation, an actual OCR call when needed, then final text/layout composition. |
| `link_validate` | Cross-page linking, result assembly, and validation. |
| `result_serialize`, `preview_encode` | Rust result conversion to JavaScript; each Worker PNG encoding operation, including asynchronous waiting. |
| `parse_total`, `worker_total` | Inclusive Rust parse time; inclusive Worker initialization or parse time, respectively. Worker parse time includes serialization and preview completion, but excludes request/result transfer and UI rendering. |

All values use monotonic wall clocks, including `performance.now()` in the dedicated Worker. Concurrent stages overlap, and totals include nested work: **do not add all stage durations to compute elapsed parse time**. The example's status timer additionally includes main-thread setup, file reading, and message delivery. Initializing a model and parsing a document are separate callback scopes. The first real inference can include lazy runtime/GPU compilation; no explicit dummy warmup run is added.

Observations describe elapsed attempts, not success. Error/cancellation can leave a partial set of events; stages that never run have no record. SDK callbacks are isolated from parser failures and scoped to the active request. Rust callbacks are delivered serially by the parse future, potentially after a stage has ended.

## Interactive example

The example selects WebGPU by default and explicitly allows CPU fallback on unsupported devices. Its layout engine selector also supports CPU/WASM, releasing the old Worker when changed. The engine status shows the initialized backend, including CPU fallback, rather than only the requested preference.

After preparing the model and building the package, run this command in `packages/web`:

```sh
rtk npm run example
```

This compiles `example/src/main.ts` through its own TypeScript configuration and starts the static server. Use `rtk npm run build:example` to build the example without starting a server. Changes to the UI do not require rebuilding Rust/WASM.

Open <http://127.0.0.1:8768/example/>. Choose a local PDF, then select **Parse document**. The example displays actual model-download, text-extraction, and page-analysis progress. Browse the PDFium page thumbnails, zoom the page, and toggle overlays without changing the parsed geometry. Click an overlay, or use the region menu, to inspect and copy its text. On narrow screens the selected text opens in a dismissible floating inspector.

The workspace fills the remaining viewport height and fits the entire PDF page by
default. The introduction collapses after file selection. Zoom percentages are relative
to this fitted size; click **Fit entire page** to reset both scale and scroll position.
Window resizing refits the image and overlays together. Thumbnails, enlarged pages, and
long extracted text scroll within their own panels. Exported PNG resolution is unchanged.

**Stage timings** opens a snapshot with counts, cumulative time, mean, and first measurement for each stage. Initialization is listed only when this run created a Worker; reusing a model does not count as new initialization. The dialog preserves the fitted workspace height.

**Export PNG** opens the generated image for inspection; **Save PNG** then downloads it. Some embedded browsers cancel file downloads, but the image remains available in the preview. Cancel stops the Worker; the next parse creates a fresh parser. A ready model is reused when choosing another PDF. Returning through browser history does not revoke a cached document's preview URLs.

The static server exposes only the example, built SDK, and model directory. It does not receive PDF uploads or perform parsing. Set `PORT` to use another local port. The example uses PDFium's inference raster and SVG hit targets; it has no pdf.js dependency. Native PDF text is extracted; image-only and outlined text still require OCR.

### UI end-to-end acceptance

The unified entry builds the SDK and example, regenerates native references, starts owned servers on free ports, and runs SDK acceptance, UI flows, and export-race checks against the real model:

```sh
rtk npx playwright install chromium
rtk npm run test:e2e
```

Use `-- --headed` to watch the command-line browser, `-- --channel chrome` to use an installed Chrome, or `-- --cycles 20` for the longer SDK repeat matrix. Reports and the final screenshot are written to `test-results/e2e/`. Playwright is a development-only E2E dependency; there are no frontend unit tests.

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
  artifacts,
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

- `createParser` resolves only after fixed artifact validation and actual model initialization.
- Each parser accepts one parse/render operation at a time; concurrent calls fail with `ParserBusy`.
- `parse(bytes, {signal})` accepts an AbortSignal. An already-aborted request leaves a ready instance usable. Active cancellation terminates the Worker and requires a new `createParser` call.
- `close()` is idempotent, rejects pending work, and makes the instance unusable.
- Fatal Worker/WASM failures settle pending requests instead of leaving Promises suspended.
- Errors contain a stable `code` and readable message; Worker error stacks are retained for diagnostics.

Set `{executionProvider: "webgpu"}` to request WebGPU explicitly. The WASM dependency graph enables `ort/webgpu`; the host loads the pinned WebGPU distribution and registers its execution provider. The SDK retains its CPU default for compatibility. PDFium rendering, text extraction, and fusion still run on CPU.

Fallback is disabled by default. Only `allowCpuFallback: true` permits CPU fallback when GPU capability or provider initialization is unavailable. Model hash/schema and inference-data failures do not trigger fallback. The read-only `parser.executionProvider` reports the initialized backend, including `"wasm"` after fallback. ORT may still execute unsupported operators on CPU within a WebGPU session. See the [ONNX Runtime WebGPU guide](https://onnxruntime.ai/docs/tutorials/web/ep-webgpu.html).

Run `rtk npm run test:e2e -- --provider webgpu` for strict GPU acceptance. The real-model test records actual GPU queue submissions and inference durations in addition to provider registration. A GPU request with no observed GPU commands fails acceptance.

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
remain separate. This does
not provide LaTeX or structured formula recognition. Unmatched inline
formula detections produce diagnostics instead of independent empty content blocks.

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
rtk proxy env DOCPARSE_WEB_REFERENCE_DIR=packages/web/test-results/native rtk cargo test -p docparse-core --test web_reference -- --ignored --nocapture
rtk proxy python3 packages/web/tests/serve.py --port 8767
```

Open `/packages/web/tests/browser.html?cycles=20&relocated=1&run=cpu-stress`. The page uses the production build, real model, and PDF fixtures. `relocated=1` checks deployment under a renamed directory. Reports are saved to `packages/web/test-results/cpu-stress-<run-id>.json`, including parity, actual fetches, live tensors, WASM capacities, and cancellation/close results.

Use `provider=webgpu` for a separate GPU run. `provider=webgpu&fallback=1&noGpu=1` disables GPU capability only in the test Worker and validates explicit fallback with the real CPU model. Replace `noGpu=1` with `failGpuInit=1` to reject GPU session creation at the ORT boundary; omit `fallback=1` to require an explicit failure. Diagnostic edge weights use the same numeric tolerance, while edge IDs, source, and reason remain exact; raw diagnostic differences are recorded. Successful parsing always uses real PDFium and inference. The report server performs no parsing.

Fixture generators require reportlab, and the Chinese generator also requires pypdf and the pinned Noto font. Embedded-font licenses accompany the PDFs. Generators and acceptance reports are not production runtime dependencies.

## Structured tables

Final `table` blocks may include a `table` object with zero-based rows/columns, positive `row_span`/`column_span`, header flags, and cell-local lines. Header inference considers explicit tags, separator bands, and available font weight/style evidence. `table.source` identifies tagged PDF structure, vector-rule guidance, or text alignment. Plain text uses row-major tabs/newlines; ordinary single-header tables render as Markdown, while merged or multi-level headers render as escaped HTML. The configured JSON renderer retains the complete table structure even when generic evidence is hidden.

Original text remains owned once by `block.lines[].text_items`. Cell lines contain non-owning references (`text_item_id`, UTF-8 `byte_range`, measured `bbox`). Do not use JavaScript string offsets directly with these byte ranges. Tagged empty cells may have a null bbox; populated tagged cells expose measured content bounds, while geometry-based cells expose inferred grid bounds. Validation checks occupancy, source coverage, byte boundaries, geometry, and cached text. Existing schema-2 documents without the optional structure remain readable.

The example displays a selected table in the text inspector with its rows and merged cells. Copy uses the table's plain-text projection. The page overlay stays one parent table region. Per-page `table_structure` measures the default local reconstruction stage; `text_finish` covers composition and final validation around that stage. `text_extract` includes the PDFium word, vector, and tag evidence scan.

Recovery is limited to already-detected table layouts. It uses existing native text or supplied OCR facts; it does not add a table/OCR model, infer missing scan text, or merge tables across pages. Ambiguous grids retain source lines with `TableStructureUnavailable`. Sparse-rule and borderless reconstruction use geometry-based heuristics, so complex/irregular structures still require inspection.

## Caller-supplied table structure

Existing parsing remains local by default. A per-call `table` option enables an
external structure callback for regions the production layout model already
identified as tables. This SDK does not ship a table recognition service.

```javascript
const document = await parser.parse(bytes, {
  table: { mode: "fallback", max_in_flight: 2, timeout_ms: 60000 },
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

Use `external_only` to bypass local topology inference for all layout tables,
or `rules_only` to disable the callback. Either external mode requires the
callback. A failed provider retains the original text and emits table warnings;
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
