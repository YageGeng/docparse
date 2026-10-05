//! Portable task contracts and finite CPU execution.
use std::{future::Future, pin::Pin};

#[cfg(not(target_arch = "wasm32"))]
/// Requires values to be transferable between native threads.
pub trait WasmCompatSend: Send {}
#[cfg(target_arch = "wasm32")]
/// Marks values confined to a single browser Worker.
pub trait WasmCompatSend {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> WasmCompatSend for T {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> WasmCompatSend for T {}

#[cfg(not(target_arch = "wasm32"))]
/// Requires shared native values to support concurrent access.
pub trait WasmCompatSync: Sync {}
#[cfg(target_arch = "wasm32")]
/// Marks shared values confined to a single browser Worker.
pub trait WasmCompatSync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> WasmCompatSync for T {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> WasmCompatSync for T {}

#[cfg(not(target_arch = "wasm32"))]
/// A boxed future that preserves the native Send requirement.
pub type WasmBoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
/// A boxed future polled locally inside the current browser Worker.
pub type WasmBoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Owned task failure that never contains browser handles.
#[derive(Debug, thiserror::Error)]
#[error("runtime task failed: {0}")]
pub struct TaskError(pub(crate) String);

impl TaskError {
    /// Converts an owned platform failure into a portable task error.
    pub fn from_message(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use super::*;
    use std::sync::LazyLock;

    /// CPU work has its own finite pool so file I/O never queues behind inference preparation.
    struct CpuPool {
        runtime: tokio::runtime::Runtime,
        // Cancelled-work destruction gets separate threads so admitted work never queues behind it.
        cleanup: tokio::runtime::Runtime,
        permits: tokio::sync::Semaphore,
        tasks: tokio_util::task::TaskTracker,
    }

    impl CpuPool {
        /// Builds an independent finite pool whose drain includes canceled input and output cleanup.
        fn new(
            name: &'static str,
            threads: usize,
        ) -> Result<Self, std::io::Error> {
            // Only spawn_blocking uses this runtime; no async worker or reactor is needed.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(threads)
                .thread_name(name)
                .build()?;
            // Cleanup bypasses admission, so it must not consume the admitted pool's blocking threads.
            let cleanup = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(threads)
                .thread_name(format!("{name}-cleanup"))
                .build()?;
            let tasks = tokio_util::task::TaskTracker::new();
            // Closed trackers still accept tokens; wait() becomes a reusable emptiness barrier.
            tasks.close();
            tracing::info!(
                "initialized isolated CPU pool {} with {} threads",
                name,
                threads
            );
            Ok(Self {
                runtime,
                cleanup,
                permits: tokio::sync::Semaphore::new(threads),
                tasks,
            })
        }

        /// Retains inputs before admission and outputs until collection or background destruction.
        async fn run<F, T>(&'static self, operation: F) -> Result<T, TaskError>
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static,
        {
            // A span enters its own subscriber but does not install its default dispatcher.
            let span = tracing::Span::current();
            let dispatcher = tracing::dispatcher::get_default(Clone::clone);
            let caller = tokio::runtime::Handle::try_current().ok();
            let pending = CpuOwned {
                value: Some((
                    operation,
                    crate::PageLease::current(),
                    crate::ResourceLease::current(),
                    self.tasks.token(),
                )),
                pool: self,
                caller: caller.clone(),
            };
            let permit = self
                .permits
                .acquire()
                .await
                .expect("CPU admission stays open");
            self.runtime
                .spawn_blocking(move || {
                    let (operation, lease, resources, token) =
                        pending.into_inner();
                    let _caller =
                        caller.as_ref().map(tokio::runtime::Handle::enter);
                    let output =
                        tracing::dispatcher::with_default(&dispatcher, || {
                            // Restore model/document admission before constructors capture resource ownership.
                            let operation = || match &resources {
                                Some(resources) => {
                                    resources.scope_sync(operation)
                                }
                                None => operation(),
                            };
                            if let Some(lease) = &lease {
                                return lease
                                    .scope_sync(|| span.in_scope(operation));
                            }
                            span.in_scope(operation)
                        });
                    CpuOwned {
                        // Tuple fields drop in order, retaining admission and drain ownership through cleanup.
                        value: Some((output, lease, resources, permit, token)),
                        pool: self,
                        caller: caller.clone(),
                    }
                })
                .await
                .map(|output| {
                    let (output, _lease, _resources, _permit, _token) =
                        output.into_inner();
                    output
                })
                .map_err(TaskError::from)
        }
    }

    /// Dropping a future may destroy a completed JoinHandle on the executor; keep that destruction off-thread.
    struct CpuOwned<T: Send + 'static> {
        value: Option<T>,
        pool: &'static CpuPool,
        caller: Option<tokio::runtime::Handle>,
    }

    impl<T: Send + 'static> CpuOwned<T> {
        /// Transfers a normally consumed value without scheduling any cleanup task.
        fn into_inner(mut self) -> T {
            self.value.take().expect("CPU-owned value")
        }
    }

    impl<T: Send + 'static> Drop for CpuOwned<T> {
        /// Cleanup bypasses admission because an uncollected output may still hold the last permit.
        fn drop(&mut self) {
            if let Some(value) = self.value.take() {
                let caller = self.caller.take();
                drop(self.pool.cleanup.spawn_blocking(move || {
                    // Native owners may schedule further joins; retain the originating shutdown boundary.
                    let _caller =
                        caller.as_ref().map(tokio::runtime::Handle::enter);
                    drop(value);
                }));
            }
        }
    }

