# ORT Layout Migration Design

## Goal

Replace the current Burn/WGPU and tract WASM layout experiments with a clean ORT-based architecture for PP-StructureV3 ONNX layout analysis. The new design must support native Rust through `ort` and browser WASM through `ort-web`, while keeping shared layout logic independent from backend-specific dependencies.

## Decisions

- Subcrate names use the `docparse-` prefix.
- The model directory is `models/pp-structure-v3-onnx`.
- `crates/docparse-core` owns shared types and backend-independent layout logic.
- `crates/docparse-ort` owns native ORT integration.
- `crates/docparse-web` owns WASM + `ort-web` integration.
- No commit is created unless explicitly requested.

## Delete Scope

Remove the old layout implementations and demos:

- `crates/docparse-layout`
- `crates/docparse-wasm`
- `crates/docparse-tract-wasm`
- `wasm/index.html`
- `wasm/tract.html`
- generated WASM outputs under `wasm/pkg` and `wasm/pkg-tract`
- old model directories under `wasm/models`
- `scripts/download_pp_doclayout_model.py`
- `scripts/reference_layout.py`
- `src/bin/layout_bench.rs`
- layout-specific code in `src/main.rs`
- tests tied to old layout APIs:
  - `tests/layout_workspace.rs`
  - old CLI assertions in `tests/cli.rs`

`Cargo.toml` and `Cargo.lock` will be regenerated around the new workspace members.

## Workspace Structure

```text
crates/
  docparse-core/
    src/
      analyzer.rs
      label.rs
      model.rs
      postprocess.rs
      preprocess.rs
      types.rs
      lib.rs
  docparse-ort/
    src/
      config.rs
      session.rs
      lib.rs
  docparse-web/
    src/
      session.rs
      lib.rs
scripts/
  download_pp_structurev3_onnx.py
models/
  pp-structure-v3-onnx/
    inference.onnx
```

## Workspace Dependencies

Put shared dependency versions in `[workspace.dependencies]` and consume them with `workspace = true` from each crate.

Shared dependencies:

- `anyhow`
- `thiserror`
- `serde`
- `serde_json`
- `image`
- `ndarray`
- `tracing`
- `ort`

WASM-only dependencies stay out of native crates:

- `ort-web` only in `docparse-web`
- `wasm-bindgen` only in `docparse-web`
- `wasm-bindgen-futures` only in `docparse-web`
- `js-sys` and `web-sys` only in `docparse-web`

Native-only dependencies stay out of web crates:

- native ORT execution-provider configuration only in `docparse-ort`

## Core Crate

`crates/docparse-core` is the only crate that knows the model’s logical input/output contract and output semantics. It does not create ORT sessions.

Responsibilities:

- define public result types
- define `LayoutLabel` as an enum, not a string
- define `LayoutAnalyzer`
- decode labels from numeric model class IDs
- preprocess images into model inputs
- postprocess model outputs into `LayoutPage`
- provide NMS and coordinate conversion
- validate known PP-StructureV3 tensor shapes where possible

