# docparse-tsr

Independent Paddle SLANet_plus ONNX inference for layout-owned table crops.
Models stay external. Layout and TSR reuse the same ONNX execution-provider
registration: CPU, CUDA, CoreML, OpenVINO, and browser WebGPU. Native layout, OCR
and TSR share the backend selected by Cargo features, defaulting to CPU. Enable
one of `cuda`, `coreml`, `metal`, or `openvino`; no runtime provider configuration
is accepted. `metal` uses CoreML with `CPUAndGPU` compute units (no separate Metal EP).
Native sessions specialize the pinned model to the preprocessing contract
`[1, 3, 488, 488]` before graph initialization, including CoreML shape inference.
WASM defaults to WebGPU, and the Web SDK applies its selected backend to all
models. Unsupported operators may still execute on CPU inside an accelerated
session; unavailable requested providers fail explicitly.

```sh
rtk uv run --locked scripts/download_models.py --model slanet-plus
rtk cargo test -p docparse-tsr -- --include-ignored
rtk cargo test -p docparse-tsr --features metal --test inference -- --include-ignored
```

`SlanetPlusEngine::from_artifacts(Arc<ValidatedConfig>, ModelArtifacts)` validates the pinned model,
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

Each engine owns one serialized session. Native cancellation retains the session
guard until blocking inference finishes; the browser actor owns inputs until the
ORT Promise settles. Layout and TSR share a browser inference guard through
output readback because ORT WebGPU reuses download buffers across sessions.
A caller deadline cannot release the guard before the actual model run finishes.
No model URL, authentication state, or generated PDF text is stored in this crate. The core adapter retains independent learned positions,
reconciles inconsistent token spans, and calibrates the grid against native/OCR
word geometry and visible separators. Word ownership remains explicit, complete,
and source-preserving even when learned content rectangles overlap.
A high structure confidence is not a guarantee of correct table semantics.
