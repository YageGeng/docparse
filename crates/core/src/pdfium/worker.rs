//! Native thread and browser-local ownership of the PDFium actor.
use super::{PdfInput, PdfiumCommand, PdfiumRuntimeError, worker_main};
use tokio::sync::{mpsc, oneshot};

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use std::time::Duration;
    use tracing::{Instrument, instrument::WithSubscriber};

    /// Deadline for awaiting the dedicated worker's resource cleanup.
    const JOIN_DEADLINE: Duration = Duration::from_secs(5);

    /// A dedicated thread that owns the complete PDFium document lifetime.
    pub(crate) struct PdfiumWorker {
        finished: oneshot::Receiver<std::thread::Result<()>>,
    }
    impl PdfiumWorker {
        /// Drives the local PDFium actor on its own thread without requiring its future to be Send.
        pub(crate) fn spawn(
            input: PdfInput,
            receiver: mpsc::Receiver<PdfiumCommand>,
            ready: oneshot::Sender<Result<u32, PdfiumRuntimeError>>,
        ) -> Result<Self, PdfiumRuntimeError> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(|error| {
                    PdfiumRuntimeError::ThreadSpawn(error.to_string())
                })?;
            // Capture both contexts before crossing threads, then restore them for each actor poll.
            let span = tracing::Span::current();
            let dispatcher = tracing::dispatcher::get_default(Clone::clone);
            let (completed, finished) = oneshot::channel();
            let thread = std::thread::Builder::new()
                .name("docparse-pdfium".into())
                .spawn(move || {
                    // Moving all owners into this closure drops the actor and runtime before signalling cleanup.
                    let result = std::panic::catch_unwind(
                        std::panic::AssertUnwindSafe(move || {
                            runtime.block_on(
                                worker_main(input, receiver, ready)
                                    .instrument(span)
                                    .with_subscriber(dispatcher),
                            );
                        }),
                    );
                    let _ = completed.send(result);
                })
                .map_err(|error| {
                    PdfiumRuntimeError::ThreadSpawn(error.to_string())
                })?;
            // The completion channel owns the cleanup boundary; no polling or uncancellable OS join is needed.
            drop(thread);
            Ok(Self { finished })
        }
        /// Waits asynchronously until all PDFium handles have been dropped.
        ///
        /// A thread that never finishes remains detached without occupying Tokio's blocking pool.
        pub(crate) async fn join(self) -> Result<(), PdfiumRuntimeError> {
            match crate::wasm_compat::timeout(JOIN_DEADLINE, self.finished)
                .await
            {
                Ok(result) => result
                    .map_err(PdfiumRuntimeError::from)?
                    .map_err(|payload| {
                        tracing::warn!(
                            "PDFium worker thread panicked: {}",
                            docparse_common::panic_message(&*payload)
                        );
                        PdfiumRuntimeError::WorkerPanicked
                    }),
                Err(_elapsed) => {
                    tracing::warn!(
                        "PDFium worker cleanup did not finish within {} s; leaving its thread detached",
                        JOIN_DEADLINE.as_secs()
                    );
                    Ok(())
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A timed-out join must leave blocking capacity and runtime shutdown available.
        #[test]
        fn join_timeout_releases_blocking_capacity() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(1)
                .build()
                .expect("runtime");
            let (release, wait) = std::sync::mpsc::channel();
            let (completed, finished) = oneshot::channel();
            let thread = std::thread::spawn(move || {
                wait.recv_timeout(Duration::from_secs(15)).expect("release");
                let _ = completed.send(Ok(()));
            });
            let worker = PdfiumWorker { finished };
            let available = runtime.block_on(async {
                worker.join().await.expect("detached worker");
                tokio::time::timeout(
                    Duration::from_millis(100),
                    docparse_common::run_cpu(|| 42),
                )
                .await
            });
            // Release the native thread before asserting so a regression cannot strand the test runtime.
            release.send(()).expect("cleanup");
            thread.join().expect("test worker cleanup");
            drop(runtime);
            assert_eq!(
                available
                    .expect("blocking capacity after timeout")
                    .expect("CPU work"),
                42
            );
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use crate::wasm_compat::{TaskError, WasmBoxedFuture, spawn};

    /// A local PDFium actor running inside the embedding application's dedicated Worker.
    pub(crate) struct PdfiumWorker {
        task: WasmBoxedFuture<'static, Result<(), TaskError>>,
    }
    impl PdfiumWorker {
        /// Starts the actor without transferring PDFium handles across an executor boundary.
        pub(crate) fn spawn(
            input: PdfInput,
            receiver: mpsc::Receiver<PdfiumCommand>,
            ready: oneshot::Sender<Result<u32, PdfiumRuntimeError>>,
        ) -> Result<Self, PdfiumRuntimeError> {
            Ok(Self {
                task: spawn(worker_main(input, receiver, ready)),
            })
        }
        /// Waits until the actor has released its document, library and source bytes.
        pub(crate) async fn join(self) -> Result<(), PdfiumRuntimeError> {
            self.task.await.map_err(|error| {
                tracing::warn!("PDFium actor task failed: {error}");
                PdfiumRuntimeError::WorkerStopped
            })
        }
    }
}

pub(crate) use platform::PdfiumWorker;
