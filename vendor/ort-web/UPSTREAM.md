# Pinned ort-web patch

Baseline: crates.io `ort-web 0.3.0+1.27`, source revision `002f41a8e175eac7f6695ff361d2e51a50874c48`, directory `backends/web`.

Published archive SHA-256: `bf2469978623be74836e83c8d9910f9a45d7a36b4edf478ff7a0c309f184ae39`.

Local changes:

- `_loader.js`: use `globalThis` and load the ESM runtime in module Workers without DOM shims.
- `api.rs`: forward requested output names through the existing `run_with_fetches` binding.
- `session.rs` and `tensor.rs`: invoke JavaScript `release`/`dispose` when Rust owners drop; session release runs asynchronously on the local executor.
- `binding/tensor.rs`: suppress existing dead-code warnings for optional binding descriptors.

Cargo manifests and other source files match the published archive. LICENSE files and this provenance note are included; the upstream package's Cargo.lock and VCS metadata are omitted. Native builds do not depend on this package. Browser lifecycle tests exercise output selection and resource release.
