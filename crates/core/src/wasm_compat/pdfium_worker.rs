//! Native thread and browser-local ownership of the PDFium actor.
use super::PdfInput;
use crate::runtime::pdfium_executor::{
    PdfiumCommand, PdfiumRuntimeError, worker_main,
};
use tokio::sync::{mpsc, oneshot};

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;

    /// A dedicated thread that owns the complete PDFium document lifetime.
    pub(crate) struct PdfiumWorker {
        thread: std::thread::JoinHandle<()>,
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
            let thread = std::thread::Builder::new()
                .name("docparse-pdfium".into())
                .spawn(move || {
                    runtime.block_on(worker_main(input, receiver, ready))
                })
                .map_err(|error| {
                    PdfiumRuntimeError::ThreadSpawn(error.to_string())
                })?;
            Ok(Self { thread })
        }
        /// Waits off the async executor until all PDFium handles have been dropped.
        pub(crate) async fn join(self) -> Result<(), PdfiumRuntimeError> {
            docparse_layout::wasm_compat::run_cpu(move || self.thread.join())
                .await
                .map_err(|_error| PdfiumRuntimeError::WorkerPanicked)?
                .map_err(|_error| PdfiumRuntimeError::WorkerPanicked)
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
            self.task
                .await
                .map_err(|_error| PdfiumRuntimeError::WorkerStopped)
        }
    }
}

pub(crate) use platform::PdfiumWorker;
