# DocParse

DocParse is a Rust PDF parsing pipeline. PDFium supplies native text facts and page rendering, the pinned PP-DocLayoutV3 ONNX model detects layout regions, SLANet_plus predicts table structures, PaddleOCR recovers scanned text, and `docparse-core` fuses both into a stable, validated `DocumentResult`. Residual XY-cut preserves text when the model misses regions or page-level layout inference fails.

The native [HTTP server](crates/server/README.md) accepts durable PDF jobs and
provides JSON results and reconnectable SSE progress. It uses SeaORM 2.0,
PostgreSQL, and a shared file directory, with separate migration, database, and
server crates. Layout, OCR, and TSR have independent bounded page stages; native
OCR can overlap two pages by default through `ocr.max_in_flight`.

Model regions are candidates rather than a one-to-one final block contract. Ownership is assigned at the `TextItem` boundary, with short, unambiguous superscripts/subscripts attached to their parent before model assignment; residual XY-cut preserves column gutters before line assembly. Page-wide normalization merges content only when one bbox fully contains the other, including identical boxes, then computes reading order. Every native text fact remains owned exactly once.

`reference` is an empty visual annotation: it never owns text, obstructs XY-cut, merges with content, or enters body reading order. Bibliography text uses `reference_content`, including recovered fragments. Partial content intersections remain separate regardless of IoU and produce a `ContentLayoutOverlap` page warning with per-pair `content.overlap.*` diagnostics. Reference outlines and detached watermarks are exempt. Merged layouts retain their primary `source_region` plus all contributing `source_regions`, whose optional `label` preserves original model semantics.

## Prepare the model

Models are distributed separately from the repository and crates. Download and verify the pinned revision using the dependency-locked uv script:

```bash
rtk uv run --locked scripts/download_models.py
rtk uv run --locked scripts/download_models.py --verify-only
```

The source is `PaddlePaddle/PP-DocLayoutV3_onnx` revision `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6`. Validation covers ONNX/YAML SHA-256 values, model schema, and the preprocessing contract.

The default command provisions layout, table models, OCR and PP-FormulaNet_plus-S/Plus-L
with their matching tokenizer in the repository's `models/` directory. Verified local
files are skipped; missing or corrupt files are downloaded and verified before
publication. Use `--model slanet-plus` for one model, `--models-dir /path/to/models`
for another root, or `--model pp-doclayout-v3 --output /path/to/layout` for an exact
single-model directory. All Python tools use the root `pyproject.toml`, `.python-version`
and `uv.lock`; see [scripts/README.md](scripts/README.md) for retained tools and dependency groups.

## Configuration

The root `docparse.toml` is an example configuration. Native inference backends are selected by Cargo features, with CPU as the default. Relative model paths resolve against the primary configuration directory. Precedence is: code defaults, primary TOML, explicit/`DOCPARSE_PROFILE` profile file, `DOCPARSE_...` environment variables, then explicit caller overrides.

Without `--config`, the CLI reads only `./docparse.toml` in the current directory and does not search parents. Library APIs do not load configuration files implicitly.

The default model combination is SLANet+ with wireless RT-DETR cell detection.
The root configuration uses `tsr.mode = "tsr_only"` to send every table to TSR.
Library defaults retain `"fallback"`: local rules run first and unresolved tables
use that same model combination. Select `"tsr_only"` for every table,
or `"rules_only"` to retain local behavior without loading TSR artifacts. Layout,
OCR and TSR share the compiled native backend. Structured
output keeps the existing `external_tsr` source value and records the engine in
evidence; a model prediction is accepted only after topology and source validation.
`table.source` distinguishes local `tagged_pdf`, `ruled`, and `text_alignment`
reconstruction from `external_tsr` model/caller input. The Web inspector displays
this as Rules or TSR input.

Use `--profile tsr-baseline` (or `tsr.cell_detection.enabled = false`) for pure
SLANet+, and `--profile tsr-upgraded` to compare SLANeXt wireless plus cell detection.

## Build and CUDA

CPU:

```bash
rtk cargo build -p docparse-cli
```

NVIDIA CUDA:

