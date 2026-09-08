# DocParse

DocParse is a Rust PDF parsing pipeline. PDFium supplies native text facts and page rendering, the pinned PP-DocLayoutV3 ONNX model detects layout regions, and `docparse-core` fuses both into a stable, validated `DocumentResult`. Residual XY-cut preserves text when the model misses regions or page-level layout inference fails.

Each validated model region produces one final block. Ownership is assigned at the `TextItem` boundary so partially overlapping visual lines cannot pull unrelated text into a model block. Unassigned text is partitioned by residual XY-cut before line assembly, so narrow column gutters are not swallowed as inline gaps. Lines are then rebuilt within each leaf and emitted with `label_source = "Fallback"`.

## Prepare the model

Models are distributed separately from the repository and crates. Download and verify the pinned revision using the dependency-locked uv script:

```bash
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3 --verify-only
```

The source is `PaddlePaddle/PP-DocLayoutV3_onnx` revision `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6`. Validation covers ONNX/YAML SHA-256 values, model schema, and the preprocessing contract.

## Configuration

The root `docparse.toml` is an example configuration. Select an execution provider available on your machine. Relative model paths resolve against the primary configuration directory. Precedence is: code defaults, primary TOML, explicit/`DOCPARSE_PROFILE` profile file, `DOCPARSE_...` environment variables, then explicit caller overrides.

Without `--config`, the CLI reads only `./docparse.toml` in the current directory and does not search parents. Library APIs do not load configuration files implicitly.

## Build and CUDA

CPU:

```bash
rtk cargo build -p docparse-cli
```

NVIDIA CUDA:

```bash
rtk cargo build -p docparse-cli --features layout-cuda
rtk docparse parse input.pdf --config docparse.cuda.toml --format json
```

CUDA configurations use `execution_provider = "cuda"`. A requested accelerator that cannot initialize fails explicitly. `layout-cuda`, `layout-coreml`, and `layout-openvino` are mutually exclusive; do not use `--all-features` for the provider matrix. Large models can retain several GiB per CUDA session, so size `session_pool_size` for the device.

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

`DocParserBuilder` accepts an `Arc<dyn LayoutEngine>` and optional `Arc<dyn OcrEngine>`. OCR is an extension interface; no OCR model is bundled. Synchronous callers can use `parse_path_blocking`; callers already inside Tokio must use the async API.

## WebAssembly and browsers

The `wasm32-unknown-unknown` build runs the same PDFium, model, preprocessing, and fusion algorithms in a dedicated module Worker. See the [Web package guide](packages/web/README.md) and [native/Web specification](docs/superpowers/specs/2026-09-07-native-web-wasm-design.md).

The shared Rust entry points are `DocParser::from_artifacts(config, ModelArtifacts)` and `parse_bytes(Arc<[u8]>)`. Filesystem and blocking APIs remain native capabilities. Cross-platform crates require the `wasm` feature for browser builds; `docparse-web` enables these dependency features directly. Native provider features cannot be combined with a Web target. Model bytes are checked against provenance, SHA-256, YAML, and tensor contracts.

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

`ValidatedConfig::try_from` checks shared parameters. ConfigLoader and native model entry points handle paths; explicit artifacts and injected engines need no placeholder absolute paths. Native async APIs require Tokio. Direct browser hosts must initialize ort-web and WASI in the same Worker; the Web SDK handles this setup.

Platform conditions are restricted to `wasm_compat.rs` and explicitly listed compatibility submodules. The browser-only `docparse-web` crate exports its API directly. Run `rtk proxy python3 scripts/check_wasm_compat.py` to check the boundary.

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

The workspace uses `members = ["crates/*"]`; `default-members` selects six native crates. Default checks use local fixtures without model downloads:

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
rtk uv run scripts/reference_layout.py \
  --model-dir models/pp-doclayout-v3 \
  --input-dir crates/layout/tests/fixtures/model \
  --output crates/layout/tests/fixtures/model/python_outputs.json
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
```

Real-PDF E2E preflight scans regular, case-insensitive PDF files at the top level of `~/Downloads`. The discovered basenames, sizes, SHA-256 values, and page counts must exactly match `tests/e2e-corpus.toml`. Adding, removing, or replacing a PDF fails preflight until the manifest is explicitly reviewed and updated. Full acceptance prohibits `--only`; that option is for smoke runs.

```bash
rtk uv run scripts/run_real_pdf_e2e.py \
  --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 \
  --execution-provider cuda --page-concurrency 1 --run-id serial
rtk uv run scripts/run_real_pdf_e2e.py \
  --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 \
  --execution-provider cuda --page-concurrency 4 --run-id parallel \
  --write-overlays
rtk proxy python3 scripts/compare_e2e_runs.py \
  target/docparse-e2e/serial/canonical-hashes.json \
  target/docparse-e2e/parallel/canonical-hashes.json
rtk uv run scripts/build_visual_review.py \
  --run-dir target/docparse-e2e/parallel
```

E2E builds release tests before timing their binaries directly. Use `--cargo-profile dev` only when diagnosing debug behavior. Performance and Cargo profile are recorded in `summary.json`, not used as cross-machine thresholds. Canonical hashes exclude timings, absolute paths, and host details.

## Initial scope

- Formula regions preserve location and content status without LaTeX recognition.
- Tables expose visual reading order without reconstructing complete cell structures.
- OCR requires a caller-provided implementation.
- User PDFs, models, rendered images, and generated E2E reports are excluded from Git and crate packages. Small generated regression PDFs and their font licenses are maintained as source fixtures.

## Provenance and licensing

DocParse uses Apache-2.0. PDFium, PP-DocLayoutV3, ONNX Runtime, development dependencies, and source derived from LiteParse have their own terms. Read [NOTICE](NOTICE) and the single root [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) before distributing artifacts.
