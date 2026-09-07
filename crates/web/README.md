# docparse-web

The browser-only DocParse ABI supports `wasm32-unknown-unknown`. Exports live directly in `src/lib.rs`. Dependency declarations enable the shared libraries' `wasm` features; this crate has no platform feature switch or native shell.

The workspace includes this crate through `members = ["crates/*"]`, but `default-members` selects only the six native crates. Root-level `cargo build/check/test/clippy` commands therefore omit it. Explicit `--workspace` commands select all members; add `--exclude docparse-web` for native checks.

```sh
rtk cargo build -p docparse-web --target wasm32-unknown-unknown --release --locked
```

Use `createParser` from `packages/web` to manage the Worker, pinned runtime assets, models, cancellation, and shutdown. Direct wasm-bindgen exports do not provide the main-thread lifecycle protections.

See the [Web package guide](../../packages/web/README.md) for build, deployment, configuration, and real-browser acceptance instructions. Models are not included in the crate.
