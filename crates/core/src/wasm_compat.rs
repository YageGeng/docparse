//! Native and browser runtime boundaries with explicitly scoped compatibility submodules.
mod pdf_input;
mod pdfium_worker;
mod task_set;

pub(crate) use pdf_input::PdfInput;
pub(crate) use pdfium_worker::PdfiumWorker;
pub(crate) use task_set::{TaskSet, spawn};

pub use docparse_layout::wasm_compat::{
    TaskError, WasmBoxedFuture, WasmCompatSend, WasmCompatSync,
};

#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!("docparse Web builds require the wasm feature");
#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
compile_error!("docparse supports wasm32-unknown-unknown browser builds only");

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod native;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use native::{write_pdf_overlays, write_pdf_overlays_for_pages};
