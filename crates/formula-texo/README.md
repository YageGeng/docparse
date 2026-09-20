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
queue_size = 8
inline_enabled = true
display_enabled = true
timeout_ms = 120000

[[formula.engine]]
batch_size = 8
type = "texo"
worker_size = 2
encoder_path = "models/texo/encoder_model.onnx"
decoder_path = "models/texo/decoder_model_merged.onnx"
tokenizer_path = "models/texo/tokenizer.json"
```

Core selects Texo only through `formula.engine[].type = "texo"`. All three paths
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
`config.formula.engine[].type` and loads preset resources automatically; callers may
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
  caches, and `cpu` for logits; Rust synchronizes only logits. Native sessions use
  standard `run_with_options` calls without I/O Binding or explicit device-wide
  synchronization. Hidden features, logits, and KV outputs return to host memory;
  ORT manages transfers to the selected accelerator on subsequent calls.
- Native calls share a bounded crop queue across pages and documents. Each of
  `formula.engine[].worker_size` independent owners holds its own encoder, decoder,
  tokenizer, and execution thread. An idle owner drains ready crops up to
  `formula.engine[].batch_size`; it never waits to fill a batch. Queue capacity is
  `formula.queue_size` crops, in addition to active batches. Parser crop admission is shared across pages
  and bounded to `queue_size + sum(worker_size * batch_size)`. Results return in each caller's original order.
  Ready crops from different pages and documents may share a model batch. Canceled crops do not consume
  its batch slots, and unconsumed tails still count toward queue capacity.
- Native defaults are one owner per configured group on the selected backend, with no fixed upper limit. Every extra
  session duplicates model/runtime resources. Browser sessions share a bounded
  per-crop ready queue under the global inference guard.
- Core submits crops independently with a shared pre-crop admission budget and
  replenishes work as each crop completes, instead of waiting for page chunks.
- Cancellation skips queued crops and pads canceled rows at decoder boundaries.
  It cannot terminate another caller sharing the batch; a fully canceled batch
  stops before its next decoder invocation. Initialization failures close and join
  any owners already created, and final shutdown joins all model threads.
  Unexpected owner exit closes admission and releases queued replies and blocked
  producers rather than leaving them waiting on an abandoned queue.
- Queue, preprocessing, inference, and tokenizer time use core's existing stages.
  Native observations are per crop and retain the originating page and document;
  shared inference time is attributed to each participating crop, so summed
  timings are not GPU busy time.
- No synthetic EOS is inserted. A sequence reaching 1024 tokens without EOS fails
  rather than returning truncated LaTeX as a successful recognition. A malformed
  input or unfinished sequence fails only its originating call; completed crops
  from other calls in the same native batch still succeed. Model-wide execution
  errors fail every affected call.
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
in Chrome, including the cached decoder branch. Native standard runs are covered
by real batched and repeated model inference tests. CUDA hardware execution must
be checked on a CUDA host; CPU parity alone does not validate GPU performance.

## Formula consumers

`formula.engine[].worker_size` selects the number of encoder/decoder owners
(default 1, positive, with no fixed upper limit). Every owner uses the compiled
native accelerator or the browser-selected backend and drains the same bounded
queue. Native standard runs return hidden states and KV caches to host memory.
Outputs retain caller order even if consumers complete out of order.

Separate CPU/GPU consumer counts and per-CPU-consumer thread settings are removed.
CPU-only builds still use their compiled CPU backend. Native sessions leave
intra-op threading at the ORT default. Queue capacity, global ORT optimization/memory
settings, cancellation, and the shared formula batch limit remain independent of the count.
