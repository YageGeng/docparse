//! Thread ownership shared by native session owners and asynchronous queue consumers.
use crate::{TaskError, WasmBoxedFuture};

/// Joins owned native threads after their owner closes admission; browser tasks stay Worker-local.
#[derive(Default)]
pub struct ThreadManager {
    #[cfg(not(target_arch = "wasm32"))]
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl ThreadManager {
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
    /// Finishes thread-local destruction before the final owner returns from shutdown.
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                tracing::error!("owned worker thread panicked during shutdown");
            }
        }
    }
}
