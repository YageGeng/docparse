//! Shared native session lifecycle, independent of model types.
use crate::queue::BlockingQueue;
use crate::{SessionRequest, TaskError, ThreadManager, run_cpu};
use std::sync::Arc;

/// Initializes, executes, and destroys each session on its own thread.
pub struct SessionManager<R: SessionRequest> {
    queue: Arc<BlockingQueue<R>>,
    threads: ThreadManager,
    metrics: Arc<crate::telemetry::ModelMetrics>,
}

/// Any unexpected owner exit closes the shared queue instead of stranding callers.
struct SessionExit<R: SessionRequest>(Arc<BlockingQueue<R>>);

impl<R: SessionRequest> Drop for SessionExit<R> {
    /// Closing during unwinding also releases work waiting for the failed consumer.
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                "model session owner panicked; closing shared queue"
            );
        }
        self.0.close();
    }
}

impl<R: SessionRequest> Drop for SessionManager<R> {
    /// Reaps every session only after closing admission and waking idle workers.
    fn drop(&mut self) {
        self.close();
    }
}

impl<R: SessionRequest> SessionManager<R> {
    /// Initializes all consumers before exposing the explicitly sized queue and cleans up partial failures.
    pub async fn load<F, W, E>(
        name: &'static str,
        session_size: usize,
        batch_size: usize,
        queue_size: usize,
        initialize: F,
    ) -> Result<Arc<Self>, E>
    where
        F: Fn() -> Result<W, E> + Send + Sync + 'static,
        W: FnMut(Vec<R>) + 'static,
        E: From<TaskError> + Send + 'static,
    {
        run_cpu(move || {
            Self::start(name, session_size, batch_size, queue_size, initialize)
        })
        .await
        .map_err(E::from)?
    }

    /// Starts native owners from blocking code while retaining the same cleanup on partial failure.
    pub fn start<F, W, E>(
        name: &'static str,
        session_size: usize,
        batch_size: usize,
        queue_size: usize,
        initialize: F,
    ) -> Result<Arc<Self>, E>
    where
        F: Fn() -> Result<W, E> + Send + Sync + 'static,
        W: FnMut(Vec<R>) + 'static,
        E: From<TaskError> + Send + 'static,
    {
        Self::start_indexed(
            name,
            session_size,
            batch_size,
            queue_size,
            move |_| initialize(),
        )
    }

    /// Initializes heterogeneous consumers by stable index while retaining one shared queue and lifecycle.
    pub fn start_indexed<F, W, E>(
        name: &'static str,
        session_size: usize,
        batch_size: usize,
        queue_size: usize,
        initialize: F,
    ) -> Result<Arc<Self>, E>
    where
        F: Fn(usize) -> Result<W, E> + Send + Sync + 'static,
        W: FnMut(Vec<R>) + 'static,
        E: From<TaskError> + Send + 'static,
    {
        // Deployments choose consumer counts according to their available CPU/GPU memory.
        if session_size == 0
            || !(1..=32).contains(&batch_size)
            || queue_size == 0
        {
            return Err(TaskError::from_message(
                "invalid model session, batch, or queue size",
            )
            .into());
        }
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        // Pending capacity is configured independently of active consumers and their batch limit.
        let queue = Arc::new(BlockingQueue::new(name, queue_size));
        let mut manager = Self {
            queue: Arc::clone(&queue),
            threads: ThreadManager::default(),
            metrics: crate::telemetry::ModelMetrics::new(
                name,
                session_size,
                batch_size,
            ),
        };
        let initialize = Arc::new(initialize);
        for index in 0..session_size {
            let metrics = Arc::clone(&manager.metrics);
            let queue = Arc::clone(&queue);
            let initialize = Arc::clone(&initialize);
            let dispatch = dispatch.clone();
            let (ready, initialized) = tokio::sync::oneshot::channel();
            manager.threads.spawn(
                format!("docparse-session-{index}"),
                move || {
                    let _exit = SessionExit(Arc::clone(&queue));
                    tracing::dispatcher::with_default(&dispatch, || {
                        let mut model = match initialize(index) {
                            Ok(model) => model,
                            Err(error) => {
                                let _ = ready.send(Err(error));
                                return;
                            }
                        };
                        drop(initialize);
                        if ready.send(Ok(())).is_err() {
                            return;
                        }
                        let _alive = metrics.alive(1);
                        while let Some(requests) = queue.pop(batch_size) {
                            let _batch = metrics.batch();
                            model(requests);
                        }
                    });
                },
            )?;
            initialized.blocking_recv().map_err(|error| {
                TaskError::from_message(error.to_string())
            })??;
        }
        tracing::info!(
            "started {} model sessions with batch limit {}",
            session_size,
            batch_size
        );
        Ok::<_, E>(Arc::new(manager))
    }

    /// Admits one crop from finite blocking submission work that retains the manager through its reply.
    pub fn send(&self, request: R) -> Result<(), TaskError> {
        self.queue.push(request)
    }

    /// Publishes an atomic caller packet without preserving its boundary during consumer batching.
    pub fn send_batch(&self, requests: Vec<R>) -> Result<(), TaskError> {
        self.queue.push_batch(requests)
    }

