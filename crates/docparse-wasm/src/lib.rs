#[cfg(target_arch = "wasm32")]
use docparse_layout::init_browser_webgpu;
use docparse_layout::{LayoutDetector, LayoutModelBytes};
use wasm_bindgen::prelude::*;

const PACKAGE_NAME: &str = env!("CARGO_PKG_NAME");
const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const BACKEND: &str = "wasm32-unknown-unknown";

/// Keeps native `cargo check` usable for the wasm crate.
#[cfg(not(target_arch = "wasm32"))]
async fn init_browser_webgpu() {}

/// Installs panic reporting for browser console diagnostics.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Returns package version for browser smoke tests.
#[wasm_bindgen]
pub fn version() -> String {
    PACKAGE_VERSION.to_owned()
}

/// Returns the compiled wasm backend label.
#[wasm_bindgen]
pub fn backend() -> String {
    BACKEND.to_owned()
}

/// Returns a small JSON payload proving the wasm module loaded.
#[wasm_bindgen]
pub fn smoke_test() -> String {
    format!(
        "{{\"package\":\"{PACKAGE_NAME}\",\"version\":\"{PACKAGE_VERSION}\",\"backend\":\"{BACKEND}\",\"ready\":true}}"
    )
}

/// Browser-side PP-DocLayout session with model weights kept in wasm memory.
#[wasm_bindgen]
pub struct LayoutSession {
    detector: LayoutDetector,
}

/// Creates a WebGPU-backed layout session from user-provided model files.
#[wasm_bindgen]
pub async fn create_layout_session(
    config_json: Vec<u8>,
    preprocessor_json: Vec<u8>,
    weights: Vec<u8>,
) -> Result<LayoutSession, JsValue> {
    init_browser_webgpu().await;
    let detector = LayoutDetector::from_model_bytes(LayoutModelBytes {
        config_json: &config_json,
        preprocessor_json: &preprocessor_json,
        weights: &weights,
    })
    .map_err(to_js_error)?;

    Ok(LayoutSession { detector })
}

#[wasm_bindgen]
impl LayoutSession {
    /// Runs layout detection for one encoded image and returns pretty JSON.
    pub async fn detect(
        &self,
        image_bytes: Vec<u8>,
    ) -> Result<String, JsValue> {
        let image =
            image::load_from_memory(&image_bytes).map_err(to_js_error)?;
        let page = self
            .detector
            .detect_image_async(&image)
            .await
            .map_err(to_js_error)?;

        serde_json::to_string_pretty(&page).map_err(to_js_error)
    }
}

/// Converts Rust display errors into JavaScript exceptions.
fn to_js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
