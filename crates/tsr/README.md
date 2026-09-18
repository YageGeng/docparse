# docparse-tsr

Paddle SLANet+ / SLANeXt structure recognition and optional RT-DETR cell detection for layout-owned table crops.
Models stay external. Layout and TSR reuse the same ONNX execution-provider
registration: CPU, CUDA, CoreML, OpenVINO, and browser WebGPU. Native layout, OCR
and TSR share the backend selected by Cargo features, defaulting to CPU. Enable
one of `cuda`, `coreml`, `metal`, or `openvino`; no runtime provider configuration
is accepted. `metal` uses CoreML with `CPUAndGPU` compute units (no separate Metal EP).
Native sessions specialize each graph to its actual input contract before
initialization: 488 pixels for SLANet+, 512 for SLANeXt, and 640 for RT-DETR,
including CoreML shape inference.
WASM defaults to WebGPU, and the Web SDK applies its selected backend to all
models. Unsupported operators may still execute on CPU inside an accelerated
session; unavailable requested providers fail explicitly.

```sh
rtk uv run --locked scripts/download_models.py
rtk cargo test -p docparse-tsr -- --include-ignored
rtk cargo test -p docparse-tsr --features metal --test inference -- --include-ignored
```

`PaddleTsrEngine::from_artifacts(Arc<ValidatedConfig>, TsrArtifacts)` validates the pinned model,
YAML, and manifest before creating a session. Native `from_config` reads resolved
`TsrConfig` paths. `predict(Arc<PageImage>, Timings)` returns tokens, crop-pixel
cell boxes, and structure confidence. The input is packed RGB; preprocessing
converts to BGR, preserves aspect ratio at a 488-pixel long edge, normalizes, and
pads with zero in normalized space. A complete EOS-terminated result is required.
Incomplete or degenerate predictions retry on image segments split at visible
separators or blank scanlines, with depth capped at three. Every segment must
complete; crop coordinates are restored before concatenation, and continuation
segments cannot introduce another column-header section.

Artifact: [PaddlePaddle/SLANet_plus_onnx](https://huggingface.co/PaddlePaddle/SLANet_plus_onnx/tree/7dbe640e127602bf506815e822c09758de73c482),
Apache-2.0, revision `7dbe640e127602bf506815e822c09758de73c482`. Preprocessing and
vocabulary follow its inference.yml and the [PaddleX reference implementation](https://github.com/PaddlePaddle/PaddleX/tree/develop/paddlex/inference/models/table_structure_recognition).

Each model owns `session_size` independent consumers (1–8, default 1), configured
separately as `tsr.session_size` and `tsr.cell_detection.session_size`. They consume
one shared queue per model. `tsr.batch_size` and
`tsr.cell_detection.batch_size` independently cap ready crops per ONNX invocation
(1–32, library default 1; the repository config uses 4 for each). Both native and
WASM runners combine queued requests and immediately run partial batches without
waiting to fill them. The batch axis stays dynamic; detector box counts split
outputs back into the original request order and each crop keeps its own scale.
The former `tsr.table_jobs` setting is removed. Ready tables are submitted
concurrently across pages and documents. Each model queue holds at most
its required `queue_size` pending inputs, in addition to active batches; a full
queue waits for capacity rather than dropping work. Crops and tensors held by
callers waiting to enqueue are outside that queue capacity. Per-request inference
timings include the shared batch interval and must not be summed as distinct
model execution time.

Native model threads initialize, batch, and destroy their own sessions independently
of caller Tokio runtimes. Native cancellation retains the session owner until
blocking inference finishes; the browser actor owns inputs until the
ORT Promise settles. Layout and TSR share a browser inference guard through
output readback because ORT WebGPU reuses download buffers across sessions.
A caller deadline cannot release the guard before the actual model run finishes.
No model URL, authentication state, or generated PDF text is stored in this crate. The core adapter retains independent learned positions,
reconciles inconsistent token spans, and calibrates the grid against native/OCR
word geometry and visible separators. Word ownership remains explicit, complete,
and source-preserving even when learned content rectangles overlap.
A high structure confidence is not a guarantee of correct table semantics.

## Structure and cell comparison

The repository configuration uses `tsr_only`. The former `external_only` spelling
is no longer accepted. Library defaults retain rules-first fallback.
Both use SLANet+ with wireless RT-DETR cell detection by default.
`[tsr].model` selects `slanet_plus`, `slanext_wired`, or `slanext_wireless`.
SLANeXt uses a 512-pixel input and its invalid position head is discarded.
Its structure tokens require an independent cell detector.

`[tsr.cell_detection]` accepts `enabled` (default `true`), `model = "wired"` or `"wireless"`, a
`score_threshold`, required `queue_size`, `session_size`, `batch_size`, and independent `model_path`, `model_config_path`, and
`model_manifest_path` values. RT-DETR reuses layout's OpenCV-compatible cubic
RGB preprocessing at 640 pixels and returns crop-pixel boxes. The core decoder
matches those observations to logical cells before source-text filling.

The default config includes the former `tsr-cells` combination.
`SlanetPlusEngine` remains available as the
original public name; `PaddleTsrEngine` names the expanded implementation.

Rust callers constructing `ParserArtifacts` directly use `TsrArtifacts` with
both independently verified model sets by default. With cell detection disabled,
a single-model `tsr` artifact can use `.into()`. Browser callers supply `tsrCellArtifacts`
alongside their structure `tsrArtifacts` and select the models in `config.tsr`.

For a pure SLANet+ comparison, replace the existing TSR sections in `docparse.toml`:

```toml
[tsr]
mode = "tsr_only"
model = "slanet_plus"
model_path = "models/slanet-plus/inference.onnx"
model_config_path = "models/slanet-plus/inference.yml"
model_manifest_path = "models/slanet-plus/model-manifest.json"

[tsr.cell_detection]
enabled = false
```

Alternatively, use SLANeXt wireless with independent wireless cell detection:

```toml
[tsr]
mode = "tsr_only"
model = "slanext_wireless"
model_path = "models/slanext-wireless/inference.onnx"
model_config_path = "models/slanext-wireless/inference.yml"
model_manifest_path = "models/slanext-wireless/model-manifest.json"

[tsr.cell_detection]
enabled = true
model = "wireless"
score_threshold = 0.3
model_path = "models/rtdetr-table-cell-wireless/inference.onnx"
model_config_path = "models/rtdetr-table-cell-wireless/inference.yml"
model_manifest_path = "models/rtdetr-table-cell-wireless/model-manifest.json"
```

Provision the models and run from the repository root with the selected settings:

```sh
rtk uv run --locked scripts/download_models.py
rtk cargo run -p docparse-cli -- parse input.pdf
```

Real-model regression tests exercise structure and detector predictions with
singleton and batched requests. Detector preprocessing, inference and postprocessing
have separate timing stages. The real-PDF comparison test records crops, raw model
outputs, structured tables and warnings; failures remain in its report.
