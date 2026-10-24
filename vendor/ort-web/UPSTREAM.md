# Pinned ort-web patch

Baseline: crates.io `ort-web 0.5.0+1.30`, source revision `4fac49ba41dc90313585e09d01e84fb43dabc8e4`, directory `backends/web`.

Published archive SHA-256: `d3713d7c0f33ee3a7a01e22115abc4249001dfa3fa74632a55f9b441c1df84d6`.

Local changes:

- `_loader.js`: use `globalThis` and load the ESM runtime in module Workers without DOM shims.
- `api/session_options.rs`: accept rc.15's canonical `WebGpuExecutionProvider` name as well as the existing `WebGPU` alias.
- `api/memory.rs` and `memory.rs`: retain allocator metadata for GPU outputs now owned through allocator handles in rc.15. Actual host allocation/free remains CPU-only; JavaScript continues to own GPU storage.
- Upstream 0.5 now forwards requested output names, graph optimization, and memory pattern options through the split `api/` modules. These former local patches are retained through the upstream implementation. `crates/web/tests/session_options.mjs` checks the real SDK-to-ORT boundary for Layout, TSR, cell detection, OCR, PP formula, and Texo sessions.
- `session.rs`, `tensor.rs`, `api/session.rs`, and `binding/session.rs`: preserve release/dispose diagnostics and extend upstream output cleanup to owned input and temporary feed wrappers. Borrowed GPU aliases never dispose the original hidden-state or KV-cache buffer. Temporary feeds stay owned until asynchronous inference completes, including failure paths.
- `_memory.js`, `binding/mod.rs`, `lib.rs`, and `tensor.rs`: enable resizable WASM buffers before runtime initialization and borrow stable input views. Engines without that capability create one safe input snapshot at the run boundary because asynchronous inference may overlap parser heap growth. Constructors keep only metadata views, and Rust-backed values remain authoritative until a fresh feed is prepared; they do not synchronize through potentially detached metadata views. The host must declare a memory maximum and adapt its wasm-bindgen text codec glue for resizable buffers; DocParse does this in its Web build. Explicit synchronization of external tensors remains available. ORT still copies CPU inputs into its own WASM heap.
- `binding/tensor.rs`: suppress existing dead-code warnings for optional binding descriptors.
- `binding/session.rs` and `api/session_options.rs`: support per-output `preferredOutputLocation` through `ort_web.preferred_output_location.<output>` session config entries. Unconfigured sessions retain the upstream CPU-output default. Texo requests GPU-resident hidden states/KV caches and CPU logits; its browser regression asserts actual tensor locations. Upstream generic session entries retain their normal handling.

Cargo manifests retain the published dependency declarations. LICENSE files and this provenance note are included; the upstream package's Cargo.lock and VCS metadata are omitted. Native builds do not depend on this package. Browser lifecycle tests exercise output selection, resource release, and input ownership across memory growth.
