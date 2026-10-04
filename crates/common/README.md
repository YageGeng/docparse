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
Native producers use one FIFO semaphore for synchronous and asynchronous admission;
only the assigned waiters wake when consumers release slots.

`ResourceLease` carries pre-allocation and document permits through task scopes,
CPU work, image ownership, and model requests. Nested leases retain their parent
budget, so canceling a waiter does not recycle still-live resources. `ResourceBudget`
keeps immutable configured capacity together with its shared semaphore, so a busy
provider's scheduling window remains stable. TSR crop pixels and prediction tensors
have independent budgets; every prediction reserves fresh tensor capacity.

`PageQueue` counts reserved deliveries until the last `PageLease` is released.
Its task-local ownership scope is independent of logging. CPU submissions retain
leases through uncollected outputs, and model requests retain them through actual
execution after caller cancellation.

On native targets, `run_cpu` uses a separate process-owned Tokio blocking pool,
limited to `max(1, available_parallelism - 1)` concurrent operations. Admission is
asynchronous. Canceled inputs waiting for admission and completed but uncollected
outputs are destroyed on their CPU pool, never synchronously by the canceling
executor. Admitted work retains its permit through output destruction; task
tracking also covers cleanup before admission. `drain_cpu()` waits for both. It
does not consume the calling runtime's filesystem/blocking workers. CPU closures
must finish without waiting for another `run_cpu` operation. Applications stop
producers and call `drain_cpu()` before process shutdown. Native initialization
uses `run_blocking` instead, preserving the calling runtime's cleanup boundary.
Model admission and replies wait asynchronously; only actual model execution owns
a native session thread.

Native `run_http_cpu` shares these ownership guarantees but uses an independent
pool capped at `min(2, available_parallelism)` workers. Upload hashing, result
index decoding, and cache generation use that pool so parser saturation cannot
hold their admission. HTTP computation still shares its own finite capacity;
neither pool reserves physical CPU time. `drain_cpu()` drains both pools after
applications stop their producers.

Thread cleanup excludes the originating thread before dispatching joins. A worker
may release its own `ThreadManager` via drop or `shutdown()` without scheduling a
join that would cycle with that worker's runtime shutdown.
