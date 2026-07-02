//! Browser ORT Web backend for layout analysis.

#[cfg(target_arch = "wasm32")]
mod pdf;
mod session;

pub use session::{WebExecutionProvider, WebLayoutAnalyzer, WebLayoutConfig};

#[cfg(target_arch = "wasm32")]
pub use pdf::{WasmPdfPage, render_pdf_to_png_pages};
#[cfg(target_arch = "wasm32")]
pub use session::WasmLayoutAnalyzer;
