# DocParse native/Web support specification

## 1. Status and decisions

Date: 2026-09-07. This document describes the implemented design and its acceptance requirements. The [validation record](../reports/2026-09-07-native-web-wasm-validation.md) distinguishes verified combinations from unverified requirements.

The implementation keeps PDFium and ort, shares document algorithms between native and browser builds, and preserves DocumentResult schema 2.0. Each cross-platform crate exposes one wasm_compat entry point. Explicitly approved submodules contain larger platform implementations; business modules do not choose platforms.

Native retains Tokio, dedicated PDFium threads, bounded ORT sessions, existing execution providers, and system-font behavior. Web runs inside one dedicated module Worker with one model session and page concurrency of one. CPU/WASM is the default; WebGPU is opt-in and independently validated.

Thread compatibility is expressed through marker bounds and boxed futures, without unsafe Send/Sync implementations for browser handles. Resource verification, model schema checks, and real-browser acceptance remain mandatory.

This specification supersedes platform assumptions in the earlier layout-fusion design. Text facts, coordinates, reading order, relations, stable-ID rules, evidence, warnings, and renderer semantics retain their existing contracts.

## 2. Scope

Required capabilities:

- Parse the same PDF bytes, model artifacts, and business configuration through shared native/Web algorithms.
- Share text extraction interpretation, coordinate transforms, INTER_CUBIC preprocessing, output validation, postprocessing, fusion, reading order, cross-page relations, and ResultValidator.
- Preserve native CLI, filesystem paths, blocking conveniences, profile/environment merging, and file output.
- Process browser PDFs and models locally without a parsing server.
- Settle initialization, document, inference, Worker, and cancellation failures with explicit terminal outcomes.
- Enforce the conditional-compilation boundary automatically.

| Environment | Contract |
|---|---|
| Native | Preserve existing supported targets, thread bounds, concurrency, and providers |
| Browser | wasm32-unknown-unknown inside a module Worker |
| Browser acceptance | Record an actual desktop Chromium version; validate Firefox/Safari independently before claiming support |
| WebGPU | Explicit selection and independent runtime/resource acceptance |
| Node.js and WASI application hosts | Outside the initial delivery scope |
| Mobile browsers | No initial performance or memory guarantee without device testing |

Native/Web support means separate builds of the same shared source, not both runtimes inside one binary.

Non-goals include PDF.js replacement, remote parsing/OCR, rewritten document algorithms, a general Tokio replacement, a generic backend/plugin framework, unused HTTP/Stream abstractions, shared-memory Rust WASM threads, cross-Worker PDFium handles, multi-Worker inference pools, built-in IndexedDB/OPFS caching, resumable downloads, quantization, graph pruning, and a new OCR model or JavaScript OCR callback protocol. Cross-provider floating-point output is not required to be byte-identical.

## 3. Baseline and fixed model identity

Initial probes exposed native Tokio features and binary downloads in the Web dependency graph, a loader that accessed window inside a Worker, unsupported newer ORT API entries, and a RunAsync bridge that did not forward output selection. Compilation and isolated model probes did not establish full browser parsing support; production-path acceptance was required afterward.

The fixed model is:

| Property | Value |
|---|---|
| Repository | PaddlePaddle/PP-DocLayoutV3_onnx |
| Revision | 46bbdf188bb0a772c08aed74882ce7e51a8f1ea6 |
| ONNX SHA-256 | 45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba |
| YAML SHA-256 | 506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc |
| ONNX size | 130,502,049 bytes, approximately 124.456 MiB |
| License | Apache-2.0 |

Temporary Canvas preprocessing, registry edits, and permissive WASI probes are not production implementations. Reproducible repository tests replace temporary investigation directories. The original single-run probe timing is not a performance baseline.

## 4. Module responsibilities