```bash
rtk cargo build -p docparse-cli --features layout-cuda,tsr-cuda
rtk docparse parse input.pdf --config docparse.toml --format json
```

Native layout, OCR and TSR use one backend selected by Cargo features; TOML and environment `execution_provider` overrides are rejected. Model-prefixed CLI features forward to the shared backend, so enabling `layout-cuda` also selects CUDA for OCR and TSR. Server builds expose `cuda`, `coreml`, `metal`, and `openvino`; omit accelerator features for CPU. Metal uses CoreML with CPU/GPU compute units, without ANE. An enabled accelerator that cannot initialize fails explicitly. CUDA, CoreML/Metal, and OpenVINO features are mutually exclusive; do not use `--all-features`. Large models can retain several GiB per CUDA session, so size `session_pool_size` for the device.

CoreML and Metal sessions request `FastPrediction` specialization for their reusable models. The [M4 benchmark report](docs/reports/2026-09-15-coreml-performance/README.md) records warmed real-PDF measurements and the compatibility and output checks for alternative settings.

### Formula recognition

Enable `[formula] enabled = true` to recognize every existing inline/display formula detection with PP-FormulaNet_plus-S by default. `batch_size` defaults to 4 and `timeout_ms` to 120000 per batch, including queueing. Install its pinned graph/tokenizer with `rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-s`. Formula numbers remain native text.

JSON includes `pages[].formulas[]`, with `latex`, `markdown`, the actual `engine`, source geometry, exact UTF-8 `text_spans`, and non-owning block/line/cell anchors. Failures retain an explicit `error` with null representations and a page warning. Markdown replaces the relevant presentation range while original text items and table spans remain available in JSON. The HTTP result endpoint accepts `?id=<uuid>&format=markdown`; omitting `format` returns JSON with both formula representations.

The pinned Plus-L export did not reliably support repeated CoreML inference on this M4. Both supported formula variants retain the explicit compatibility policy: CoreML/Metal builds run **formula recognition on CPU**, with real tensor batches, and log the compatibility choice. Layout, OCR and table models keep their selected backend. Formula initialization and inference failures are explicit; no formula confidence is fabricated. See [the formula module](crates/formula/README.md) for the artifact and validation details.

## Rust API

```rust,no_run
use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_core::DocParser;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let raw = ConfigLoader::new("docparse.toml").load_raw()?;
let parser = DocParser::from_config(ValidatedConfig::try_from(raw)?).await?;
let document = parser.parse_path("input.pdf").await?;
# Ok(())
# }
```

`DocParserBuilder` accepts an `Arc<dyn LayoutEngine>`, optional `Arc<dyn OcrEngine>`, and optional `Arc<dyn TableStructureEngine>`. An injected table engine overrides built-in model loading. Per-call `ParseOptions.table` is optional and inherits the configured policy when absent. OCR is an extension interface; no OCR model is bundled. Synchronous callers can use `parse_path_blocking`; callers already inside Tokio must use the async API.

## WebAssembly and browsers

The `wasm32-unknown-unknown` build runs the same PDFium, model, preprocessing, and fusion algorithms in a dedicated module Worker. See the [Web package guide](packages/wasm-web/README.md) and [native/Web specification](docs/superpowers/specs/2026-09-07-native-web-wasm-design.md).

The shared Rust entry points are `DocParser::from_artifacts(config, ParserArtifacts::builder().layout(layout).tsr(Some(tsr)).build())` and `parse_bytes(Arc<[u8]>)`. Enabled model families use the supplied bytes without reading configured paths. Rules-only parsers may still pass a single layout `ModelArtifacts`. Filesystem and blocking APIs remain native capabilities. Cross-platform crates require the `wasm` feature for browser builds; `docparse-web` enables these dependency features directly. Native provider features cannot be combined with a Web target. Model bytes are checked against provenance, SHA-256, YAML, and tensor contracts.

Custom LayoutEngine/OcrEngine implementations must return `WasmBoxedFuture` instead of using async_trait. This preserves native Send/Sync bounds while allowing local browser futures:

