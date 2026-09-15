# docparse-formula

Batched PP-FormulaNet_plus-L image-to-LaTeX recognition using the shared ONNX
execution lifecycle. The module takes formula crops, verifies the pinned graph
and BPE tokenizer, runs a real `[batch, 1, 768, 768]` tensor and returns one decoded
LaTeX string per crop. Core supplies existing layout boxes, applies batch/deadline
limits and attaches LaTeX and Markdown without replacing source text facts.

## Artifacts

```sh
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-l
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-l --verify-only
```

- Graph SHA-256: `b4924d69c731365048de3d11a5d1829f3dfd8b98b4dbfd82437f934c2611934f`.
- Tokenizer SHA-256: `2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2`.
- Reference/export provenance: OAR-OCR `7feb044d74be09e3e2078a89cec0f0f8688e942b`, release v0.3.0 model artifact.

The graph emits i64 token IDs rather than probabilities. Decoding checks output
cardinality and termination, preserves LaTeX content, and does not invent a
confidence score or apply a post-inference token-length truncation.

## Backend compatibility

Features mirror `docparse-tsr`: `default = []`, `metal`, `coreml`, `cuda`,
`openvino`, and `wasm`. Native provider features forward to `docparse-layout`;
select one provider per build. `wasm` forwards to config/layout, while the
WASM-target tokenizer dependency enables `unstable_wasm` for browser support.

CPU batches of 1, 2 and 3 identical real formula crops produced matching outputs.
CoreML's first invocation produced the correct result, but repeated invocations
failed or consumed excessive resources, including with microbatch 1. Disabling
memory patterns/CPU arenas did not fix the failure. MLProgram dynamic shapes
failed compilation; fixed-batch alternatives did not produce a validated reusable
accelerated path. The retained Apple behavior is therefore an explicit CPU
compatibility executor, reported as `pp-formulanet-plus-l-onnx-cpu` in JSON.

Other native builds retain the selected provider. CUDA execution has not been
hardware-validated in this integration. Browser hosts supply explicit formula
artifact bytes/URLs and use their selected ORT-Web provider; native CoreML results
are not evidence of browser compatibility.

The native owner keeps tensors alive and signals `RunOptions::terminate` on
caller cancellation. The browser actor retains inputs until the JS promise and
output synchronization finish, under the existing cross-model inference lock.

## Real-model check

```sh
rtk proxy env FORMULA_TEST_CROP=/absolute/path/to/formula.png \
  cargo test --release -p docparse-formula --test model \
  real_formula_batches_preserve_cardinality_and_content -- --ignored --nocapture
```

Use `--features coreml` to verify the Apple compatibility executor. The test checks
multi-image batches, cancellation and subsequent session reuse.

## Source organization

Like `docparse-tsr`, `lib.rs` exposes the public API, `artifacts.rs` verifies model
assets, `model.rs` owns the engine and output decoding, `preprocess.rs` builds
input tensors, and `wasm_compat.rs` owns native/browser execution and file access.

## Attribution

Image preprocessing is adapted from Apache-2.0 OAR-OCR
[formula_preprocess.rs](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/processors/formula_preprocess.rs):
foreground margin cropping, triangular resizing, centered black padding and
normalized grayscale. The root and upstream projects retain their respective
Apache-2.0 license notices.
