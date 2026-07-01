//! Document layout detection for docparse.

mod detector;
mod ml;
mod pp_doclayout;

pub use detector::LayoutError;
pub use detector::{
    LayoutBlock, LayoutDetector, LayoutModel, LayoutModelBytes, LayoutOptions,
    LayoutPage, LayoutRect,
};
#[cfg(target_arch = "wasm32")]
pub use ml::backend::init_browser_webgpu;
