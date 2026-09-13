# docparse-web

The browser-only DocParse ABI supports `wasm32-unknown-unknown`. Exports live directly in `src/lib.rs`. Dependency declarations enable the shared libraries' `wasm` features; this crate has no platform feature switch or native shell.

The workspace includes this crate through `members = ["crates/*"]`, but `default-members` selects only the native crates, including the independent TSR crate. Root-level `cargo build/check/test/clippy` commands therefore omit it. Explicit `--workspace` commands select all members; add `--exclude docparse-web` for native checks.

```sh
rtk cargo build -p docparse-web --target wasm32-unknown-unknown --release --locked
```

Use `createParser` from `packages/wasm-web` to manage the Worker, pinned runtime assets, models, cancellation, and shutdown. Direct wasm-bindgen exports do not provide the main-thread lifecycle protections.

See the [Web package guide](../../packages/wasm-web/README.md) for build, deployment, configuration, and real-browser acceptance instructions. Models are not included in the crate.

JavaScript calls are centralized in `src/js.rs`. The pinned js-sys property and
callback APIs are safe Rust APIs with caught JS exceptions; business code does
not need unsafe blocks. `ValueExt` checks property writes and normalizes thrown
values, while `FunctionExt` handles synchronous callbacks and Promise/thenable
results. Pixel transfers copy into JS-owned storage. The crate denies unsafe
code, retaining only the existing C ABI export exceptions in `src/libc.rs`.

After building and serving the production SDK, exercise the boundary with a real
single-page table PDF:

```sh
rtk proxy node crates/web/tests/js_boundary.mjs /absolute/path/table-page.pdf
```

The probe covers failed property writes, throwing getters, invalid/throwing
callbacks, Promise rejection, plain values, thenables and owned RGB copies.

The ABI constructs `ParserArtifacts` and delegates model initialization to the
shared core builder. It does not maintain a second layout/TSR initialization
sequence. Model errors retain their categories for explicit GPU fallback.