| Location | Responsibility |
|---|---|
| core/runtime/pdfium_executor.rs | Shared commands and serialized extraction/rendering |
| core/runtime/pipeline.rs | Shared scheduling policy, context, failure handling, and fusion |
| core/parser.rs | Shared bytes/page APIs and engine injection |
| core/wasm_compat/pdfium_worker.rs | Native thread or browser-local PDFium actor lifetime |
| core/wasm_compat/task_set.rs | Native Tokio/local browser task scheduling |
| core/wasm_compat/pdf_input.rs | Native path/bytes sources and browser byte sources |
| core/wasm_compat/native.rs | Native path/blocking parsing and overlay file output |
| layout/pp_doclayout_v3/mod.rs | Shared preprocessing, execution call, and postprocessing |
| layout/pp_doclayout_v3/session.rs | Owned, validated output conversion |
| layout/wasm_compat/session_pool.rs | Native sessions/leases and the Web inference actor |
| layout/wasm_compat/native.rs and web.rs | Platform model initialization, metadata, and CPU execution |
| config/wasm_compat.rs | Native configuration loading and Web concurrency limits |
| pdfium and pdfium-sys compatibility modules | Locks, target/OS selection, optional symbols, and FFI dispatch |
| crates/web | Browser-only ABI and final WASM linkage |
| packages/web | TypeScript facade, Worker, deployment, and browser acceptance |

The former layout pool.rs forwarding file is removed. Its lease-return test remains beside the native pool implementation.

## 5. Conditional compilation

Shared crates use one browser predicate and its negation:

```rust
all(feature = "wasm", target_arch = "wasm32")
```

Use the same predicate for markers, futures, and platform implementations. Do not mix it with target_family-based aliases. Native builds with the wasm feature still select native code and retain Send/Sync.

Reject shared wasm32 builds without the wasm feature, wasm32 hosts other than target_os=unknown, and Web builds with CUDA/CoreML/OpenVINO features. Native provider features remain mutually exclusive.

First-party source cfgs are allowed only in:

- crates/config/src/wasm_compat.rs
- crates/core/src/wasm_compat.rs
- crates/core/src/wasm_compat/task_set.rs
- crates/core/src/wasm_compat/pdfium_worker.rs
- crates/core/src/wasm_compat/pdf_input.rs
- crates/layout/src/wasm_compat.rs
- crates/layout/src/wasm_compat/session_pool.rs
- crates/pdfium/src/wasm_compat.rs
- crates/pdfium-sys/src/wasm_compat.rs

Core/layout native.rs and layout web.rs contain no cfg attributes. Their entry modules select them at module declarations and exports. Core's entry retains declarations, exports, and build constraints; layout's entry also owns the foundational markers, boxed future, and TaskError.

The browser-only docparse-web crate directly exports lib.rs and has no compatibility shell. Its build.rs enforces the exact browser target. It receives no additional source-cfg allowance.

Do not hide platform choices in cfg!, if_wasm!/if_not_wasm!, generated business code, or per-engine async_trait cfg_attr annotations. Business code may unconditionally import compatibility types.

Allowed build/test boundaries are Cargo target dependencies and feature propagation, build.rs decisions based on Cargo's target environment, generated FFI bindings, and exact `#[cfg(test)] mod tests` declarations. Tests must not introduce scattered platform conditions. The pinned vendor/ort-web patch is excluded explicitly; no business directory receives a wildcard exclusion.

`scripts/check_wasm_compat.py` scans multiline cfg/cfg_attr attributes and platform macros after masking comments/literals. Its tests must demonstrate both accepted boundaries and rejected neighboring files.

## 6. Compatibility types and engines

Define WasmCompatSend, WasmCompatSync, and WasmBoxedFuture in layout::wasm_compat and re-export them through core. Do not add a crate solely for a few marker types.

| Type | Native | Web |
|---|---|---|
| WasmCompatSend | Send supertrait | Empty marker |
| WasmCompatSync | Sync supertrait | Empty marker |
| WasmBoxedFuture<'a, T> | Pin<Box<dyn Future<Output=T> + Send + 'a>> | Pin<Box<dyn Future<Output=T> + 'a>> |

Blanket marker implementations include ?Sized. The markers do not make JS objects transferable. A Send future does not imply a Send output; native spawn adapters constrain both separately.

Do not add unused Stream aliases. If a real stream is later required, preserve an explicit lifetime and constrain its actual Item. The illustrative HTTP-specific WasmCompatSendStream/InnerItem contract is not imported into this project.

LayoutEngine and OcrEngine remain object-safe and inherit WasmCompatSend + WasmCompatSync. Their operations return:

```rust
/// Detects layout from an owned page request on the active platform.
fn detect(
    &self,
    request: LayoutRequest,
) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>>;

/// Recognizes missing text from an owned OCR request on the active platform.
fn recognize(
    &self,
    request: OcrRequest,
) -> WasmBoxedFuture<'_, Result<OcrResult, OcrError>>;
```

