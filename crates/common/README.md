# docparse-common

Model-independent queues, thread ownership, platform execution, and request timing.
This crate depends on no DocParse model crate, configuration crate, or ONNX runtime.
Native owners preserve thread affinity and survive their construction Tokio runtime.
Ready batches never wait to fill; caller cancellation does not release active input buffers.

`session/manager.rs` coordinates model consumers; `session/worker.rs` preserves
single-session thread affinity. Both use `ThreadManager` for native ownership.
`Queue` provides asynchronous ready batches and `BlockingQueue` provides atomic
native packet admission. `TaskSet` and `spawn` own cancelable native/browser tasks;
`run_cpu`, `timeout`, portable future bounds, and timing contexts share this layer.

Session managers accept explicit queue capacity independently of consumer count
and batch size. Full queues backpressure producers, and short batches run immediately.

`PageQueue` counts reserved deliveries until the last `PageLease` is released.
Its task-local ownership scope is independent of logging. CPU submissions retain
leases through uncollected outputs, and model requests retain them through actual
execution after caller cancellation.