```rust,ignore
use docparse_layout::{LayoutEngine, LayoutRequest, LayoutDetection, LayoutError, WasmBoxedFuture};

impl LayoutEngine for MyEngine {
    // name() and model_revision() retain their existing signatures.
    /// Detects one page using the implementation's actual model.
    fn detect(&self, request: LayoutRequest) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async move { self.detect_page(request).await })
    }
}
```

`ValidatedConfig::try_from` checks shared parameters. ConfigLoader and native model entry points handle paths; explicit artifacts and injected engines need no placeholder absolute paths. Browser hosts inject both layout and table engines from artifacts, or select `rules_only` to omit the table model. Native async APIs require Tokio. Direct browser hosts must initialize ort-web and WASI in the same Worker; the Web SDK handles this setup.

Platform conditions are restricted to `wasm_compat.rs` and explicitly listed compatibility submodules. The browser-only `docparse-web` crate exports its API directly. Run `rtk uv run --locked scripts/check_wasm_compat.py` to check the boundary.

## CLI

```bash
rtk docparse parse INPUT.pdf --config docparse.toml --format json
rtk docparse parse INPUT.pdf --format text --view semantic
rtk docparse parse INPUT.pdf --format markdown --output result.md --force
rtk docparse parse INPUT.pdf --overlay-dir diagnostics
rtk docparse inspect-model --config docparse.toml
```

JSON preserves the complete schema, evidence, warnings, and relations. Text/Markdown semantic views suppress repeated page furniture only at presentation time. Overlays reopen the PDF serially to produce PNG/SVG files without rerunning ONNX inference.

## Watermarks and rotated bounds

Watermarks are detached before body statistics, layout assignment, XY-cut, paragraph
assembly, formula attachment, and reading-order constraints. They remain independent
`watermark` blocks at the end of each page; text and Markdown retain their text.
The derived label has no PP-DocLayoutV3 class index. Original text facts remain uniquely
owned and carry `watermark` source metadata plus rule evidence when applicable.

PDFium content marks and Watermark annotations take precedence over conservative
cross-page/paint/geometry rules. A generic `Artifact` mark is not sufficient. The pinned
PDFium API can read string-valued mark parameters but cannot expose PDF Name values
such as `/Subtype /Watermark`; those cases require the fallback rules. Annotation
`Contents` is not appearance text, so a watermark annotation without extracted native
text produces a region rather than invented text.

`Quad` validates four convex, ordered corners. Native text contours come from PDFium's
`FPDFPageObj_GetRotatedBounds`, with nested form and page transforms applied. Results
keep a conservative `bbox` and an optional precise `polygon`; overlays and hit testing
prefer the polygon. Raster inputs remain unchanged, so model detections can still be
visually affected by a watermark even though its recognized text is isolated from fusion.

## Tests

The workspace uses `members = ["crates/*"]`; `default-members` selects the native crates. Default checks use local fixtures without model downloads:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --locked
rtk cargo clippy --locked --all-targets -- -D warnings
```

Explicit `--workspace` includes the browser-only crate. Add `--exclude docparse-web` for native workspace commands. Build Web separately:

```bash
rtk cargo build -p docparse-web --target wasm32-unknown-unknown --release --locked
```

Model parity:

```bash
rtk uv run --locked --group reference scripts/reference_layout.py \
  --model-dir models/pp-doclayout-v3 \
  --input-dir crates/layout/tests/fixtures/model \
  --output crates/layout/tests/fixtures/model/python_outputs.json
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
```

Real-PDF E2E preflight scans regular, case-insensitive PDF files at the top level of `~/Downloads`. The discovered basenames, sizes, SHA-256 values, and page counts must exactly match `tests/e2e-corpus.toml`. Adding, removing, or replacing a PDF fails preflight until the manifest is explicitly reviewed and updated. Full acceptance prohibits `--only`; that option is for smoke runs.

```bash
rtk uv run --locked --group dev scripts/run_real_pdf_e2e.py \
  --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 \
  --execution-provider cuda --page-concurrency 1 --run-id serial
rtk uv run --locked --group dev scripts/run_real_pdf_e2e.py \
  --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 \
  --execution-provider cuda --page-concurrency 4 --run-id parallel \
  --write-overlays