    /// Stops admission and wakes all producers and consumers before owner cleanup.
    pub fn close(&self) {
        self.queue.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    /// Minimal requests exercise real queue ownership without loading ONNX artifacts.
    struct Request {
        canceled: bool,
        reply: std::sync::mpsc::Sender<usize>,
    }
    impl SessionRequest for Request {
        /// Marks canceled inputs without tying tests to an inference backend.
        fn cancelled(&self) -> bool {
            self.canceled
        }
        /// This test has no timing observer to finish.
        fn end_queue(&mut self) {}
    }

    /// Heterogeneous initializers each consume the same queue even when their work overlaps.
    #[test]
    fn indexed_consumers_share_one_queue() {
        let gate =
            Arc::new((std::sync::Mutex::new(0), std::sync::Condvar::new()));
        let started = Arc::clone(&gate);
        let manager = SessionManager::<Request>::start_indexed(
            "test",
            2,
            1,
            2,
            move |index| {
                let started = Arc::clone(&started);
                Ok::<_, TaskError>(move |requests: Vec<Request>| {
                    let mut count = started.0.lock().expect("gate");
                    *count += 1;
                    started.1.notify_all();
                    let (_count, _) = started
                        .1
                        .wait_timeout_while(
                            count,
                            Duration::from_secs(2),
                            |count| *count < 2,
                        )
                        .expect("peer");
                    for request in requests {
                        let _ = request.reply.send(index);
                    }
                })
            },
        )
        .expect("indexed owners");
        let (reply, response) = std::sync::mpsc::channel();
        manager
            .send_batch(vec![
                Request {
                    canceled: false,
                    reply: reply.clone(),
                },
                Request {
                    canceled: false,
                    reply,
                },
            ])
            .expect("packet");
        let mut indices = vec![
            response
                .recv_timeout(Duration::from_secs(3))
                .expect("first"),
            response
                .recv_timeout(Duration::from_secs(3))
                .expect("second"),
        ];
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1]);
    }

    /// The shared runtime starts every requested consumer beyond the former eight-session ceiling.
    #[test]
    fn session_count_is_positive_without_fixed_ceiling() {
        let count = Arc::new(AtomicUsize::new(0));
        let initialized = Arc::clone(&count);
        let manager =
            SessionManager::<Request>::start("test", 16, 1, 1, move || {
                initialized.fetch_add(1, Ordering::SeqCst);
                Ok::<_, TaskError>(|requests: Vec<Request>| {
                    for request in requests {
                        let _ = request.reply.send(1);
                    }
                })
            })
            .expect("sixteen consumers");
        assert_eq!(count.load(Ordering::SeqCst), 16);
        let (reply, response) = std::sync::mpsc::channel();
        manager
            .send(Request {
                canceled: false,
                reply,
            })
            .expect("enqueue");
        assert_eq!(
            response
                .recv_timeout(Duration::from_secs(2))
                .expect("response"),
            1
        );
        drop(manager);
        let invalid = SessionManager::<Request>::start("test", 0, 1, 1, || {
            Ok::<_, TaskError>(|_: Vec<Request>| {})
        });
        assert!(invalid.is_err());
    }

    /// Two consumers share capacity, skip canceled requests, and outlive their construction runtime.
    #[test]
    fn consumers_share_ready_queue_and_survive_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let next = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicBool::new(false));
        let workers = Arc::clone(&next);
        let released = Arc::clone(&release);
        let manager = runtime
            .block_on(SessionManager::load("test", 2, 1, 2, move || {
                let worker = workers.fetch_add(1, Ordering::SeqCst);
                let released = Arc::clone(&released);
                Ok::<_, TaskError>(move |requests: Vec<Request>| {
                    for request in requests {
                        request.reply.send(worker).expect("reply");
                    }
                    while !released.load(Ordering::SeqCst) {
                        std::thread::park_timeout(Duration::from_millis(1));
                    }
                })
            }))
            .expect("sessions");
        drop(runtime);
        let (reply, results) = std::sync::mpsc::channel();
        manager
            .send(Request {
                canceled: true,
                reply: reply.clone(),
            })
            .expect("canceled");
        for _ in 0..2 {
            manager
                .send(Request {
                    canceled: false,
                    reply: reply.clone(),
                })
                .expect("send");
        }
        let first = results.recv_timeout(Duration::from_secs(2));
        let second = results.recv_timeout(Duration::from_secs(2));
        release.store(true, Ordering::SeqCst);
        drop(manager);
        assert_ne!(
            first.expect("first consumer"),
            second.expect("second consumer")
        );
        assert_eq!(next.load(Ordering::SeqCst), 2);
    }

    /// A failed later initializer must close admission and destroy every already-created model.
    #[test]
    fn partial_initialization_reaps_started_sessions() {
        struct Model(Arc<AtomicUsize>);
        impl Drop for Model {
            /// Observes real owner cleanup when the following initializer fails.
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let attempts = AtomicUsize::new(0);
        let drops = Arc::new(AtomicUsize::new(0));
        let destroyed = Arc::clone(&drops);
        let result = runtime.block_on(SessionManager::<Request>::load(
            "test",
            2,
            2,
            4,
            move || {
                if attempts.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err(TaskError::from_message("second model failed"));
                }
                let model = Model(Arc::clone(&destroyed));
                Ok(move |_requests| {
                    let _owner = &model;
                })
            },
        ));
        assert!(result.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
