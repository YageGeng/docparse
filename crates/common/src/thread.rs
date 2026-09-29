//! Thread ownership shared by native session owners and asynchronous queue consumers.
use crate::{TaskError, WasmBoxedFuture};

/// Joins owned native threads after their owner closes admission; browser tasks stay Worker-local.
#[derive(Default)]
pub struct ThreadManager {
    #[cfg(not(target_arch = "wasm32"))]
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Reaps threads even when runtime shutdown discards a not-yet-started cleanup task.
#[cfg(not(target_arch = "wasm32"))]
struct Joining(Vec<std::thread::JoinHandle<()>>);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Joining {
    /// Native owners may finish on their own thread; all other handles must be joined.
    fn drop(&mut self) {
        for thread in self.0.drain(..) {
            if thread.thread().id() != std::thread::current().id()
                && thread.join().is_err()
            {
                tracing::error!("owned worker thread panicked during shutdown");
            }
        }
    }
}

impl ThreadManager {
    /// Explicitly awaits native teardown when a caller must finish cleanup before reporting failure.
    pub async fn shutdown(self) -> Result<(), TaskError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut owner = self;
            let current = std::thread::current().id();
            // Identify self ownership before moving cleanup onto another thread in this runtime.
            owner
                .threads
                .retain(|thread| thread.thread().id() != current);
            if owner.threads.is_empty() {
                return Ok(());
            }
            crate::run_blocking(move || {
                owner.join();
            })
            .await
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(())
        }
    }

    /// Completes cleanup synchronously when already running in a blocking initialization path.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn join(&mut self) {
        drop(Joining(std::mem::take(&mut self.threads)));
    }

    /// Installs thread ownership immediately so partial initialization also joins already-started work.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn spawn(
        &mut self,
        name: String,
        operation: impl FnOnce() + Send + 'static,
    ) -> Result<(), TaskError> {
        self.threads.push(
            std::thread::Builder::new()
                .name(name)
                .spawn(operation)
                .map_err(|error| TaskError::from_message(error.to_string()))?,
        );
        Ok(())
    }

    /// Runs a queue consumer independently of its construction runtime, or locally in a browser Worker.
    pub fn spawn_async(
        future: WasmBoxedFuture<'static, ()>,
    ) -> Result<Self, TaskError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            let (ready, initialized) = std::sync::mpsc::sync_channel(1);
            let mut owner = Self::default();
            owner.spawn("docparse-queue".into(), move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    // Creating the runtime here also keeps its destruction off the async caller when spawning fails.
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => {
                            if ready.send(Ok(())).is_ok() {
                                runtime.block_on(future);
                            }
                        }
                        Err(error) => {
                            let _ = ready.send(Err(TaskError::from_message(
                                error.to_string(),
                            )));
                        }
                    }
                });
            })?;
            // A thread handle alone does not prove its executor initialized successfully.
            initialized.recv().map_err(|error| {
                TaskError::from_message(format!(
                    "async worker initialization stopped: {error}"
                ))
            })??;
            Ok(owner)
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_futures::spawn_local(future);
            Ok(Self::default())
        }
    }
}

impl Drop for ThreadManager {
    /// Schedules native joins without blocking an async caller; explicit shutdown awaits completion.
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let current = std::thread::current().id();
            // A blocking reaper cannot detect its origin: joining this thread would cycle with runtime shutdown.
            self.threads
                .retain(|thread| thread.thread().id() != current);
            let threads = std::mem::take(&mut self.threads);
            if threads.is_empty() {
                return;
            }
            let joining = Joining(threads);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                // Runtime shutdown still waits for cleanup, but polling a canceled request never joins an OS thread.
                drop(runtime.spawn_blocking(move || drop(joining)));
            } else {
                drop(joining);
            }
        }
    }
}
