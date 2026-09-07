# DocParse Web

Run Rust DocParse, PDFium, and the pinned PP-DocLayoutV3 model inside a dedicated module Worker. PDFs and models are processed locally in the browser, producing the same `DocumentResult` schema as native builds without a parsing server.

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

`dist/` contains the ES module API, Worker, Rust WASM, ORT 1.27.0 assets, WASI adapter, and licenses. `build-manifest.json` records versions, file SHA-256 values, PDFium library checksums, and final WASM imports. The build verifies the pinned PDFium chromium/8028 libraries and real setjmp runtime; arbitrary replacement SDKs are rejected.

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

## Lifecycle

- `createParser` resolves only after fixed artifact validation and actual model initialization.
- Each parser accepts one parse/render operation at a time; concurrent calls fail with `ParserBusy`.
- `parse(bytes, {signal})` accepts an AbortSignal. An already-aborted request leaves a ready instance usable. Active cancellation terminates the Worker and requires a new `createParser` call.
- `close()` is idempotent, rejects pending work, and makes the instance unusable.
- Fatal Worker/WASM failures settle pending requests instead of leaving Promises suspended.
- Errors contain a stable `code` and readable message; Worker error stacks are retained for diagnostics.

Set `{executionProvider: "webgpu"}` to request WebGPU explicitly. Fallback is disabled by default. Only `allowCpuFallback: true` permits CPU fallback when GPU capability or provider initialization is unavailable. Model hash/schema and inference-data failures do not trigger fallback. See the [validation report](../../docs/superpowers/reports/2026-09-07-native-web-wasm-validation.md) for the tested matrix; CPU acceptance does not establish GPU support.

## Native/Web result parity

Native font selection is preserved. Embedded-font PDFs provide strict parity fixtures. For unembedded fonts, host substitution can change glyph metrics, rendered images, confidence scores, coordinates, and derived IDs. Record these differences separately while checking text preservation and deterministic results within each environment.

The model is approximately 124.46 MiB. The browser also holds Rust/PDFium, ORT, image, and tensor buffers. A single WASM memory capacity is not total process memory, and arbitrary document sizes are not guaranteed. Inference requests only boxes and count; the unused mask is not returned.

## Real-browser acceptance

From the repository root, generate native references and start the static/report server:

```sh
rtk proxy env DOCPARSE_WEB_REFERENCE_DIR=packages/web/test-results/native rtk cargo test -p docparse-core --test web_reference -- --ignored --nocapture
rtk proxy python3 packages/web/tests/serve.py --port 8767
```

Open `/packages/web/tests/browser.html?cycles=20&relocated=1&run=cpu-stress`. The page uses the production build, real model, and PDF fixtures. `relocated=1` checks deployment under a renamed directory. Reports are saved to `packages/web/test-results/cpu-stress-<run-id>.json`, including parity, actual fetches, live tensors, WASM capacities, and cancellation/close results.

Use `provider=webgpu` for a separate GPU run. `provider=webgpu&fallback=1&noGpu=1` disables GPU capability only in the test Worker and validates explicit fallback with the real CPU model. Diagnostic edge weights use the same numeric tolerance, while edge IDs, source, and reason remain exact; raw diagnostic differences are recorded. Instrumentation observes the real runtime and does not replace PDFium or inference. The report server performs no parsing.

Fixture generators require reportlab, and the Chinese generator also requires pypdf and the pinned Noto font. Embedded-font licenses accompany the PDFs. Generators and acceptance reports are not production runtime dependencies.
