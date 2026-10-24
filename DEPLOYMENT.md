# Deployment guide

[English](DEPLOYMENT.md) | [简体中文](DEPLOYMENT.zh-CN.md) | [Project overview](README.md)

This guide follows the checked-in Cargo/npm manifests, build scripts, and `docparse.toml`. Run commands from the repository root unless a step says otherwise. Build on the target OS/architecture; the native server targets Linux/macOS. Models are downloaded separately and are not included in binaries or the WASM SDK.

## Choose the deployment

| | CLI | Web Server + HTTP workbench | WASM browser SDK |
| --- | --- | --- | --- |
| Rust package | `docparse-cli` | `docparse-server` + `docparse-core` worker | `docparse-web` |
| Frontend package | None | `packages/web` | `packages/wasm-web` |
| Parse location | Native process | Native workers; HTTP clients upload PDFs | Browser module Worker |
| Output | Local JSON/Markdown and optional overlays | Durable jobs, JSON/Markdown, SSE, web inspection | In-memory document, JSON/Markdown, browser previews |
| Model delivery | Files beside the configuration | Files on each parsing worker | URLs or caller-owned bytes |
| Backend choice | Cargo feature | Cargo feature | `executionProvider: "webgpu"` or `"wasm"` |
| PostgreSQL / shared storage | Neither required | Both required for every server role | Neither required |
| Deployable artifact | Executable + native runtime files + models/config | Two executables + runtime files + models/config + optional UI | Complete SDK `dist/` + enabled model assets + host page |

The HTTP workbench is a client of the server, so its PDFs leave the browser. The WASM example only serves static resources and parses PDFs locally. Configuring an HTTP formula engine sends formula images to an external service in both paths.

