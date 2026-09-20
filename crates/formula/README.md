# docparse-formula

Batched PP-FormulaNet_plus-S/Plus-L image-to-LaTeX recognition using the shared ONNX
execution lifecycle. The module takes formula crops, verifies the pinned graph
and BPE tokenizer, runs a real `[batch, 1, edge, edge]` tensor and returns one decoded
LaTeX string per crop. Core supplies existing layout boxes, applies batch/deadline
limits and attaches LaTeX and Markdown without replacing source text facts.
`formula.inline_enabled` and `formula.display_enabled` independently control which
regions enter recognition; both default to true. Set both to false to skip model
loading. The former `formula.enabled` master switch is no longer accepted.
Plus-S remains the default. Plus-S and Plus-M use a 384-pixel input edge;
explicitly supplied Plus-L artifacts retain their 768-pixel input edge. The verified graph hash selects the
variant, even when files are renamed.

## Artifacts

```sh
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-s
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-s --verify-only
```

- Plus-S graph SHA-256: `449d205c8fb2fe0a9b134a5e4a0f2421c2e7812fd902ea67dfda4e9ef4588978`.
- Plus-M graph SHA-256: `9e3539c2b4eeed28f2d35e342fd5bb0bdaa7f6034a475fc7e890c92780910618`.
- Plus-L graph SHA-256: `b4924d69c731365048de3d11a5d1829f3dfd8b98b4dbfd82437f934c2611934f`.
- Tokenizer SHA-256: `2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2`.
- Reference/export provenance: OAR-OCR `7feb044d74be09e3e2078a89cec0f0f8688e942b`, release v0.3.0 model artifact.

To select Plus-M, update the existing `[formula.engine]` selection in `docparse.toml`:

```toml
[formula]
queue_size = 8
inline_enabled = true
display_enabled = true

[formula.engine]
type = "pp"
model_path = "models/pp-formulanet-plus-m/inference.onnx"
tokenizer_path = "models/pp-formulanet-plus-m/tokenizer.json"
model_manifest_path = "models/pp-formulanet-plus-m/model-manifest.json"
```

Then provision the model and start the server from the repository root:

```sh
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-m
rtk cargo run --release -p docparse-server --features coreml
```

Use `--features cuda` on a CUDA host. Medium is a different model with the same
384-pixel input size as Small; selecting it does not increase crop resolution.

The graph emits i64 token IDs rather than probabilities. Decoding checks output
cardinality and termination, preserves LaTeX content, and does not invent a
confidence score or apply a post-inference token-length truncation. Vocabulary
size is cached at tokenizer initialization: querying it with added tokens clones
the vocabulary, so it must not run in the per-token decoding loop.

## Backend compatibility

Features mirror `docparse-tsr`: `default = []`, `metal`, `coreml`, `cuda`,
`openvino`, and `wasm`. Native provider features forward to `docparse-layout`;
select one provider per build. Core selects one provider for layout, OCR, TSR
and formula together through `coreml`, `cuda`, `metal`, or `openvino`; CLI and
server forward those unified features. Model-prefixed core features are not
supported. `wasm` forwards to config/layout, while the WASM-target tokenizer
dependency enables `unstable_wasm` for browser support.

CPU batches of 1, 2 and 3 identical real formula crops produced matching outputs.
In the earlier Plus-L investigation, CoreML's first invocation produced the correct result, but repeated invocations
failed or consumed excessive resources, including with microbatch 1. Disabling
memory patterns/CPU arenas did not fix the failure. MLProgram dynamic shapes
failed compilation; fixed-batch alternatives did not produce a validated reusable
accelerated path. The retained Apple behavior is therefore an explicit CPU
compatibility executor for all three variants. JSON reports the actual model, e.g.
`pp-formulanet-plus-s-onnx-cpu`. Plus-S acceleration through CoreML is not
claimed by this default-model change.

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
multi-image batches, cancellation and subsequent session reuse. Set
`FORMULA_TEST_MODEL=pp-formulanet-plus-m` or `pp-formulanet-plus-l` to check the
optional medium or large model.
`FORMULA_BENCH_OUTPUT=/absolute/path/to/result.json` records first/batched timings
and three subsequent warm single-crop runs separately.

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

## Shared queue

Native and browser sessions use one bounded per-crop queue shared by all callers.
Required `formula.queue_size` sets its capacity independently of sessions and batches.
`formula.engine.session_size` creates consumers on the selected backend (default 1,
positive, with no fixed upper limit). PP retains its CPU compatibility executor for
unsupported CoreML graphs on Apple. There is no separate CPU consumer group.
Native queue executors belong to the engine and survive its construction runtime.
Each idle owner drains only ready work up to `formula.batch_size`; short tails
run immediately. The parser reserves crop admission before raster allocation,
keeps a bounded sliding window, and maps independently completed results back to
original regions. Canceled requests do not use batch slots. Native PP execution
is terminated only when all callers sharing the physical batch have canceled;
one caller cannot terminate a neighbor's inference. Decoder failures remain
specific to the affected crop.

Native sessions leave intra-op threading at the ORT default. Browser sessions use
the ORT Web runtime's global thread settings.
