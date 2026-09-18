# docparse-layout

Neutral `LayoutEngine` and geometry APIs with pinned PP-DocLayoutV3 ONNX inference.
Supports CPU and mutually exclusive `cuda`, `coreml`, and `openvino` features.
Explicit accelerator initialization failures are returned without silent fallback.
Model artifacts and Python output oracles are documented in the root README.

`layout.session_size` creates 1–8 independent consumers (default 1) sharing one
bounded queue. Required `layout.queue_size` bounds pending pages independently
of sessions. `layout.batch_size` caps ready pages per inference (1–32, default 1).
An idle consumer takes available pages immediately, combines their tensors, and
splits output boxes using per-page counts. It never waits for a full batch.

Native sessions create, run, and destroy their model on the same dedicated thread.
They outlive their construction runtime. Finite submission waits retain model
ownership through cancellation; final shutdown closes admission and joins all
owners. Browser sessions consume the same queue and keep the global ORT guard
through output readback.
