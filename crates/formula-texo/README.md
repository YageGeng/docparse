# docparse-formula-texo

Texo formula recognition implementing `docparse_formula::FormulaEngine`.
The pinned author-published FP32 encoder and merged decoder use the transferred
687-token WordLevel vocabulary. Inputs are RGB formula crops; outputs are LaTeX
strings in the same order. Both inline and display formulas use core's existing
crop, batching, timeout, ownership, and rendering policies.

## Native configuration

From the workspace root, download the verified artifacts explicitly:

```sh
rtk python3 crates/formula-texo/examples/download.py models/texo
```

Configure the existing formula section:

```toml
[formula]
inline_enabled = true
display_enabled = true
batch_size = 4
timeout_ms = 120000

[formula.engine]
type = "texo"
encoder_path = "models/texo/encoder_model.onnx"
decoder_path = "models/texo/decoder_model_merged.onnx"
tokenizer_path = "models/texo/tokenizer.json"
```

Core selects Texo only through `formula.engine.type = "texo"`. All three paths
are explicit and may use arbitrary filenames. Texo has no manifest-path field:
its SHA-256 hashes and source revision are pinned in this crate. The `pp` variant
owns `model_path`, `tokenizer_path`, and `model_manifest_path`. Flat paths are
rejected, and switching variants in a profile or environment resets stale paths.
Explicitly injected engines remain authoritative; byte artifacts must match the
configured engine when core selects the recognizer.

## Features and browser loading

`default = []` selects native CPU. `metal`, `coreml`, `cuda`, and `openvino`
forward to `docparse-layout` and register the same provider for both sessions.
As in layout, `metal` means CoreML with CPU/GPU compute units. Registration errors
are propagated; the crate does not force a CPU compatibility executor. ORT can
still partition unsupported graph operators onto CPU, so a registered provider
does not prove that every operator was accelerated.

`wasm` supports `wasm32-unknown-unknown`. It additionally forwards
`docparse-formula/wasm`, because that crate owns the shared formula trait.
The browser host initializes ORT Web and selects WASM CPU or WebGPU through
`ValidatedConfig::with_webgpu`. Supply `TexoArtifacts { encoder, decoder,
tokenizer }` to `TexoEngine::from_artifacts` and inject the engine, or set
`ParserArtifacts::texo_formula`. Do not supply both PP-FormulaNet and Texo artifacts.
Native filesystem loading is unavailable in browsers. The JS SDK accepts
`config.formula.engine.type` and loads preset resources automatically; callers may
override them with `formulaArtifacts: { type: "texo", kind: "urls" | "bytes",
encoder, decoder, tokenizer }`. The example has a Texo/PP selector.
`examples/browser/main.rs` also demonstrates direct Rust/WASM loading.

## Execution contract

- A batch contains 1..32 crops and runs one encoder invocation followed by
  batched cached decoder steps. Finished rows are padded until the batch finishes.
- The first decoder step computes cross-attention caches. Later steps preserve
  them and replace only self-attention caches; no cache is shared across requests.
- Runtime tensor handles are retained between steps rather than extracting KV
  buffers into Rust. WebGPU sessions request `gpu-buffer` for hidden features and
  caches, and `cpu` for logits; Rust synchronizes only logits. CUDA binds hidden
  features and caches to device memory, returning only logits
  to CPU. Dynamic cache outputs receive fresh bindings at each step to avoid
  overwriting still-live inputs. Their allocation device is validated at runtime.
- Native calls use the existing bounded `SessionWorker`. Dropping a pending call
  terminates its ORT run. The browser actor skips canceled requests and checks
  cancellation between decoder steps while retaining the global inference guard.
- Queue, preprocessing, inference, and tokenizer time use core's existing stages.
- No synthetic EOS is inserted. A sequence reaching 1024 tokens without EOS fails
  rather than returning truncated LaTeX as a successful recognition.
- Preprocessing preserves upstream Pillow/OpenCV rounding and bounds crop area
  and intermediate resizing allocations. It does not add the web demo's optional
  dark-background inversion heuristic.

## Validation and timing

```sh
rtk cargo test -p docparse-formula-texo -- --include-ignored
rtk cargo test -p docparse-core --lib parser::tests -- --include-ignored
rtk cargo run -p docparse-formula-texo --example recognize --release -- models/texo 10 crates/formula-texo/tests/fixtures/formula_single.png
```

The native example separates initialization from one warmup batch, then reports
warm P50/P95 batch latency, throughput, and per-stage means. Pass multiple images
to measure a real batch. It verifies repeated outputs are stable. Model tests use
`models/texo` or `DOCPARSE_TEXO_MODELS`; default unit tests need no model downloads.

Regenerate independent reference outputs when intentionally changing model bytes:

```sh
rtk uv run --no-project --python 3.12 --with onnxruntime --with pillow python crates/formula-texo/tests/reference.py models/texo
```

Run browser model integration with the workspace's existing Playwright and
self-hosted ORT distribution:

```sh
rtk cargo build -p docparse-formula-texo --example browser --target wasm32-unknown-unknown --features wasm --release
rtk wasm-bindgen target/wasm32-unknown-unknown/release/examples/browser.wasm --target web --out-dir target/texo-browser
rtk node crates/formula-texo/tests/browser.mjs wasm
rtk node crates/formula-texo/tests/browser.mjs webgpu
```

This crate and its adapted preprocessing are AGPL-3.0-only. See `NOTICE.md` and
`LICENSE` for provenance. The ORT graphs are separate model artifacts.

### Verification snapshot (2026-09-17)

On Apple M4, CPU, Metal, and CoreML passed the real-model batch/singleton/cancellation
tests. CPU preprocessing matched independently generated Pillow tensors byte for
byte. Chrome 153 passed real-model WASM CPU and WebGPU single/batch inference.
CUDA and OpenVINO were compile-checked only; their hardware execution is not verified.

Initial release probes of `formula_single.png` (53 tokens including BOS/EOS,
FP32, batch 1, one warmup) reported CPU P50 79 ms over five measurements and
Metal P50 48 ms over three. These small, separately collected samples are smoke
measurements, not a representative backend benchmark; initialization was excluded.

The upstream encoder metadata declares four output positions although a 384x384
input produces 144. Some ORT distributions log a shape warning; generation uses
runtime tensor handles rather than allocating against that stale metadata.

The GPU-residency regression asserts actual output and next-step input locations
in Chrome, including the cached decoder branch. Native I/O-binding ownership is
also tested with real batched model inference on CPU. CUDA compilation and the
device-location checks are covered, but CUDA hardware execution remains unverified
on this Apple host.