rtk uv run --locked scripts/compare_e2e_runs.py \
  target/docparse-e2e/serial/canonical-hashes.json \
  target/docparse-e2e/parallel/canonical-hashes.json
```

E2E builds release tests before timing their binaries directly. Use `--cargo-profile dev` only when diagnosing debug behavior. Performance and Cargo profile are recorded in `summary.json`, not used as cross-machine thresholds. Canonical hashes exclude timings, absolute paths, and host details.

## Frontend applications

Run frontend commands from the repository root, selecting the package with
`--prefix`. Keep each development server in its own terminal.

The [HTTP workbench](packages/web/README.md) uploads PDFs to the native server and
inspects persisted results. Start the configured backend in another terminal,
then run:

```sh
rtk npm ci --prefix packages/web
rtk npm run dev --prefix packages/web
```

The [WASM browser example](packages/wasm-web/README.md) parses PDFs locally in the
browser. Complete the tools and model setup in its guide, then run:

```sh
rtk npm ci --prefix packages/wasm-web --ignore-scripts
rtk npm run build --prefix packages/wasm-web
rtk npm run example --prefix packages/wasm-web
```

The workbench opens at <http://127.0.0.1:5173>; the WASM example opens at
<http://127.0.0.1:8768/example/>. The package guides list build, type-check,
preview, and acceptance commands with the same repository-root convention.

## Initial scope

- Optional formula recognition provides LaTeX and Markdown for detected inline and display regions while preserving original source text.
- User PDFs, models, rendered images, and generated E2E reports are excluded from Git and crate packages. Small generated regression PDFs and their font licenses are maintained as source fixtures.

## Provenance and licensing

DocParse uses Apache-2.0. PDFium, PP-DocLayoutV3, ONNX Runtime, development dependencies, and source derived from LiteParse have their own terms. Read [NOTICE](NOTICE) and the single root [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) before distributing artifacts.

## Built-in PaddleOCR

`docparse-ocr` implements DB detection/unclip, perspective rectification, line
orientation, recognition and CTC decoding without an OAR dependency. Native and
Web use the same algorithms and pinned PP-OCRv6 medium models. Download all three:

```sh
rtk uv run --locked scripts/download_models.py
```

Enable native OCR in your TOML configuration:

```toml
[ocr]
policy = "missing_regions" # disabled (library default), missing_regions, or always
classify_orientation = true
recognition_threshold = 0.5
timeout_ms = 120000

[ocr.detection]
model_path = "models/pp-ocrv6-medium-det/inference.onnx"
model_config_path = "models/pp-ocrv6-medium-det/inference.yml"
model_manifest_path = "models/pp-ocrv6-medium-det/model-manifest.json"

[ocr.recognition]
model_path = "models/pp-ocrv6-medium-rec/inference.onnx"
model_config_path = "models/pp-ocrv6-medium-rec/inference.yml"
model_manifest_path = "models/pp-ocrv6-medium-rec/model-manifest.json"

[ocr.orientation]
model_path = "models/pp-lcnet-textline-ori/inference.onnx"
model_config_path = "models/pp-lcnet-textline-ori/inference.yml"
model_manifest_path = "models/pp-lcnet-textline-ori/model-manifest.json"
```

The CLI exposes `ocr-cuda`, `ocr-coreml`, `ocr-metal`, and `ocr-openvino`
features. Select one optional native provider per build, or omit them for CPU.
CoreML requires a compatible macOS runtime; `metal` selects CoreML's CPU/GPU
compute units. Browser WebGPU/CPU is selected through `executionProvider` for
all model families. Model contracts reject unknown weights or dictionaries.

Healthy native text wins over overlapping OCR. Confident OCR may replace only
strongly invalid native mappings; original facts remain in
`PageResult.replaced_native_text` for auditing and native-ID conservation.
OCR retains its quadrilateral, rotation, confidence and estimated font size;
line, paragraph and table composition use the ordinary parser pipeline.
The WebUI defaults to automatic OCR; the SDK and native library default to off.