Implementations use Box::pin(async move { ... }); callers continue to await them. Existing name/model_revision methods and Arc<dyn Engine> injection remain. Arc preserves the ownership interface, not cross-Worker sharing.

This is a source migration for custom async_trait engine implementations. Update examples and test native Send/Sync as well as a Web engine owning non-Send local state.

## 7. Artifacts, configuration, and Rust APIs

ModelArtifacts owns three Arc<[u8]> fields: model, config, and manifest. It is an input bundle for the supported fixed model, not an arbitrary model-plugin interface.

Before publishing an engine, validate manifest provenance, declared and actual model/YAML hashes, YAML preprocessing/labels, and actual session input/output contracts. Trust neither the download URL nor self-reported manifest hashes alone.

Resource flow:

1. Read files on native, or fetch/receive artifact bytes in the browser binding.
2. Apply shared Rust content verification.
3. Create the platform session from those same verified buffers using commit_from_memory.
4. Validate the actual session and publish the engine; release partial resources on failure.

Native loading runs outside the async executor. Never re-open the model path after verification. Pool initialization shares application-level Arc buffers; ORT may retain its own copies. Account for the additional native startup model buffer and release unnecessary copies after initialization. Networking, HTTP cache, authentication, and URL resolution remain outside core.

Configuration rules:

- Preserve native TOML fields/defaults and profile/environment precedence.
- ValidatedConfig checks shared numeric/content invariants; native ConfigLoader/model entry points resolve and validate paths.
- Explicit artifacts or injected engines do not read legacy path fields and need no placeholder absolute paths.
- WebParseConfig projects the shared business groups with snake_case names, excluding paths, profile/environment inputs, and layout.execution_provider.
- Web provider selection belongs to WebParserOptions, not DocumentResult.
- Web session_pool_size, page_concurrency, render_queue_capacity, and blocking_task_limit must all equal 1. Reject unsupported values rather than silently truncating them.
- Preserve existing continue_on_page_error, render, fusion, OCR, and output validation.

| API | Contract |
|---|---|
| PpDocLayoutV3Engine::from_artifacts | Verified model bytes on either platform |
| DocParser::from_artifacts | Create the engine and reuse existing builder injection |
| DocParser builder with explicit engine | Shared; no implicit model-file reads |
| DocParser::from_config | Native file loading; Web without explicit artifacts/engine reports ModelArtifactsRequired |
| parse_bytes(Arc<[u8]>) | Complete shared document parsing |
| parse_page(PageInput) | Shared single-page parsing from real supplied facts |
| parse_path / parse_path_blocking | Native conveniences |
| ConfigLoader / inspect_model / write_pdf_overlays | Native filesystem capabilities |

Do not introduce competing engine-versus-artifact state into DocParserBuilder; from_artifacts uses the existing engine-injection path.

## 8. Shared pipeline and PDFium ownership

The shared flow is PDF bytes, PDFium prescan/text facts, frozen DocumentContext, bounded rendering, Rust INTER_CUBIC/NCHW preprocessing, platform inference, validated owned outputs, shared postprocessing/fusion/reading order, cross-page relations, ResultValidator, and DocumentResult.

One serialized actor owns Library, source bytes, and Document. Borrowing ensures bytes and Library outlive Document; do not introduce raw-pointer self-referential storage. Temporary Page/TextPage/Bitmap handles are dropped before responding. Channels carry owned facts, pixels, and transforms only.

Native retains the process-global PDFium lock and a dedicated thread that drives the async command loop. The actor future itself need not be Send. Web drives the same loop as a local task inside the embedding Worker's realm. Synchronous FFI blocks that Worker, not the page. Web must not call blocking_recv or create a Worker for each page/command.

PdfiumExecutor's open/pre_scan_page/render_page/page_count/close interface stays platform-neutral. PdfiumWorker owns platform startup and join behavior.

Every normal and error exit, including context construction and rendering/task failures, must reach cleanup. Orderly close stops new work, waits for active FFI, releases document/library/bytes, and confirms completion. Preserve the original business failure if cleanup also fails; log cleanup context. Native join and waits for started blocking work must not block an async executor thread.

