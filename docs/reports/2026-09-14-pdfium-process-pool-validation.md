# PDFium process pool: local validation

Date: 2026-09-14. Host: macOS, aarch64. Scope: local CPU and WASM build validation.
Changes remain uncommitted on the benchmark branch.

## Delivered behavior

- The `docparse-core` package provides the feature-gated `docparse-pdfium-worker` binary from `src/bin/pdfium_worker.rs`; the PDFium implementation is grouped under `src/pdfium/`.
- The server starts the fixed-size pool with `tokio::process` and uses `ipc-channel` for document operations.
- `server.pdfium_max_workers` defaults to 1. Startup, leased, idle, and unreaped processes all count toward the limit.
- The server and worker are installed together; ordinary core/CLI/Web callers keep the local provider.
- Model engines stay in the server. Workers reuse the existing PDFium extraction and rendering functions.
- Cancelled operations retire uncertain sessions even if the session object remains alive. Cancelling a queued operation leaves the active request alone.
- Invalid and empty PDFs reuse healthy workers. Intentional cancellation does not consume the crash-restart budget; repeated actual crashes stop admission.
- Parent-owned temporary directories clean bootstrap rendezvous files even when startup fails before the child can remove them.

## Checks

| Check | Observed result |
| --- | --- |
| Config, layout, core, server, worker tests | 366 passed; 20 ignored in the default run; 42 suites |
| Explicit real-layout parser parity test | Passed for table, rotated CJK, and glyph-recovery PDFs |
| Workspace pre-commit Clippy command (`cargo clippy --tests --examples -- -Dwarnings`) | Passed |
| Web `wasm32-unknown-unknown` Clippy with warnings denied | Passed |
| `cargo fmt --all -- --check` | Passed |
| Platform cfg boundary check | Passed |
| Server, worker, and benchmark executable builds | Passed |

The opt-in real-layout parity test was run separately from the default suite. Remaining
opt-in tests were not claimed as executed. Browser UI lifecycle acceptance and CUDA
performance testing were not run in this local delivery.

The real-process tests cover N=1/2/4 admission, simultaneous open documents, path and
memory input, text/raster parity, glyph callbacks and callback failure, individual
request cancellation, session cancellation, startup cancellation, busy shutdown,
invalid configuration, malformed/empty PDFs, bad handshakes, bootstrap file cleanup,
worker death, replacement limits, and PID reaping. The checks use real worker processes.

## CPU runner smoke

The runner used three fixtures totaling six pages, document concurrency 4, and three
measured repetitions per process-count case. OCR was explicitly disabled and TSR used
rules-only mode. Layout used the installed production model with the CPU backend and a
debug executable. Each PDFium process was explicitly warmed, shared models received
the normal warmup, and input files were already in the OS cache.

| Configured workers | Observed worker PIDs | Repetitions | Document executions | Pages | Failed/degraded documents |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 3 | 9 | 18 | 0 |
| 2 | 2 | 3 | 9 | 18 | 0 |
| 4 | 4 | 3 | 9 | 18 | 0 |

This validates the runner and process-count controls, not a controlled throughput
comparison. A real-model parity check overlapped part of the smoke run, so no speedup
or optimal-concurrency conclusion is drawn from its elapsed times. GPU telemetry was
unavailable; the metrics explicitly identify the CPU backend. Per-PID RSS includes
shared mappings and is not unique physical memory consumption.

- [Portable smoke summary](2026-09-14-pdfium-process-pool-smoke.json)
- [Local raw artifacts](../../target/pdfium-pool-smoke/final-results/)
- [Implementation spec](../superpowers/specs/2026-09-14-pdfium-process-pool-design.md)
- [Server build and deployment instructions](../../crates/server/README.md)

## Reproduction

```sh
rtk cargo build -p docparse-core -p docparse-server --features docparse-core/pdfium-ipc --bins --examples
rtk cargo test -p docparse-config -p docparse-layout -p docparse-core -p docparse-server
rtk cargo test -p docparse-server --test pdfium_pool real_layout_parser_preserves_canonical_results -- --ignored
rtk cargo clippy --tests --examples -- -Dwarnings
rtk cargo clippy -p docparse-web --target wasm32-unknown-unknown --locked -- -D warnings
rtk cargo fmt --all -- --check
rtk proxy python3 scripts/check_wasm_compat.py
```

The [benchmark instructions](../../scripts/README.md) describe companion placement and
independent document/process concurrency controls. Production CPU/GPU tuning still
requires a representative corpus and isolated measurements on the deployment hardware.

## Module and binary packaging follow-up

PDFium executor/provider/IPC code is now grouped under `core/src/pdfium`,
with declarations and exports in `mod.rs`. The worker is a core bin target gated
by `pdfium-ipc`; the separate worker package has been removed. The executable
name, public provider interfaces, and `docparse_core::pdfium_ipc` alias are unchanged.

Cargo metadata confirmed one core worker bin and no worker package. Its explicit
build, the default core build without features, the full local regression suite
(366 passed, 20 ignored), and a subsequent core/server regression run (302 passed,
17 ignored) succeeded. Native/WASM Clippy, formatting, and platform-boundary checks
also passed. No performance conclusion is added for this structural change.

The PDF input and local worker platform adapters have also moved into
`core/src/pdfium/input.rs` and `worker.rs`. Existing public type exports and IPC
aliases remain available; references to the low-level dependency use explicit
`::pdfium` paths to distinguish it from the core module. After this consolidation,
302 core/server tests passed (17 ignored), all seven Python boundary-policy tests
passed, and the worker build, default core check, Native/WASM Clippy, formatting,
and platform-boundary scan passed.
