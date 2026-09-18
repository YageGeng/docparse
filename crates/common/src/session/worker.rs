//! Dedicated native session execution and cancellation-safe ownership.
use crate::{TaskError, ThreadManager, run_cpu};
use std::sync::Arc;

type SessionOperation<S> = Box<dyn FnOnce(&mut S) + Send>;

/// Keeps model initialization, execution, and destruction on one independently owned native thread.
pub struct SessionWorker<S> {
    owner: Arc<SessionOwner<S>>,
}

/// Closes the queue before joining; finite initialization and inference tasks retain this owner through cancellation.
struct SessionOwner<S> {
    sender: Option<tokio::sync::mpsc::Sender<SessionOperation<S>>>,
    _threads: ThreadManager,
}

impl<S> Drop for SessionOwner<S> {
    /// Joins native destruction and thread-local cleanup before the last owner can finish dropping.
    fn drop(&mut self) {
        drop(self.sender.take());
    }
}

impl<S: 'static> SessionWorker<S> {
    /// Waits only for initialization in the blocking pool, leaving idle model ownership independent of Tokio.
    pub async fn new<F, E>(initialize: F) -> Result<Self, E>
    where
        F: FnOnce() -> Result<S, E> + Send + 'static,
        E: From<TaskError> + Send + 'static,
    {
        // Initialization may use a local subscriber, but its context must end before this shared actor starts serving calls.
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let initialize = move || {
            tracing::dispatcher::with_default(&dispatcher, || {
                span.in_scope(initialize)
            })
        };
        run_cpu(move || {
            let (sender, mut requests) =
                tokio::sync::mpsc::channel::<SessionOperation<S>>(1);
            let (ready, initialized) = tokio::sync::oneshot::channel();
            let mut threads = ThreadManager::default();
            threads.spawn("docparse-onnx".into(), move || {
                let mut session = match initialize() {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while let Some(operation) = requests.blocking_recv() {
                    operation(&mut session);
                }
            })?;
            // Install ownership before waiting: cancellation and initialization errors must also close and join the thread.
            let worker = Self {
                owner: Arc::new(SessionOwner {
                    sender: Some(sender),
                    _threads: threads,
                }),
            };
            initialized.blocking_recv().map_err(|_closed| {
                TaskError::from_message("model initialization thread stopped")
            })??;
            Ok::<_, E>(worker)
        })
        .await
        .map_err(E::from)?
    }

    /// Owns each input closure through actual execution, skipping queued work whose caller has already cancelled.
    pub async fn run<F, R>(&self, operation: F) -> Result<R, TaskError>
    where
        F: FnOnce(&mut S) -> R + Send + 'static,
        R: Send + 'static,
    {
        let owner = Arc::clone(&self.owner);
        // This receiver belongs to the async caller, so queued requests still observe cancellation.
        let (caller, _caller_lifetime) = tokio::sync::oneshot::channel::<()>();
        // Capture each invocation separately because the same session thread serves different PDFs.
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        run_cpu(move || {
            let (response, result) = tokio::sync::oneshot::channel();
            owner
                .sender
                .as_ref()
                .ok_or_else(|| {
                    TaskError::from_message("model execution thread stopped")
                })?
                .blocking_send(Box::new(move |session| {
                    tracing::dispatcher::with_default(&dispatcher, || {
                        span.in_scope(|| {
                            if caller.is_closed() {
                                // Release captured leases before waking the waiter that owns the thread's join handle.
                                drop(operation);
                                return;
                            }
                            let value = operation(session);
                            let _ = response.send(value);
                        })
                    })
                }))
                .map_err(|_closed| {
                    TaskError::from_message("model execution thread stopped")
                })?;
            // Only finite work occupies Tokio's pool. Retain the owner until native work releases its captures,
            // even when the caller is cancelled, so the session can never try to join its own thread.
            result.blocking_recv().map_err(|_closed| {
                TaskError::from_message("model execution response lost")
            })
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionWorker, TaskError, run_cpu};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    /// Idle sessions must leave a one-thread blocking pool available for CPU work and another model.
    #[test]
    fn idle_sessions_leave_blocking_capacity() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let first = SessionWorker::new(|| Ok::<_, TaskError>(()))
                .await
                .expect("first session");
            let cpu =
                tokio::time::timeout(Duration::from_secs(1), run_cpu(|| 42))
                    .await;
            let second = tokio::time::timeout(
                Duration::from_secs(1),
                SessionWorker::new(|| Ok::<_, TaskError>(())),
            )
            .await;
            // Close the idle queue before asserting so the old implementation can finish shutdown.
            drop(first);
            assert_eq!(
                cpu.expect("CPU work must not wait for idle sessions")
                    .expect("CPU work"),
                42
            );
            drop(
                second
                    .expect("another session must initialize")
                    .expect("second session"),
            );
        });
    }

    /// A model can leave its construction runtime and continue serving a later synchronous caller.
    #[test]
    fn session_survives_construction_runtime() {
        let (models, model) = std::sync::mpsc::channel();
        let (finished, exited) = std::sync::mpsc::channel();
        let host = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("construction runtime");
            let session = runtime
                .block_on(SessionWorker::new(|| Ok::<_, TaskError>(42)))
                .expect("session");
            models.send(session).expect("transfer model");
            drop(runtime);
            finished.send(()).expect("runtime exited");
        });
        let session =
            model.recv_timeout(Duration::from_secs(5)).expect("model");
        let closed = exited.recv_timeout(Duration::from_secs(1));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("caller runtime");
        let value = runtime
            .block_on(session.run(|value| *value))
            .expect("inference");
        drop(session);
        host.join().expect("runtime host");
        assert!(
            closed.is_ok(),
            "construction runtime must close while the model is still owned"
        );
        assert_eq!(value, 42);
    }

    /// Holds session destruction at a deterministic barrier while its owning runtime tries to exit.
    struct BlockingDrop {
        owner: std::thread::ThreadId,
        started: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Drop for BlockingDrop {
        /// Checks destruction affinity and prevents cleanup from finishing before the test releases it.
        fn drop(&mut self) {
            assert_eq!(self.owner, std::thread::current().id());
            self.started.send(()).expect("destruction started");
            self.release
                .recv_timeout(Duration::from_secs(5))
                .expect("release destruction");
        }
    }

    /// Runtime shutdown must wait for session destruction rather than returning while native cleanup is blocked.
    #[test]
    fn runtime_shutdown_waits_for_session_destruction() {
        let (started, destroying) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let (finished, exited) = std::sync::mpsc::channel();
        let host = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let worker = SessionWorker::new(move || {
                    Ok::<_, TaskError>(BlockingDrop {
                        owner: std::thread::current().id(),
                        started,
                        release: blocked,
                    })
                })
                .await
                .expect("session");
                drop(worker);
            });
            drop(runtime);
            finished.send(()).expect("runtime exited");
        });
        destroying
            .recv_timeout(Duration::from_secs(5))
            .expect("session is being destroyed");
        let premature = exited.recv_timeout(Duration::from_millis(100));
        // Release native cleanup before asserting, so the failing implementation cannot leave a blocked thread behind.
        release.send(()).expect("release cleanup");
        host.join().expect("runtime host");
        assert!(matches!(
            premature,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
    }

    /// Cancelling initialization must keep its native work owned until it has actually returned.
    #[test]
    fn runtime_shutdown_waits_for_cancelled_initialization() {
        let (entered, initializing) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let (cancelled, cancelling) = std::sync::mpsc::channel();
        let (finished, exited) = std::sync::mpsc::channel();
        let host = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let task = tokio::spawn(SessionWorker::new(move || {
                    entered.send(()).expect("initialization entered");
                    blocked
                        .recv_timeout(Duration::from_secs(5))
                        .expect("release initialization");
                    Ok::<_, TaskError>(())
                }));
                initializing.await.expect("initializing");
                task.abort();
                assert!(
                    matches!(task.await, Err(error) if error.is_cancelled())
                );
            });
            cancelled.send(()).expect("caller cancelled");
            drop(runtime);
            finished.send(()).expect("runtime exited");
        });
        cancelling
            .recv_timeout(Duration::from_secs(5))
            .expect("initialization cancelled");
        let premature = exited.recv_timeout(Duration::from_millis(100));
        release.send(()).expect("release initialization");
        host.join().expect("runtime host");
        assert!(matches!(
            premature,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
    }

    /// Flattening initialization errors must retain the concrete model failure rather than reclassifying it as a thread failure.
    #[tokio::test]
    async fn session_initialization_preserves_model_error() {
        #[derive(Debug, thiserror::Error)]
        enum InitError {
            #[error("model initialization rejected")]
            Model,
            #[error(transparent)]
            Task(#[from] TaskError),
        }
        let result = SessionWorker::<()>::new(|| Err(InitError::Model)).await;
        assert!(matches!(result, Err(InitError::Model)));
    }

    /// Multiple async callers must execute on the same thread that initialized the session.
    #[tokio::test]
    async fn session_worker_preserves_thread_affinity() {
        let worker = Arc::new(
            SessionWorker::new(|| {
                Ok::<_, TaskError>(std::thread::current().id())
            })
            .await
            .expect("session"),
        );
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let worker = Arc::clone(&worker);
            tasks.spawn(async move {
                worker
                    .run(|owner| (*owner, std::thread::current().id()))
                    .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            let (initialized, executing) =
                result.expect("join").expect("execution");
            assert_eq!(initialized, executing);
        }
    }

    /// Tracks real actor-state destruction without adding any test-only production fields.
    struct Lifetime {
        alive: Arc<AtomicBool>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl Drop for Lifetime {
        /// Signals only after the in-flight operation and its session have actually finished.
        fn drop(&mut self) {
            self.alive.store(false, Ordering::SeqCst);
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    /// Cancelling a caller must not destroy a session while native inference is still using it.
    #[tokio::test]
    async fn cancellation_retains_running_session_until_completion() {
        let alive = Arc::new(AtomicBool::new(true));
        let state_alive = Arc::clone(&alive);
        let (dropped, destroyed) = tokio::sync::oneshot::channel();
        let worker = Arc::new(
            SessionWorker::new(move || {
                Ok::<_, TaskError>(Lifetime {
                    alive: state_alive,
                    dropped: Some(dropped),
                })
            })
            .await
            .expect("session"),
        );
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let caller = Arc::clone(&worker);
        // Real layout requests capture a pool that also owns this worker; cancellation must not cause self-join.
        let captured_worker = Arc::clone(&worker);
        let task = tokio::spawn(async move {
            caller
                .run(move |_state| {
                    let _ = started.send(());
                    blocked.recv().expect("release native operation");
                    drop(captured_worker);
                })
                .await
        });
        entered.await.expect("operation started");
        // A queued cancelled request must be discarded before invoking its captured input closure.
        let queued_ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&queued_ran);
        let queued_worker = Arc::clone(&worker);
        let queued = tokio::spawn(async move {
            queued_worker
                .run(move |_state| flag.store(true, Ordering::SeqCst))
                .await
        });
        while worker.owner.sender.as_ref().expect("sender").capacity() != 0 {
            tokio::task::yield_now().await;
        }
        queued.abort();
        assert!(
            queued
                .await
                .expect_err("queued caller cancelled")
                .is_cancelled()
        );
        task.abort();
        assert!(task.await.expect_err("caller cancelled").is_cancelled());
        drop(worker);
        assert!(alive.load(Ordering::SeqCst));
        release.send(()).expect("unblock native operation");
        destroyed.await.expect("session eventually dropped");
        assert!(!alive.load(Ordering::SeqCst));
        assert!(!queued_ran.load(Ordering::SeqCst));
    }
}