## 9. Task scheduling

The compatibility layer provides only the operations required by the pipeline: spawn, completion, a bounded-by-caller task set, abort_all, and a portable task error.

Native retains Tokio JoinSet and blocking-pool behavior. Web uses local tasks and a finite local-future collection, with ORT's asynchronous API. Browser execution must demonstrate that reused Tokio channels/select operations work without entering a Tokio runtime.

Preserve native concurrency. Business errors and collection code depend on TaskError rather than tokio::task::JoinError. Convert platform failures into owned diagnostic context without storing JS handles in native Send/Sync error chains.

Dropping a caller does not stop an already-running native spawn_blocking closure or Web ORT Promise. The executing closure/actor retains its session and input buffers until completion; no lease may be returned early.

## 10. ORT contracts and the upstream patch

The public detect flow is shared preprocessing into owned ModelInputs, a compatibility-layer run, owned ModelOutputs containing boxes/count, then shared postprocessing. ORT sessions, SessionOutputs, JS tensors, and backend exceptions do not enter core's public interface.

Native uses exclusive bounded session leases. Web serializes owned requests through one actor and holds no MutexGuard or RefCell borrow across an await.

Validate the complete fixed graph at initialization:

| Direction | Name | dtype | Shape |
|---|---|---|---|
| Input | im_shape | f32 | [dynamic, 2] |
| Input | image | f32 | [dynamic, 3, 800, 800] |
| Input | scale_factor | f32 | [dynamic, 2] |
| Output | fetch_name_0 | f32 | [dynamic, 7] |
| Output | fetch_name_1 | i32 | [dynamic] |
| Output | fetch_name_2 | i32 | [dynamic, 200, 200] |

Match names, dtypes, ranks, counts, and fixed dimensions. Dynamic dimensions need not retain their original symbolic names. Validate the third output even though inference consumers do not request it.

Runtime inputs use batch one. Boxes must be two-dimensional with seven columns; count must contain one nonnegative value no larger than the row count. Existing postprocessing validates class, score, and geometry. Missing outputs are errors.

Native inspection retains descriptive metadata. Web explicitly represents unsupported descriptive values as unavailable, while required identity comes from the verified manifest. Do not fabricate producer/graph descriptions or swallow unrelated ORT failures.

Pins: ort =2.0.0-rc.13; Web api-17 plus alternative-backend; native api-28; ort-web 0.3.0+1.27; ONNX Runtime Web 1.27.0. Do not inherit newer native API entries into Web.

Use the recorded crates.io archive and local vendor/ort-web patch, with root patch.crates-io and an explicit workspace exclusion. Preserve licenses and provenance. The patch:

- Loads ESM in module Workers through globalThis without fabricated DOM globals.
- Forwards Rust's requested output names into JavaScript session.run fetches.
- Releases JS sessions/tensors when Rust owners drop; session release is asynchronously scheduled.
- Suppresses existing unused optional binding declarations locally.

Request only fetch_name_0 and fetch_name_1, explicitly synchronize them before Rust reads, and convert them into owned values. Omitting the approximately 48 MB mask return does not establish pruning of internal model computation. Upstream replacement requires equivalent integration acceptance.

## 11. PDFium linkage and browser imports

Pin the static WASI PDFium foundation to chromium/8028 and verify its library checksums. Keep native and WASM library caches distinct. The final docparse-web build.rs owns final-artifact linker flags; dependency rustc-link-arg output is not assumed to propagate.

Link PDFium, WASI libc/libc++/ABI, emulation libraries, and real setjmp/longjmp support. Provide only the browser imports actually required by the final binary. Validate pointer ranges, clocks, stdio, errno, and proc_exit behavior. Unsupported filesystem operations return appropriate failures; a no-op exception/setjmp shim is not acceptable.

Reject unknown imports and unsupported required WASM features at packaging time. Test normal, rotated, CropBox/UserUnit, embedded Chinese, malformed PDF, and recoverable resource-error paths in the real browser artifact.

## 12. Browser ABI, facade, and lifecycle

crates/web directly exposes browser exports, serialization, logging, and PDFium libc support from lib.rs. build.rs rejects targets other than wasm32-unknown-unknown and configures final linkage. packages/web contains the main-thread facade, Worker, WASI adapter, build scripts, and browser acceptance tests.