    // Process-owned execution survives short-lived library callers and their cancellation.
    static CPU: LazyLock<Result<CpuPool, std::io::Error>> =
        LazyLock::new(|| {
            CpuPool::new(
                "docparse-cpu",
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .saturating_sub(1)
                    .max(1),
            )
        });
    // HTTP hashing and result projections must never queue behind parser admission.
    // ponytail: cap HTTP compute at two workers; tune only with measured HTTP CPU contention.
    static HTTP_CPU: LazyLock<Result<CpuPool, std::io::Error>> =
        LazyLock::new(|| {
            CpuPool::new(
                "docparse-http-cpu",
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .min(2),
            )
        });
    impl From<tokio::task::JoinError> for TaskError {
        /// Retains the native task failure without exposing Tokio in shared APIs.
        fn from(error: tokio::task::JoinError) -> Self {
            Self(error.to_string())
        }
    }

    /// Runs owned CPU work away from the native async executor.
    pub async fn run_cpu<F, T>(operation: F) -> Result<T, TaskError>
    where
        F: FnOnce() -> T + WasmCompatSend + 'static,
        T: WasmCompatSend + 'static,
    {
        let pool = CPU.as_ref().map_err(|error| {
            tracing::error!("failed to initialize CPU pool: {}", error);
            TaskError::from_message(error.to_string())
        })?;
        pool.run(operation).await
    }

    /// Runs HTTP request computation with capacity independent of parsing and filesystem I/O.
    pub async fn run_http_cpu<F, T>(operation: F) -> Result<T, TaskError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let pool = HTTP_CPU.as_ref().map_err(|error| {
            tracing::error!("failed to initialize HTTP CPU pool: {}", error);
            TaskError::from_message(error.to_string())
        })?;
        pool.run(operation).await
    }

    /// Keeps finite blocking work and abandoned outputs attached to the caller's resource lifetime.
    pub async fn run_blocking<F, T>(operation: F) -> Result<T, TaskError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let resources = crate::ResourceLease::current();
        tokio::task::spawn_blocking(move || {
            let output = tracing::dispatcher::with_default(&dispatcher, || {
                // Publication cleanup keeps its document admission even if supervision is cancelled.
                span.in_scope(|| match &resources {
                    Some(resources) => resources.scope_sync(operation),
                    None => operation(),
                })
            });
            // Uncollected outputs must be destroyed before their resource permits are returned.
            (output, resources)
        })
        .await
        .map(|(output, _resources)| output)
        .map_err(TaskError::from)
    }

    /// Drains both CPU pools after producers stop, including canceled admission and uncollected results.
    pub async fn drain_cpu() -> Result<(), TaskError> {
        for pool in [&*CPU, &*HTTP_CPU] {
            let pool = pool.as_ref().map_err(|error| {
                tracing::error!("failed to drain CPU pool: {}", error);
                TaskError::from_message(error.to_string())
            })?;
            pool.tasks.wait().await;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Duration;

        /// Blocks its destructor until released, modelling slow cleanup of a cancelled input.
        struct SlowDrop(std::sync::mpsc::Receiver<()>);
        impl Drop for SlowDrop {
            /// Holds the cleanup thread until the test releases it.
            fn drop(&mut self) {
                let _ = self.0.recv_timeout(Duration::from_secs(5));
            }
        }

        /// Slow cancelled-input cleanup must not occupy the only admitted CPU thread.
        #[tokio::test]
        async fn admitted_work_does_not_queue_behind_cleanup() {
            let pool: &'static CpuPool = Box::leak(Box::new(
                CpuPool::new("cleanup-test", 1).expect("pool"),
            ));
            // Hold the only permit so the next submission is cancelled before admission.
            let held = pool.permits.acquire().await.expect("permit");
            let (release, blocked) = std::sync::mpsc::channel();
            let guard = SlowDrop(blocked);
            let mut cancelled = Box::pin(pool.run(move || drop(guard)));
            assert!(futures_util::poll!(&mut cancelled).is_pending());
            // Dropping the waiting caller schedules the slow destructor as background cleanup.
            drop(cancelled);
            drop(held);
            let admitted =
                tokio::time::timeout(Duration::from_secs(2), pool.run(|| 7))
                    .await;
            release.send(()).expect("release cleanup");
            assert_eq!(
                admitted
                    .expect("admitted work waited behind cleanup")
                    .expect("result"),
                7
            );
        }
    }
}
#[cfg(target_arch = "wasm32")]
mod platform {
    use super::*;
    /// Executes a CPU segment within the dedicated browser Worker.
    pub async fn run_cpu<F, T>(operation: F) -> Result<T, TaskError>
    where
        F: FnOnce() -> T + WasmCompatSend + 'static,
        T: WasmCompatSend + 'static,
    {
        Ok(operation())
    }
}
pub use platform::run_cpu;
#[cfg(not(target_arch = "wasm32"))]
pub use platform::{drain_cpu, run_blocking, run_http_cpu};

/// Renders a panic payload as text so the original panic message is never lost.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}
