use docparse_layout::PageImage;
use serde::Serialize;

use crate::wasm_compat::{WasmCompatSend, WasmCompatSync};

/// Actual document pipeline boundaries, independent of execution speed or platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum ParseProgress {
    Opening,
    Scanning { completed: u32, total: u32 },
    Analyzing { completed: u32, total: u32 },
    Linking { total: u32 },
    Complete { total: u32 },
}

/// Optional per-parse observer; callbacks run serially in the calling parse future.
pub trait ParseObserver: WasmCompatSend + WasmCompatSync {
    /// Reports completed work rather than an estimated timer-based percentage.
    fn on_progress(&self, progress: ParseProgress);

    /// Borrows the same PDFium RGB raster used by inference, without rendering a second time.
    fn on_page_image(&self, _page_number: u32, _image: &PageImage) {}
}