Workspace members = ["crates/*"] includes the Web crate. default-members selects cli/config/core/layout/pdfium/pdfium-sys. Root default build/check/test/clippy commands omit Web; explicit --workspace needs --exclude docparse-web on native. The Web crate has no platform feature switch and directly enables its shared dependencies' wasm features.

```typescript
type ModelSource =
  | { kind: "urls"; model: string; config: string; manifest: string }
  | { kind: "bytes"; model: Uint8Array; config: Uint8Array; manifest: Uint8Array };

interface WebParserOptions {
  artifacts: ModelSource;
  runtimeBaseUrl?: string;
  executionProvider?: "wasm" | "webgpu";
  allowCpuFallback?: boolean;
  config?: WebParseConfig;
  signal?: AbortSignal;
}
interface ParseOptions { signal?: AbortSignal }
interface DocParser {
  parse(pdf: Uint8Array, options?: ParseOptions): Promise<DocumentResult>;
  render(document: DocumentResult, format: "json" | "text" | "markdown"): Promise<string>;
  close(): Promise<void>;
}
declare function createParser(options: WebParserOptions): Promise<DocParser>;
```

createParser resolves only after real initialization. Each parser accepts one parse/render operation; additional concurrent calls fail with ParserBusy rather than entering an unbounded queue. render uses the creation-time OutputConfig and existing renderers without rerunning inference. The initial JS facade does not add overlay-management APIs.

Return JSON-compatible objects, arrays, strings, finite numbers, and null. Do not expose BTreeMap as JS Map or add platform-specific canonical fields.

Copy the caller's exact Uint8Array byteOffset/byteLength range before transfer; never detach caller-owned input unexpectedly. Resolve model URLs against the calling page before sending them to the Worker. Runtime assets default to the SDK location. Worker fetches add no custom authentication headers and do not log credential-bearing URLs; callers can fetch protected resources themselves and supply bytes.

PostMessage payloads contain owned serializable values, not PDFium/ORT handles, Rust references, File objects, or callbacks. Request IDs and per-instance Worker ownership prevent late replies from settling later requests. Remove AbortSignal listeners after each initialization/request lifetime. No zero-copy guarantee spans JS, Worker, Rust WASM, and ORT WASM.

| State | Accepted actions and transitions |
|---|---|
| Initializing | Initialization completes to Ready, fails to Failed, or cancellation/close reaches Closed |
| Ready | parse/render reaches Busy; close reaches Closed |
| Busy | Completion/recoverable failure returns Ready; fatal failure reaches Failed; cancellation/close reaches Closed |
| Failed | close is allowed; reuse requires a new parser |
| Closed | Repeated close succeeds; other operations reject with ParserClosed |

An already-aborted request does not destroy a ready parser. Aborting active initialization/parse or closing a busy parser rejects pending work and terminates its Worker. Reuse requires explicit createParser; there is no automatic background redownload/reinitialization.

Internal pipeline failures still use orderly cleanup. Public cancellation uses Worker termination so it can interrupt a Worker occupied by synchronous FFI/CPU work without waiting for a cancellation message.

Handle Worker error/unhandledrejection and host error/messageerror. Rust panic/WASM traps must settle active requests through fatal handling. After OOM/trap, do not promise partial results or reuse. An unloaded page receives best-effort cleanup rather than a guarantee about observing Promises in a destroyed realm.

## 13. Providers and distribution

Preserve native CPU/CUDA/CoreML/OpenVINO selection and explicit accelerator-initialization failures. Web executionProvider=wasm maps to shared CPU semantics; WebGpu is an explicit shared enum variant, not a cfg-gated business type.

Reject WebGpu on native and native providers on Web. WebGPU fallback is disabled unless allowCpuFallback=true. Only capability/provider initialization failures permit CPU fallback; hash, schema, PDF, and inference-data errors do not. Log the actual selected provider and validate GPU combinations independently.

Self-host matching JS/mjs/wasm runtime versions. The distributable includes Worker, Rust WASM, wasm-bindgen glue, WASI adapter, ORT assets, and licenses. Models remain separate artifacts.

Disable ORT telemetry. Document same-origin/CORS, CSP, WASM MIME, and WebGPU secure-context requirements. Initial CPU uses numThreads=1 and no ORT proxy Worker; basic use needs no cross-origin isolation. Any later multithreaded mode requires separate deployment acceptance.

