//! PDFium document execution, provider contracts, and optional native IPC.
mod executor;
mod input;
mod provider;
mod worker;

#[cfg(all(feature = "pdfium-ipc", not(target_arch = "wasm32")))]
pub mod ipc;

pub(crate) use executor::{PdfiumCommand, PdfiumExecutor, worker_main};
pub use executor::{PdfiumRuntimeError, PreScannedPage, RenderedPage};
pub use provider::{LocalPdfiumProvider, PdfiumProvider, PdfiumSession};

pub use input::PdfInput;
pub(crate) use worker::PdfiumWorker;