Read the sections in order: [tools](#prepare-tools), [models](#download-and-verify-models), [native backends](#native-inference-backends), then [CLI](#cli), [Web Server](#web-server-and-http-workbench), or [WASM](#wasm-browser-sdk).

## Prepare tools

| Tool/resource | Required for | Requirement |
| --- | --- | --- |
| Rust + Cargo | All Rust builds | Use the stable toolchain in `rust-toolchain.toml` and the committed `Cargo.lock`; crates use edition 2024 |
| Native linker / system runtime | Native builds | Linux build toolchain and C/C++ runtime, or macOS Xcode Command Line Tools |
| uv / Python | Model provisioning | `.python-version` selects Python 3.12; uv manages the root `pyproject.toml` and `uv.lock` |
| Node.js + npm | Workbench and WASM SDK builds | Node.js 22.13+; install each package from its committed npm lockfile |
| PostgreSQL | Web Server | Create an accessible database before startup; the application applies migrations |
| `wasm32-unknown-unknown` target | WASM SDK | Install through rustup |
| `wasm-bindgen-cli` | WASM SDK | Exactly `0.2.125`, matching the Rust dependency |
| Native accelerator libraries | CUDA/TensorRT/OpenVINO | Must match the ONNX Runtime distribution used by the build |

For a new checkout:

```sh
git clone https://github.com/YageGeng/docparse.git
cd docparse
rustup show
uv sync --locked
```

The default Python environment has no third-party dependencies. TATR's first download additionally runs an isolated, pinned CPU ONNX exporter via uv and needs network access for its Python dependencies. Model provisioning does not require installing PaddleOCR, Python ONNX Runtime, or CUDA into the root environment.

Native build scripts obtain pinned PDFium `chromium/8028` and ONNX Runtime when no local override is supplied. The Cargo-locked `ort 2.0.0-rc.15` distribution table uses native ONNX Runtime 1.31; browser packaging separately pins `onnxruntime-web 1.30.0`. Model downloads do not install either runtime. Initial builds need network access to dependency registries and runtime download hosts. Preserve the caches/artifacts when preparing an offline build.

PDFium defaults to pregenerated bindings: libclang is needed only if its optional `bindgen` feature is enabled. To provide an unpacked PDFium SDK at build time, set both `PDFIUM_LIB_PATH` and `PDFIUM_INCLUDE_PATH` to its `lib` and `include` directories. WASM builds verify the pinned static-library checksums and reject an incompatible SDK.

## Download and verify models

### TATR on macOS

The current `scripts/export_tatr.py` pins `torch==2.9.1+cpu` and
`torchvision==0.24.1+cpu` from the PyTorch CPU index. These pins have no macOS
wheels, so first-time TATR provisioning on macOS cannot resolve the exporter
environment. This does not prevent using an already exported, verified TATR model.

On a Linux export host, run from its checkout:

```sh
uv run --locked scripts/download_models.py --model tatr-v1.1-all --models-dir /srv/docparse/models
uv run --locked scripts/download_models.py --model tatr-v1.1-all --models-dir /srv/docparse/models --verify-only
```

Then copy the complete installation to your macOS checkout. Replace `export-host`
with your SSH host:

```sh
mkdir -p models
scp -r export-host:/srv/docparse/models/tatr-v1.1-all models/
uv run --locked scripts/download_models.py --model tatr-v1.1-all --verify-only
```

Subsequent provisioning skips the verified TATR files and does not invoke the
exporter. Alternatively, choose SLANet+ and its matching paths/artifacts explicitly
using the [TSR configuration examples](crates/tsr/README.md); the shipped native
config and browser example otherwise still request TATR.

### Models used by the shipped native configuration

Download these seven model installations to `models/`:

```sh
for model in pp-doclayout-v3 tatr-v1.1-all rtdetr-table-cell-wireless \
  pp-ocrv6-medium-det pp-ocrv6-medium-rec pp-lcnet-textline-ori texo
do
  uv run --locked scripts/download_models.py --model "$model" || exit 1
done
```

| Stage | Model directory | Installed runtime files |
| --- | --- | --- |
| Layout | `pp-doclayout-v3` | `inference.onnx`, `inference.yml`, `model-manifest.json` |
| Table structure | `tatr-v1.1-all` | Same three files |
| Table cells | `rtdetr-table-cell-wireless` | Same three files |
| OCR detection | `pp-ocrv6-medium-det` | Same three files |
| OCR recognition | `pp-ocrv6-medium-rec` | Same three files; YAML includes the recognition dictionary |
| OCR orientation | `pp-lcnet-textline-ori` | Same three files |
| Formulas | `texo` | `encoder_model.onnx`, `decoder_model_merged.onnx`, `tokenizer.json`, `model-manifest.json` |

Keep each installation intact; an ONNX file alone is insufficient. The downloader pins upstream revisions and SHA-256 hashes, skips verified files, repairs missing/corrupt artifacts, and publishes the manifest last. TATR is exported from a verified checkpoint on CPU; the exporter is unnecessary after provisioning.

Verify exactly this set without downloading anything:

```sh
for model in pp-doclayout-v3 tatr-v1.1-all rtdetr-table-cell-wireless \
  pp-ocrv6-medium-det pp-ocrv6-medium-rec pp-lcnet-textline-ori texo
do
  uv run --locked scripts/download_models.py --model "$model" --verify-only || exit 1
done
```

### Full catalog, alternative locations, and optional models

To provision all 14 registered models, including SLANet+/SLANeXt, wired cells, and PP-FormulaNet Plus S/M/L:

```sh
uv run --locked scripts/download_models.py
uv run --locked scripts/download_models.py --verify-only
```

The unqualified `--verify-only` checks the entire catalog. It will fail for intentionally absent alternatives if only the seven-model set was installed.

```sh
uv run --locked scripts/download_models.py --model texo --models-dir /srv/docparse/models
uv run --locked scripts/download_models.py --model pp-doclayout-v3 --output /srv/docparse/layout
uv run --locked scripts/download_models.py --model pp-formulanet-plus-s
```

`--models-dir` selects a model root; `--output` is allowed only with one explicit `--model` and selects that installation directory. Neither rewrites TOML paths. `--force` downloads every selected artifact again. The unified downloader already installs Texo; no second Texo download is needed.

Layout is always required. `tsr.mode = "rules_only"` skips table models; `ocr.policy = "disabled"` skips OCR. Setting both `formula.inline_enabled` and `formula.display_enabled` to `false` skips formula models. An HTTP formula engine requires a service instead of local formula weights. Keep the required queue settings even when disabling a stage.

The shipped native configuration selects TATR, `tsr_only`, automatic OCR, and Texo. Native library defaults select SLANet+ with rules-first fallback and OCR disabled; the WASM SDK defaults to TATR with rules-first fallback and OCR disabled. The browser example enables automatic OCR. See [TSR](crates/tsr/README.md) and [formula engines](crates/formula/README.md) when switching models.

## Native inference backends

Choose one accelerator family for both CLI and server builds. `embed-web` is independent and can be combined with a backend.

| Backend | Build feature | Deployment notes |
| --- | --- | --- |
| CPU | Omit accelerator features | No GPU required |
| NVIDIA CUDA | `cuda` | Matching NVIDIA driver; the locked automatic x86_64 Linux GPU distribution uses CUDA 13 and cuDNN 9 ([requirements](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider#requirements)) |
| NVIDIA TensorRT | `tensorrt` | Implies CUDA; additionally needs compatible TensorRT libraries and a writable `runtime.tensorrt_cache_dir` |
| Apple CoreML | `coreml` | Compatible macOS; formula inference explicitly uses CPU for model compatibility |
| Apple Metal | `metal` | CoreML with CPU/GPU compute units, without ANE; formulas use CPU |
| Intel OpenVINO | `openvino` | Supply a compatible ONNX Runtime build with OpenVINO; the locked automatic distribution table does not provide it |

Layout, OCR, TSR and formula share the compiled backend, subject to the documented CoreML/formula and TensorRT graph compatibility policies. Native TOML/env `execution_provider` overrides and model-prefixed backend features are rejected. CUDA, CoreML/Metal, and OpenVINO families are mutually exclusive; do not use `--all-features`. If an explicitly selected accelerator cannot initialize, startup fails.

Keep different native backends in separate `CARGO_TARGET_DIR` directories. ONNX Runtime libraries copied or symlinked into the output directory must belong to that build.

For example, a CUDA CLI build is:

```sh
CARGO_TARGET_DIR=target/cuda cargo build --locked --release -p docparse-cli --features cuda
./target/cuda/release/docparse parse input.pdf --config docparse.toml --output result.json
```

For OpenVINO or a custom CUDA distribution, provide the matching runtime during compilation, for example:

```sh
ORT_LIB_LOCATION=/opt/onnxruntime/lib ORT_PREFER_DYNAMIC_LINK=1 \
  CARGO_TARGET_DIR=target/openvino cargo build --locked --release \
  -p docparse-cli --features openvino
```

The runtime must expose the required native API (API 31 for these crates) and contain the requested provider. Install its provider dependencies and include its dynamic libraries in the deployment. Merely installing a Python `onnxruntime` wheel does not configure this Rust build.

TensorRT runs supported layout, OCR, table and Texo encoder graphs, with CUDA fallback for unsupported parts; Texo's merged decoder, PP-FormulaNet and SLANet+/SLANeXt remain on CUDA. The first startup builds engines and can take minutes. Later starts reuse `runtime.tensorrt_cache_dir`. Rebuild caches after changing GPU model, ONNX Runtime or TensorRT. The generated provider library's dependencies determine the required CUDA/cuDNN/TensorRT ABI; inspect them with `ldd` on Linux instead of copying a version from an old benchmark. See the official [CUDA requirements](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider#requirements) and [TensorRT requirements and cache guidance](https://onnxruntime.ai/docs/execution-providers/TensorRT-ExecutionProvider.html#requirements).

## CLI

### Build and parse

The CPU baseline requires the downloaded models, but no database or Node.js:

```sh
cargo build --locked --release -p docparse-cli
./target/release/docparse --help
./target/release/docparse inspect-model --config docparse.toml
./target/release/docparse parse input.pdf --config docparse.toml \
  --format json --output result.json
./target/release/docparse parse input.pdf --config docparse.toml \
  --format markdown --view semantic --output result.md
```

The package is `docparse-cli`; the executable is `docparse`. `cargo build` does not put it on `PATH`. `target/release/` is the default output directory; use your configured Cargo target directory when overridden.

`--output` writes atomically; add `--force` to replace an existing file. Omitting `--output` writes to stdout. `--overlay-dir diagnostics` adds per-page PNG/SVG diagnostics. `--continue-on-error true|false` overrides recoverable page continuation; fatal document/transport errors still stop parsing.

For a PATH installation:

```sh
cargo install --path crates/cli --locked
```

Add your chosen backend feature to either build/install command. Installing an executable does not install models or make the build machine's PDFium cache portable. Follow [native packaging](#package-native-artifacts) before moving it to another machine.

## Web Server and HTTP workbench

### 1. Prepare PostgreSQL, storage, and configuration

Create the PostgreSQL database and a role with permission to apply application migrations. There is no automatic PostgreSQL installation or database creation. Pending SeaORM migrations run when the server connects, before accepting jobs; the separate [migration CLI](crates/migration/README.md) is optional.

Use `DOCPARSE_DATABASE__URL` from the service environment/secret manager to override the sample `database.url`. Supply it to every server role. The credentials in `docparse.toml` are local examples. Each replica has its own database pool; size the total against PostgreSQL capacity.

Choose a writable storage directory, such as `/srv/docparse/shared`. Multiple API/worker replicas must see the same files, even if mount paths differ. Storage needs coherent reads, atomic publication, file/directory synchronization, and cross-client file locking. Separate container-local directories are insufficient.

### 2. Build the workbench, then both native executables

The shipped configuration selects `server.webui = "embedded"`. Build the frontend before compiling the release server:

```sh
npm ci --prefix packages/web
npm run build --prefix packages/web
cargo build --locked --release -p docparse-core \
  --features pdfium-ipc --bin docparse-pdfium-worker
cargo build --locked --release -p docparse-server --features embed-web
```

Deliver both `target/release/docparse-server` and `target/release/docparse-pdfium-worker` in the same directory. They must come from the same source revision/protocol. Building only the server package does not build the worker executable, and there is no in-process fallback.

For CUDA, use a separate directory and the same backend when building both:

```sh
CARGO_TARGET_DIR=target/cuda cargo build --locked --release -p docparse-core \
  --features pdfium-ipc,cuda --bin docparse-pdfium-worker
CARGO_TARGET_DIR=target/cuda cargo build --locked --release -p docparse-server \
  --features embed-web,cuda
```

Substitute `coreml`, `metal`, `tensorrt` or `openvino` as appropriate, retaining `pdfium-ipc` for the worker and `embed-web` for an embedded workbench. The TensorRT/custom-runtime prerequisites above also apply to these builds.

### 3. Start and check

After providing the database URL in the environment, start the combined API/worker:

```sh
./target/release/docparse-server --config docparse.toml \
  --storage-dir /srv/docparse/shared --role all
```

For the CUDA example, run `./target/cuda/release/docparse-server` instead.

The checked-in configuration listens on `127.0.0.1:8080` with `server.api_prefix = "/api/v1/docparse"`. Code defaults use `"/api"`; all commands here use the checked-in prefix:

```sh
curl --fail http://127.0.0.1:8080/api/v1/docparse/health
curl --fail http://127.0.0.1:8080/api/v1/docparse/ready
curl --fail http://127.0.0.1:8080/api/v1/docparse/openapi.json --output openapi.json
```

Open the workbench at `http://127.0.0.1:8080/api/v1/docparse/webui/` and Scalar at `http://127.0.0.1:8080/api/v1/docparse/docs`. Readiness checks database/schema and storage; it does not prove a real PDF has completed successfully. Scalar's default page loads JavaScript from jsDelivr.

Upload a real PDF with a new UUID, save it, and reuse it when retrying the same upload:

```sh
job_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
curl --fail -i -H "Idempotency-Key: $job_id" \
  -F 'file=@input.pdf;type=application/pdf' \
  http://127.0.0.1:8080/api/v1/docparse/jobs
curl --fail "http://127.0.0.1:8080/api/v1/docparse/jobs/status?id=$job_id"
curl --fail -N "http://127.0.0.1:8080/api/v1/docparse/jobs/events?id=$job_id"
```

The upload should return `202`. Wait for `succeeded` (or inspect `failed`), then stop the SSE client and download results:

```sh
curl --fail "http://127.0.0.1:8080/api/v1/docparse/jobs/result?id=$job_id" \
  --output result.json
curl --fail "http://127.0.0.1:8080/api/v1/docparse/jobs/result?id=$job_id&format=markdown" \
  --output result.md
```

Ordinary JSON business responses wrap data in `ApiResponse`; CLI/WASM documents are direct `DocumentResult` values. SSE reconnects do not cancel durable jobs. Keep ingress upload/SSE streaming and compression headers intact.

### Workbench delivery and development

| Mode | Configuration/build | Runtime files |
| --- | --- | --- |
| Embedded | `webui = "embedded"`; build `packages/web` first, then server with `embed-web` | Release server contains UI; no workbench `dist/` required at runtime |
| Disk | Replace that value with `webui = { disk = "packages/web/dist" }`; build frontend, server without `embed-web` | Complete frontend directory, readable `index.html`; relative directory resolves beside the config |
| API only, no UI | Remove `server.webui`; omit `embed-web` | No workbench assets |

Use only one `webui` value. A debug embedded build still reads its build-time frontend directory; the self-contained UI behavior applies to release builds.

The browser build's `VITE_API_PREFIX` must match `server.api_prefix`. Copy `packages/web/.env.example` to `packages/web/.env.local` if you need overrides, set the prefix before building, and rebuild the frontend and embedded server after changing it. `VITE_API_TARGET` controls the development proxy, not a production backend URL.

For UI development, first run the configured native backend, then in another terminal:

```sh
npm run dev --prefix packages/web
```

Open `http://127.0.0.1:5173/api/v1/docparse/webui/`. The Vite server is a development frontend, not the production parser.

### Split API and worker roles

Use separate terminals or service units, with the same database and shared storage:

```sh
./target/release/docparse-server --config docparse.toml \
  --storage-dir /srv/docparse/shared --role api
./target/release/docparse-server --config docparse.toml \
  --storage-dir /srv/docparse/shared --role worker
```

`api` exposes HTTP and does not load models or require the worker executable. `worker` consumes queued jobs and needs models and the sibling PDFium worker, but has no HTTP listener. `all` does both. API-only readiness can succeed while no parsing worker is available, so verify a completed job.

`DOCPARSE_SERVER__HOST=0.0.0.0` exposes the listener when needed behind ingress. Provide TLS, authentication, authorization and any browser CORS policy there. Job UUIDs are identifiers; task history is shared across the deployment, not tenant-isolated. On SIGTERM/Ctrl-C the server drains and shuts down PDFium children; allow time for cleanup.

## WASM browser SDK

### 1. Install the target and pinned tools

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.125 --locked
npm ci --prefix packages/wasm-web --ignore-scripts
```

### 2. Build the complete SDK

```sh
npm run build --prefix packages/wasm-web
npm run check --prefix packages/wasm-web
npm run check:example --prefix packages/wasm-web
```

The build script runs Cargo for `docparse-web` in release mode, wasm-bindgen, the npm-pinned Binaryen `wasm-opt -O4`, TypeScript, import validation, and runtime/license packaging. It explicitly uses the repository's `target/` directory. No native accelerator feature, Emscripten SDK, or system `wasm-opt` is required.

A raw `cargo build -p docparse-web --target wasm32-unknown-unknown` produces only the Rust artifact; it does not create a deployable browser SDK. Deploy the complete `packages/wasm-web/dist/`, preserving its relative structure and `build-manifest.json`. It contains the SDK/Worker, Rust WASM, ORT Web JS/WASM, WASI adapter, and licenses. Models remain separate.

### 3. Run the local example

After downloading the needed models and building the SDK:

```sh
npm run example --prefix packages/wasm-web
```

Open `http://127.0.0.1:8768/example/`. The server binds to loopback and serves static resources; it needs no PostgreSQL or native server. `PORT` changes its port. The example defaults to automatic OCR, TATR and Texo; the optional PP formula selector needs `pp-formulanet-plus-s`. The example allows CPU fallback and shows the actual backend.

### 4. Host assets and integrate the SDK

For your application, copy the full SDK to a static directory such as `/assets/docparse/` and expose enabled models at the URLs you pass to the SDK. Serve JavaScript/MJS as JavaScript and WASM as `application/wasm`. Cross-origin resources need CORS. WebGPU needs a secure context (HTTPS or localhost) and compatible browser/device; explicit single-threaded `"wasm"` inference needs no cross-origin isolation.

This minimal example intentionally uses layout plus local table rules, with OCR/formulas disabled. It needs only `pp-doclayout-v3`; `file` is a caller-selected browser `File`:

```javascript
import { prepareModels } from "/assets/docparse/index.js";

const parser = await prepareModels({
  artifacts: {
    kind: "urls",
    model: "/models/pp-doclayout-v3/inference.onnx",
    config: "/models/pp-doclayout-v3/inference.yml",
    manifest: "/models/pp-doclayout-v3/model-manifest.json",
  },
  executionProvider: "wasm",
  config: {
    render: { workers: 1, queue_size: 2 },
    layout: { queue_size: 1 },
    tsr: { mode: "rules_only", queue_size: 1, cell_detection: { queue_size: 1 } },
    ocr: {
      policy: "disabled",
      detection: { queue_size: 1 },
      recognition: { queue_size: 1 },
      orientation: { queue_size: 1 },
    },
    formula: { queue_size: 1, inline_enabled: false, display_enabled: false },
  },
});
try {
  const document = await parser.parse(new Uint8Array(await file.arrayBuffer()));
  const markdown = await parser.render(document, "markdown");
  console.log(document.pages.length, markdown);
} finally {
  await parser.close();
}
```

Use `executionProvider: "webgpu"` to request acceleration. The SDK's `allowCpuFallback` defaults to `false`; opt in explicitly when GPU unavailability may fall back to CPU. Reuse a prepared parser across PDFs and call `close()` when disposing it. Recreate it when model/provider settings change.

For full parsing, supply `tsrArtifacts`, `tsrCellArtifacts` and enabled `ocrArtifacts`. Local formula presets use `/models/texo/` or `/models/pp-formulanet-plus-s/`; supply `formulaArtifacts` to change their URLs. Native filesystem paths, profiles and environment overrides are not accepted. Examples for all model sources are in the [SDK guide](packages/wasm-web/README.md).

When deploying the included example unchanged, preserve `/example/` (its HTML, CSS and built `example/dist/`), `/dist/` (the SDK), and its model routes. The example's layout URLs are the special `/models/inference.onnx`, `/models/inference.yml` and `/models/model-manifest.json`, mapped to `models/pp-doclayout-v3/` by `serve-example.mjs`; other model URLs use `/models/<name>/`. Mirror those mappings or update the host page's URLs. Copying only `example/dist/` is insufficient.

ORT Web still serializes physical inference/readback through a global guard. Additional model sessions do not promise parallel GPU runs, and large PDFs/models remain subject to browser memory limits. An HTTP formula engine requires suitable CORS and HTTPS/mixed-content handling and sends formula crops outside the browser.

## Configuration and resource limits

Native precedence is defaults → primary TOML → selected profile → `DOCPARSE_...` environment → explicit caller/CLI overrides. The CLI reads only `./docparse.toml` when `--config` is omitted. Relative model, log and cache paths resolve beside the primary config, so copying a config elsewhere without its paths/models changes what gets loaded. `--storage-dir` is a separate server process option; use an absolute deployment path.

| Required setting | Meaning |
| --- | --- |
| `render.workers` | Native PDFium process-pool size; WASM must use 1 |
| `render.queue_size` | Unfinished pages across documents, held through actual cleanup |
| `layout.queue_size` | Pending layout inputs |
| `tsr.queue_size`, `tsr.cell_detection.queue_size` | Pending structure/cell inputs |
| `ocr.detection.queue_size`, `ocr.recognition.queue_size`, `ocr.orientation.queue_size` | Pending OCR inputs |
| `formula.queue_size` | Pending formula crops shared by engine consumers |

Every capacity must be positive, including queues for disabled stages. `session_size` controls independent model sessions; local formula engines use `formula.engine[].worker_size`. `batch_size` caps already-ready inputs and does not wait to fill a batch. More sessions duplicate model memory. Start with one session/consumer per local model on a small host, then measure your real PDFs before increasing concurrency.

The shipped native config uses five PDFium workers, multiple model sessions, and four Texo consumers; these are example throughput settings, not minimum hardware requirements. `server.max_uploads` independently limits simultaneous uploads (100 in the file, 4 in code defaults), not parsing concurrency. Native document admission is bounded by `render.workers + render.queue_size`; completed results still scale with PDF size.

Use `runtime.optimization_level` and `runtime.memory_pattern` for global ONNX settings. `runtime.continue_on_error` controls recoverable page continuation. Legacy fields such as `server.jobs`, `server.pdfium_workers`, `runtime.stage_pages`, `sessions` and `formula.enabled` are rejected.

Server logs use `log.directives` and optional `log.file`; the shipped file appends to `logs/docparse.log` beside the config. Make that destination writable. CLI diagnostics use `RUST_LOG` and stderr. Inspect startup/model errors and page warnings as well as exit status. See [common runtime](crates/common/README.md) and [server](crates/server/README.md) for detailed lifetime, retry and error behavior.

## Package native artifacts

Models and native libraries are not embedded by `embed-web`. The default native ONNX Runtime core is statically linked, but provider libraries emitted alongside the binaries must be distributed; a custom dynamically linked runtime also needs its core library.

A Linux server bundle can be prepared from the CPU release build:

```sh
mkdir -p release/docparse/bin release/docparse/lib
cp target/release/docparse-server target/release/docparse-pdfium-worker release/docparse/bin/
cp -L target/release/deps/libpdfium.so release/docparse/lib/
for library in target/release/*.so*
do
  [ -e "$library" ] || continue
  cp -L "$library" release/docparse/lib/
done
```

For a CLI bundle, copy `target/release/docparse` instead of the two server executables. For CUDA/TensorRT use the corresponding target directory. `cp -L` copies library bytes rather than symlinks into build-machine caches. On macOS package `libpdfium.dylib` from `target/release/deps/` and any emitted `.dylib` files instead.

Install models/config/notices and arrange the server deployment, for example:

```text
/srv/docparse/
  bin/docparse-server
  bin/docparse-pdfium-worker
  lib/                         # PDFium and emitted provider/runtime libraries
  models/                      # Complete enabled model installations
  docparse.toml
  shared/                      # Persistent PDF/result storage
  logs/
  cache/tensorrt/               # If using TensorRT
  webui/                       # Only when serving a disk build
```

Retain the root and bundled third-party notices. Model paths `models/...` work when the config is `/srv/docparse/docparse.toml`. For disk UI, change `server.webui.disk` to the installed directory.

Set `PDFIUM_LIB_PATH=/srv/docparse/lib` in the service environment. Add that directory and required GPU/OpenVINO dependency directories to the OS library search path (`LD_LIBRARY_PATH` on Linux, `DYLD_LIBRARY_PATH` where applicable on macOS). PDFium otherwise searches its baked build-cache path, sibling files and system paths; a working checkout does not prove a moved bundle works.

Start the installed executable with absolute config/storage paths. Use `ldd` on Linux or `otool -L` on macOS to inspect runtime dependencies, including emitted provider libraries. Verify on the deployment host without relying on the build machine's caches. Keep PostgreSQL and shared storage when replacing binaries, and update server/worker together.

## Deployment verification and troubleshooting

| Check/symptom | Action |
| --- | --- |
| Missing model or manifest/hash failure | Verify the selected model with `--verify-only`; check paths relative to the config; rerun the downloader to repair |
| TATR exporter has no compatible macOS wheel | Provision TATR on Linux and copy the complete verified installation, or explicitly select another supported TSR model |
| CLI smoke | Run `inspect-model`, then parse a real PDF to JSON and Markdown; inspect warnings/errors |
| Server smoke | Check health/ready, upload a real PDF, observe `succeeded`, retrieve both formats, refresh the workbench to confirm persisted history |
| Missing/incompatible PDFium worker | Build `docparse-core` with `pdfium-ipc`; install matching worker beside the server |
| `server.webui` initialization error | Build frontend first; match `embedded` with `embed-web` or provide a readable disk `index.html` |
| Workbench assets/API return 404 | Match built `VITE_API_PREFIX`, server prefix, and proxy routes; rebuild after changing them |
| Jobs remain queued | Confirm a worker role is running against the same database/storage; API-only readiness is insufficient |
| Native provider fails / library missing | Check emitted runtime dependencies and selected feature; install matching libraries or rebuild for CPU |
| GPU memory exhaustion | Reduce session/consumer counts, batches and queued inputs before raising concurrency |
| WASM load/import error | Deploy the entire SDK `dist/`; keep pinned wasm-bindgen/ORT versions, relative files and MIME types |
| Browser model URL returns HTML | Fix the static model mapping; do not use SPA fallback for model/JS/WASM asset requests |
| WebGPU unavailable | Use a supported secure context/device or explicitly select `"wasm"`/authorize CPU fallback |

For browser acceptance, use a real PDF and the production SDK/native server with its configured provider. Do not substitute a fixture parsing backend. The [HTTP workbench](packages/web/README.md), [server](crates/server/README.md) and [WASM SDK](packages/wasm-web/README.md) guides contain the existing acceptance commands. A successful build or readiness probe alone is not end-to-end acceptance.