## 14. Memory and performance requirements

Record Rust/PDFium WASM, ORT WASM, JS heap, and observable GPU allocations separately. A single memory capacity does not establish total process peak or post-GC retention.

Initial controls are one model session, one analyzed page, render queue length one, bounded image size, owned inputs, timely release of unnecessary pixels/download copies, and only two returned outputs. Preserve DocumentContext/prescan behavior; text and final results still grow with page count.

Check address-space/integer overflow and reject invalid sizes. Browser OOM is not a universally catchable business error, and arbitrary PDF sizes are not supported by promise. Stop reusing a resource-failed instance.

Reports should identify cold initialization, warm parsing, phase costs, input size, versions, hardware, and available memory measurements. Do not infer a speedup or SLA from isolated probes.

After warmup, repeat a fixed document at least 20 times and discard caller results. Live sessions, output tensors, queued requests, and handles must not accumulate. WASM allocator high-water capacity may remain. Where reliable post-GC retained-memory measurements are available, the last five-run median must not exceed the first five post-warmup median by more than max(16 MiB, 10%). Investigate sustained retention rather than classifying linear leaks as allocator capacity. Unavailable metrics must remain explicitly unverified.

## 15. Errors and logging

Retain thiserror and prefer From/TryFrom conversions at existing boundaries. Initialization errors include artifact fetch, missing explicit artifacts, hash/YAML/schema mismatch, and unavailable providers. Request errors include invalid configuration, unsupported Web concurrency, Busy, and closed parsers.

Document-open/page-count/context failures reject the document and clean up. Recoverable page failures retain existing continue_on_page_error, warning/error, and fallback behavior. Cancellation and fatal Worker/task failures are terminal, not successful partial DocumentResults. Unknown backend errors must not become empty successful detections.

Convert JS exceptions into owned diagnostic information at the compatibility boundary. Preserve useful causes without storing JsValue in Send/Sync source chains. Native errors may retain typed source chains.

Log meaningful initialization, artifact validation, document lifecycle, page failures/degradation, Worker termination, and resource-cleanup failures. Use fully qualified tracing event macros and normal formatting arguments. Spans may retain minimal correlation fields. Do not log tokens, pixels, characters, tensor elements, document/model bodies, credentials, authentication headers, or full signed URLs.

## 16. Cargo and build matrix

All dependency versions and paths belong in root workspace.dependencies. Child crates inherit and add only needed features, optional/default-feature settings, or target conditions.

Root ort features contain shared std/ndarray/tracing only. Native adds api-28, binary download/copy, and rustls support. Web adds api-17 and alternative-backend. Root Tokio contains shared macros/sync; native adds runtime/time capabilities. Browser-only crates can declare browser libraries normally; shared crates keep them target-specific. Pin wasm-bindgen CLI compatibility and inspect the complete transitive feature graph.

Feature propagation is Web dependency declarations to core, layout/config/PDFium, then pdfium-sys. The Web crate has no redundant wasm switch. Do not use mutually exclusive --all-features to validate providers.

```sh
rtk cargo check --locked
rtk cargo test --locked
rtk cargo check -p docparse-core --features wasm --locked
rtk cargo check -p docparse-core --target wasm32-unknown-unknown --features wasm --locked
rtk cargo clippy -p docparse-web --target wasm32-unknown-unknown --locked -- -D warnings
rtk cargo build -p docparse-web --target wasm32-unknown-unknown --release --locked
```

Verify metadata includes Web in workspace_members but excludes it from workspace_default_members. Test missing shared wasm features, unsupported Web targets, and Web/native-provider conflicts. Native+wasm must retain native Send/Sync and avoid alternative-backend; Web must avoid native api-28/download-binaries/rt-multi-thread. Release linking, wasm-bindgen, packaging, import checks, and browser execution are required beyond cargo check.

## 17. Test organization and acceptance

Rust tests live in crate tests directories or exact `#[cfg(test)] mod tests` modules. Do not add production fields/helpers solely for tests. Target-specific compile samples belong in tests/fixtures and are compiled by a dedicated driver; ordinary native tests need neither a browser nor a WASM target.