Public types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LayoutLabel {
    ParagraphTitle,
    Image,
    Text,
    Number,
    Abstract,
    Content,
    FigureTitle,
    Formula,
    Table,
    TableTitle,
    Reference,
    DocTitle,
    Footnote,
    Header,
    Algorithm,
    Footer,
    Seal,
    ChartTitle,
    Chart,
    FormulaNumber,
    AsideText,
    ReferenceContent,
    Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutPage {
    pub width: u32,
    pub height: u32,
    pub blocks: Vec<LayoutBlock>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutBlock {
    pub label: LayoutLabel,
    pub score: f32,
    pub bbox: LayoutBox,
    pub order: Option<i64>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct LayoutBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
```

`LayoutAnalyzer` should be async because `ort-web` requires async session commit and tensor synchronization. The trait is meant for generic use first; object safety is not required in the first implementation.

```rust
pub trait LayoutAnalyzer {
    async fn analyze_image(
        &self,
        image: &image::DynamicImage,
    ) -> Result<LayoutPage, LayoutError>;
}
```

Core model input:

```rust
pub struct LayoutInput {
    pub image: ndarray::Array4<f32>,
    pub im_shape: ndarray::Array2<f32>,
    pub scale_factor: ndarray::Array2<f32>,
    pub original_width: u32,
    pub original_height: u32,
}
```

Core model output contract:

- Core accepts backend outputs as typed arrays plus shape metadata.
- Core does not expose ORT tensors in its public API.
- Output parsing starts with PP-StructureV3 ONNX output names and shapes discovered during implementation.
- If the model uses Paddle-style `fetch` output rows, core decodes `[class_id, score, x1, y1, x2, y2, order?]`.
- If the model exposes separate score/box/order tensors, core decodes those through a separate parser module.

## Native Backend Crate

`crates/docparse-ort` implements `LayoutAnalyzer` for native Rust.

Responsibilities:

- initialize ORT
- load `models/pp-structure-v3-onnx/inference.onnx`
- configure execution providers
- convert `LayoutInput` into ORT input tensors
- run inference
- convert ORT outputs into `docparse-core` output views
- call core postprocessing

Initial execution-provider policy:

- default: CPU, because it is portable
- optional config: ordered execution providers, such as CoreML, CUDA, TensorRT, or CPU
- EP failures should be explicit when requested and non-fatal only when configured as best-effort

Example config:

```rust
pub enum NativeExecutionProvider {
    Cpu,
    CoreMl,
    Cuda,
    TensorRt,
}

pub struct OrtLayoutConfig {
    pub model_path: std::path::PathBuf,
    pub execution_providers: Vec<NativeExecutionProvider>,
}
```

## Web Backend Crate

`crates/docparse-web` implements `LayoutAnalyzer` for WASM.

Responsibilities:

- initialize `ort-web`
- call `ort::set_api(ort_web::api(FEATURE_WEBGPU).await?)`
- create a session with `ep::WebGPU`
- load the model from URL or bytes
- convert core inputs into ORT tensors
- await web session creation and output synchronization
- expose a small wasm-bindgen API for the browser demo or consuming JS code

Initial web policy:

- prefer WebGPU
- expose an explicit fallback option to WASM CPU later, but do not silently mask WebGPU initialization failure in the first implementation
- log backend initialization and inference timing to make browser failures diagnosable

## Model Download Script

Create `scripts/download_pp_structurev3_onnx.py`.

Responsibilities:

- download PP-StructureV3 ONNX model files into `models/pp-structure-v3-onnx`
- avoid importing Paddle runtime
- print every downloaded file path
- keep output deterministic
- use a CLI argument for repository or source URL if needed

The script should prefer Hugging Face download APIs when the model is hosted there. If the model source requires a direct URL, the script should use streaming download with a visible progress line and checksum support once a known checksum is available.

## Root CLI

Remove layout command behavior during cleanup unless the ORT implementation is included in the same iteration. If a minimal CLI remains, it should not depend on old layout crates.

Future layout CLI shape:

```text
docparse layout --image <path> --model models/pp-structure-v3-onnx/inference.onnx --ep cpu
```

This CLI belongs after `docparse-ort` is implemented.

## Tests

Initial tests should focus on backend-independent contracts before ORT integration:

- `LayoutLabel::from_class_id` maps known class IDs to enum variants.
- unknown class IDs map to `LayoutLabel::Unknown`.
- bounding boxes convert from xyxy to xywh and clamp to image bounds.
- postprocessing filters by threshold.
- postprocessing sorts by reading order when present.
- preprocessing returns expected `1x3xHxW` layout and scale factors.

Native backend tests:

- session config rejects missing model path with a clear error
- a downloaded real ONNX model can create a session when available
- real inference test is ignored unless `PP_STRUCTUREV3_ONNX_MODEL` is set

Web backend tests:

- wasm crate builds for `wasm32-unknown-unknown`
- browser runtime tests are manual initially unless a browser automation setup is added

## Migration Sequence

1. Update workspace metadata and shared dependencies.
2. Delete old crates and old WASM demo files.
3. Delete old scripts and layout-specific tests.
4. Add `docparse-core` with types, trait, preprocessing, and postprocessing tests.
5. Add `download_pp_structurev3_onnx.py`.
6. Add `docparse-ort` native backend skeleton and tests.
7. Add `docparse-web` wasm backend skeleton and build verification.
8. Add real-model tests behind environment variables.
9. Reintroduce CLI only after native backend works.

## Open Implementation Checks

These must be verified during implementation:

- exact PP-StructureV3 ONNX repository or download URL
- exact ONNX input names
- exact ONNX output names and shapes
- whether PP-StructureV3 ONNX postprocess output is Paddle fetch rows or separate tensors
- exact `ort` and `ort-web` crate versions and feature names
- whether browser WebGPU requires cross-origin isolation for the chosen `ort-web` distribution

## Acceptance Criteria

- Old `docparse-layout`, `docparse-wasm`, and `docparse-tract-wasm` code is gone.
- Workspace members are `docparse-core`, `docparse-ort`, and `docparse-web`.
- Shared dependency versions live in the workspace where practical.
- `LayoutLabel` is an enum, not stringly typed.
- `LayoutAnalyzer` is defined in `docparse-core`.
- Native and wasm backend crates implement the same analyzer contract.
- Model download script writes to `models/pp-structure-v3-onnx`.
- `cargo test -p docparse-core` passes.
- `cargo check -p docparse-ort` passes.
- `wasm-pack build crates/docparse-web --target web` or equivalent wasm build passes.
