# Web Optimization and Stage Timings Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan. Steps use checkbox syntax for tracking.

**Goal:** Optimize the shipped DocParse WASM with `wasm-opt -O4` and expose meaningful stage durations without changing canonical document results.

**Architecture:** Run a pinned Binaryen optimizer after wasm-bindgen and before ABI validation and hashing. Collect Rust timings through an optional per-parse channel and deliver callbacks serially through the existing observer. Use a Worker-compatible monotonic clock. Add SDK timing events and a compact example summary. Keep all inference and PDF buffers under their existing ownership rules.

**Tech stack:** Rust, web-time, Tokio channels, wasm-bindgen, Binaryen, TypeScript, browser Workers.

- [x] Pin Binaryen, optimize a temporary WASM file with the required PDFium features, validate it, and record optimizer metadata and artifact sizes.
- [x] Test timing collection across concurrent producers and drain boundaries; add stage instrumentation for extraction, rendering, inference, fusion, and linking.
- [x] Add `onTiming` to SDK initialization and parse options; measure downloads, initialization, serialization, and preview encoding separately. Keep callbacks isolated and request-scoped.
- [x] Show stage totals in the example without increasing its fixed viewport height. Document inclusive totals, concurrent stages, cold first inference, and callback behavior.
- [x] Run Rust checks, TypeScript checks, the optimized build, and real-model browser acceptance on CPU and WebGPU, including timing events and repeated parses.

## Acceptance boundaries

Timings are elapsed wall-clock intervals, not GPU kernel profiling. Session wait is separate from ORT execution. Web output synchronization is separate from the ORT Promise. Concurrent and nested intervals must not be summed into an end-to-end total. Canonical JSON remains deterministic. No dummy warmup inference is introduced by this task.

## Verification

- `cargo test --locked -p docparse-core -p docparse-layout`: 192 passed, 5 ignored; the real-model native reference test also passed.
- Native and browser-target Clippy, compatibility-boundary validation, SDK type checking, and example compilation passed.
- Optimized release WASM: 8,518,623 to 6,329,611 bytes; 141 imports validated. Optimization took 80.9 seconds on the local machine.
- CPU and WebGPU browser suites passed with three repeat cycles each, forced memory growth, no intermediate input copies, no retained tensors, cancellation/recreation, stage attribution, and strict embedded-font parity. Unembedded-font metric differences were recorded separately.
- The production example parsed all 47 pages of `2403.01632v4.pdf` with WebGPU, displayed per-stage counts and durations, and preserved the 1280 by 720 viewport without document scrolling. Timing samples are observations, not a controlled before/after performance benchmark.
- Reusing the model for a second 47-page parse retained 610 regions and omitted initialization timings. Status wall times were 18.4 seconds initially and 14.6 seconds with the existing Worker. Detailed local evidence: `target/web-optimization-timings-verification.json`.