Browser tests use the production SDK, real PDFium/model, and the shared Rust preprocessing. Fake engines remain appropriate for algorithm/unit contracts but cannot replace end-to-end browser acceptance. The local browser server only serves resources and saves reports. It performs no parsing. A future WebUI integration must use its production app backend under repository rules.

Required coverage includes type bounds, configuration merging, altered artifacts, PDF text/RGB/geometry, Chinese and embedded fonts, malformed resources, actual ORT output selection/synchronization, full context/fusion/relations, cancellation/late replies/close/recreation, deployment under relocated paths, compatible runtime assets, and optional GPU failure/fallback.

Preserve native font choice. Embedded fonts support strict numeric/geometry parity. Record unembedded-font differences separately while still requiring text preservation, functional parsing, and per-platform determinism.

Result comparison requirements:

- Fix PDF/model/config/PDFium/ORT/browser versions and verify input hashes.
- Preserve schema meaning, original text, page numbering, error categories, and structure unaffected by floating-point variation.
- Shared algorithms given identical extracted facts and ModelOutputs must produce identical canonical results.
- PDFium coordinate tolerance is 1e-3 pt; text characters/order remain exact.
- Identical fixed images through Rust preprocessing require f32 absolute error at most 1e-6.
- Non-boundary detections match labels/counts; same-class IoU matching requires IoU at least 0.99 and confidence error at most 1e-3.
- Isolate scores within 1e-3 of thresholds and near-tied ordering cases. Do not hide differences by discarding detections, rewriting IDs, skipping pages, or globally relaxing comparisons.
- Stable end-to-end fixtures preserve ownership and reading order, with documented geometry tolerance. Known numeric edge weights inside diagnostics may use the same tolerance while their surrounding identifiers and reasons remain exact.
- Repeated canonical output is deterministic within one environment; timings and provider logs do not change canonical hashes.

Before release, require native regression and target/cfg checks, real Worker PDF-to-DocumentResult execution, shared ResultValidator/native comparisons, two actual fetches, explicit terminal failures, PDFium recovery/resource tests, reproducible assets/tools, provenance/licenses, and an honest tested-browser matrix. Compilation alone, fake models, boxes-only output, or main-thread probes do not satisfy acceptance.

## 18. Delivery stages and stop conditions

| Stage | Exit criterion |
|---|---|
| A: Pinned runtime | Real Worker loads/runs the fixed model, reports failures, and forwards fetches |
| B: Platform boundaries | Type/build matrix, native regressions, and cfg enforcement pass |
| C: Shared parsing | Real single/multipage PDFs produce valid results and native parity |
| D: Browser package | Lifecycle, memory, relocated deployment, and reproducible builds pass |
| E: Optional targets | Each GPU/browser combination passes independently |

Do not claim delivery while startup needs fabricated DOM objects, validation must be skipped, malformed PDFs irrecoverably corrupt the runtime, native thread safety/concurrency is lost, real-model output is unverified, or resources grow cumulatively across repeated use. Preserve failure evidence and resolve the affected boundary.

## 19. Coding and documentation

Write all code comments and project documentation in English. Test fixture text may retain its source language. New nontrivial logic needs explanatory comments, and every new function needs a function-level comment. Use typed-builder for structs with more than three fields and builder defaults for Option fields. Clone shared fields with Arc::clone.

Prefer associated methods and From/TryFrom/traits to many small free helpers. Update public API migration examples whenever engine signatures, validation responsibilities, or lifecycle semantics change. Do not add speculative abstractions or unused dependencies.

Keep one root THIRD_PARTY_NOTICES.md. Crate NOTICE files refer to that root document; the legal sync script copies only LICENSE and NOTICE. Preserve third-party licenses required in distributable runtime assets.

## 20. References

- [Existing layout-fusion design](./2026-09-04-layout-text-fusion-design.md)
- [ort Web backend documentation](https://ort.pyke.io/backends/web)
- [Pinned ort-web patch provenance](../../../vendor/ort-web/UPSTREAM.md)
- [ONNX Runtime Web environment/session options](https://onnxruntime.ai/docs/tutorials/web/env-flags-and-session-options.html)
- [Web package usage and reproduction](../../../packages/web/README.md)
- [Implementation and validation record](../reports/2026-09-07-native-web-wasm-validation.md)

Published examples and runtime behavior can differ. Version changes require fresh validation of the affected integration contracts.
