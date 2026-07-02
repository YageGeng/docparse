# ORT Layout Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace old Burn/WGPU and tract layout experiments with `docparse-core`, `docparse-ort`, and `docparse-web` crates built around ORT/ort-web.

**Architecture:** `docparse-core` owns backend-independent layout types, preprocessing, and postprocessing. `docparse-ort` and `docparse-web` adapt native ORT and browser ort-web sessions to the same `LayoutAnalyzer` trait. Old layout crates, demos, tests, and scripts are removed.

**Tech Stack:** Rust 2024 workspace, `ort`, `ort-web`, `wasm-bindgen`, `image`, `ndarray`, `serde`, `thiserror`, Python model download script.

**User Constraint:** Do not create commits.

---

### Task 1: Remove Old Layout Code

**Files:**
- Delete: `crates/docparse-layout`
- Delete: `crates/docparse-wasm`
- Delete: `crates/docparse-tract-wasm`
- Delete: `wasm/index.html`
- Delete: `wasm/tract.html`
- Delete: `scripts/download_pp_doclayout_model.py`
- Delete: `scripts/reference_layout.py`
- Delete: `src/bin/layout_bench.rs`
- Delete: `tests/layout_workspace.rs`
- Modify: `src/main.rs`
- Modify: `tests/cli.rs`
- Modify: `Cargo.toml`

- [x] Delete old crates, WASM demos, old scripts, and old layout benchmark.
- [x] Replace root CLI with a minimal non-layout placeholder until `docparse-ort` is wired.
- [x] Replace CLI test with a generic usage test that does not depend on old layout APIs.
- [x] Remove old workspace members and old root dependency on `docparse-layout`.

### Task 2: Create `docparse-core`

**Files:**
- Create: `crates/docparse-core/Cargo.toml`
- Create: `crates/docparse-core/src/lib.rs`
- Create: `crates/docparse-core/src/analyzer.rs`
- Create: `crates/docparse-core/src/label.rs`
- Create: `crates/docparse-core/src/model.rs`
- Create: `crates/docparse-core/src/preprocess.rs`
- Create: `crates/docparse-core/src/postprocess.rs`
- Create: `crates/docparse-core/src/types.rs`

- [x] Add `LayoutLabel` enum with class ID conversion.
- [x] Add `LayoutBox`, `LayoutBlock`, `LayoutPage`, `ModelOutput`, and `LayoutTensor`.
- [x] Add `LayoutAnalyzer` async trait.
- [x] Add image preprocessing to RGB NCHW `1x3x800x800`.
- [x] Add Paddle-style fetch-row postprocessing.
- [x] Add unit tests for labels, preprocessing shape, bbox clamping, threshold filtering, and order sorting.

### Task 3: Create Model Download Script

**Files:**
- Create: `scripts/download_pp_structurev3_onnx.py`

- [x] Add a Python script that downloads ONNX model files into `models/pp-structure-v3-onnx`.
- [x] Support `--repo-id`, `--filename`, and `--output-dir`.
- [x] Print downloaded file paths.

### Task 4: Create `docparse-ort`

**Files:**
- Create: `crates/docparse-ort/Cargo.toml`
- Create: `crates/docparse-ort/src/lib.rs`
- Create: `crates/docparse-ort/src/config.rs`
- Create: `crates/docparse-ort/src/session.rs`

- [x] Add native ORT config types.
- [x] Add `OrtLayoutAnalyzer` skeleton that validates model path and implements `LayoutAnalyzer`.
- [x] Keep real session wiring isolated so API changes in `ort` affect only this crate.
- [x] Add tests for config defaults and missing model errors.

### Task 5: Create `docparse-web`

**Files:**
- Create: `crates/docparse-web/Cargo.toml`
- Create: `crates/docparse-web/src/lib.rs`
- Create: `crates/docparse-web/src/session.rs`

- [x] Add wasm-bindgen-facing web analyzer skeleton.
- [x] Keep `ort-web` initialization isolated in this crate.
- [x] Expose WebGPU-first configuration.
- [x] Add compile-time structure that avoids pulling wasm dependencies into native crates.

### Task 6: Verify

**Commands:**
- `cargo fmt`
- `cargo test -p docparse-core`
- `cargo test -p docparse-ort`
- `cargo check -p docparse-web --target wasm32-unknown-unknown`
- `cargo test`

- [x] Run verification commands.
- [x] Report exact pass/fail status and any blocked dependency/API issues.
