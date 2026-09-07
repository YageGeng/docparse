# Native/Web implementation plan

Implement and verify the platform boundaries, shared interfaces, browser runtime, and regression matrix in that order.

**Goal:** Deliver one native/Web CPU parsing pipeline with reproducible browser builds, lifecycle behavior, and result validation.

**Architecture:** Preserve the PDFium actor, pipeline, and ORT model. Keep platform decisions in each crate's wasm_compat entry and explicitly approved compatibility submodules. The Web facade owns one dedicated Worker; Rust business algorithms remain shared.

**Stack:** Rust, Tokio, ort, pinned ort-web patch, wasm-bindgen, TypeScript, and module Workers.

**Specification:** [Native/Web support](../specs/2026-09-07-native-web-wasm-design.md).

**Status:** Implementation and Chromium CPU/WebGPU acceptance are complete within the scope recorded in the [validation report](../reports/2026-09-07-native-web-wasm-validation.md). Additional browsers and the full CUDA corpus remain outside the verified scope.

## Global constraints

- Use `all(feature = "wasm", target_arch = "wasm32")` consistently in shared crates.
- Keep platform cfg/cfg_attr/cfg! and expansion macros out of business files. Exact test-module declarations are allowed.
- Preserve the model, business schema, algorithms, native concurrency, and native font selection.
- Use real PDFium, the fixed model, and production Rust preprocessing for browser acceptance.
- Declare versions and paths in workspace.dependencies; select native features at target boundaries.
- Write documentation and code comments in English. Use fully qualified tracing macros with readable message arguments.
- Use typed-builder for new structs with more than three fields, and explicit Arc::clone for shared fields.
- Keep machine-specific configuration and generated runtime reports outside the feature commit.

## Task 1: Model contracts and resource entry points

Files: layout/model_manifest.rs, pp_doclayout_v3/schema.rs, pp_doclayout_v3/mod.rs, wasm_compat.rs and its native.rs/web.rs/session_pool.rs submodules; config validation and compatibility modules; related tests.

Deliver: `ModelArtifacts { model, config, manifest: Arc<[u8]> }`, verify(), from_artifacts(), and a platform-neutral tensor contract.

- [x] Accept valid dynamic dimensions without requiring backend-specific symbolic names; continue rejecting incorrect fixed dimensions.
- [x] Reject altered artifact hashes and validate the fixed file fixtures.
- [x] Separate artifact-content validation from native filesystem reads.
- [x] Create sessions from the exact verified byte buffers.
- [x] Move path resolution to native entry points while preserving ConfigLoader/profile/environment behavior.
- [x] Run layout/config regressions.
- [x] Remove the old pool.rs forwarding module and retain its lease-return test beside the native pool implementation.
- [x] Select native.rs/web.rs at module declarations and re-exports; these files contain no cfg attributes themselves.

## Task 2: Thread bounds and ORT compatibility

Files: workspace/crate manifests, layout/core compatibility modules, engine.rs, ocr.rs, and their implementations/tests.

Deliver: WasmCompatSend, WasmCompatSync, WasmBoxedFuture, ModelOutputs, LayoutSessionPool::load/run, and shared detection postprocessing.

- [x] Pin Web to api-17 and native to api-28 with the existing provider features.
- [x] Verify native and native+wasm reject an Rc-based engine while Web accepts it.
- [x] Migrate engine signatures to boxed futures and update all callers/examples.
- [x] Preserve bounded native session leases; use a browser actor that exclusively owns its session across awaits.
- [x] Synchronize Web outputs before converting them into owned ModelOutputs.
- [x] Check native, native+wasm, and Web builds.

## Task 3: PDFium, scheduling, and filesystem boundaries

Files: core runtime/parser/render modules; wasm_compat.rs and task_set.rs/pdfium_worker.rs/pdf_input.rs/native.rs; PDFium and pdfium-sys compatibility files.

Deliver: PdfiumWorker::spawn/join, spawn, TaskSet::spawn/join_next/abort_all, and DocParser::from_artifacts.

- [x] Preserve real-PDF and fatal-cleanup regression coverage.
- [x] Use one shared async PDFium command loop, driven by a dedicated native thread or browser-local task.
- [x] Replace pipeline-specific Tokio types with the limited compatibility interface while retaining native concurrency.
- [x] Close the executor on early context-construction failures.
- [x] Move native path/blocking/file-output and FFI/OS choices into compatibility modules.
- [x] Reject unsupported targets, missing shared-crate wasm features, and Web/native-provider combinations.
- [x] Run core/PDFium/config/layout tests and both target checks.

## Task 4: Pinned upstream runtime and WASM packaging

Files: vendor/ort-web, crates/web, packages/web build scripts/WASI imports, and provenance/license documentation.

Deliver: release WASM, wasm-bindgen glue, and pinned module-Worker-compatible runtime assets.

- [x] Record the upstream archive checksum and reproduce the original Worker/fetches failures.
- [x] Patch the module Worker loader and actual fetches forwarding.
- [x] Add JS session/tensor resource release and verify the real lifecycle.
- [x] Pin PDFium libraries, final setjmp/WASI linkage, and allowed imports.
- [x] Provide reproducible tool versions and build commands without temporary registry edits.
- [x] Run the fixed model with the production Rust preprocessing path in a real Worker.

## Task 5: Browser API and lifecycle

Files: packages/web/src/index.ts, worker.ts, wasm_imports.ts; crates/web/src/lib.rs and build.rs; packages/web/tests.

Deliver: createParser(options), parse(bytes, {signal}), render(document, format), and close().

- [x] Project Web options onto shared configuration and validate resources/concurrency.
- [x] Handle request identity, Busy, idempotent close, signal cleanup, and fatal Worker settlement.
- [x] Copy the caller's exact Uint8Array view before transfer.
- [x] Resolve URLs in the facade, self-host matching runtime assets, and disable telemetry.
- [x] Exercise real PDF parsing, renderers, cancellation, corrupt input, shutdown, and recreation.
- [x] Make docparse-web browser-only, include it through members = ["crates/*"], and omit it from native default-members.
- [x] Require an explicit browser package/target build and reject direct native builds of docparse-web.

## Task 6: Acceptance, checks, and documentation

Files: tests, the cfg-boundary checker, READMEs, specification, and validation report.

- [x] Test multiline cfg/macros and reject attributes outside explicit boundaries.
- [x] Run default native tests and the separate Web build matrix.
- [x] Run format, Clippy, real-model regressions, and supported native-provider compile checks.
- [x] Compare fixed native/Web fixtures using text/structure rules, numeric tolerances, and ResultValidator.
- [x] Exercise repeated parsing, cancellation/recreation, relocated assets, and memory stability.
- [x] Validate WebGPU separately; keep Firefox/Safari listed as unverified.
- [x] Document usage, engine migration, target selection, licenses, and reproduction steps in English.
- [x] Keep generated evidence in ignored output directories and remove obsolete investigation fixtures.
