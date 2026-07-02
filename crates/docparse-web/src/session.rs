//! WASM ORT Web analyzer session.

use docparse_core::{LayoutAnalyzer, LayoutError, LayoutPage};
#[cfg(target_arch = "wasm32")]
use docparse_core::{
    PostprocessOptions, PreprocessOptions, postprocess_fetch_rows,
    preprocess_image,
};
#[cfg(target_arch = "wasm32")]
use ort::{
    session::{RunOptions, Session},
    value::TensorRef,
};

/// Browser execution provider preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebExecutionProvider {
    /// Browser WebGPU execution provider.
    WebGpu,
}

/// Configuration for browser layout analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebLayoutConfig {
    /// URL for `inference.onnx`, relative to the hosting page or absolute.
    pub model_url: String,
    /// Browser execution provider.
    pub execution_provider: WebExecutionProvider,
}

impl Default for WebLayoutConfig {
    fn default() -> Self {
        Self {
            model_url: "./models/pp-structure-v3-onnx/inference.onnx"
                .to_owned(),
            execution_provider: WebExecutionProvider::WebGpu,
        }
    }
}

/// Browser ORT Web layout analyzer.
#[derive(Debug, Clone)]
pub struct WebLayoutAnalyzer {
    config: WebLayoutConfig,
}

impl WebLayoutAnalyzer {
    /// Creates a browser analyzer configuration holder.
    #[must_use]
    pub fn new(config: WebLayoutConfig) -> Self {
        Self { config }
    }

    /// Returns the active configuration.
    #[must_use]
    pub fn config(&self) -> &WebLayoutConfig {
        &self.config
    }
}

impl LayoutAnalyzer for WebLayoutAnalyzer {
    async fn analyze_image(
        &self,
        _image: &image::DynamicImage,
    ) -> Result<LayoutPage, LayoutError> {
        Err(LayoutError::Backend(
            "ORT Web inference is not wired yet".to_owned(),
        ))
    }
}

/// WASM-facing analyzer wrapper.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub struct WasmLayoutAnalyzer {
    config: WebLayoutConfig,
    session: Session,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl WasmLayoutAnalyzer {
    /// Initializes ORT Web with WebGPU and loads a model session.
    pub async fn create_webgpu(
        model_url: String,
    ) -> Result<WasmLayoutAnalyzer, wasm_bindgen::JsValue> {
        init_ort_webgpu().await?;
        web_sys::console::log_1(
            &"docparse-web: initialized ORT Web with WebGPU".into(),
        );

        let session = Session::builder()
            .map_err(to_js_error)?
            .with_execution_providers([ort::ep::WebGPU::default().build()])
            .map_err(to_js_error)?
            .commit_from_url(&model_url)
            .await
            .map_err(to_js_error)?;
        web_sys::console::log_1(&"docparse-web: loaded ONNX model".into());

        Ok(Self {
            config: WebLayoutConfig {
                model_url,
                execution_provider: WebExecutionProvider::WebGpu,
            },
            session,
        })
    }

    /// Returns the configured model URL.
    #[wasm_bindgen::prelude::wasm_bindgen(getter)]
    pub fn model_url(&self) -> String {
        self.config.model_url.clone()
    }

    /// Analyzes PNG/JPEG/WebP image bytes and returns JSON layout detections.
    pub async fn analyze_image_bytes(
        &mut self,
        image_bytes: &[u8],
    ) -> Result<String, wasm_bindgen::JsValue> {
        let image = image::load_from_memory(image_bytes).map_err(|error| {
            wasm_bindgen::JsValue::from_str(&format!(
                "failed to decode image bytes: {error}"
            ))
        })?;
        let page = analyze_image_with_session(&mut self.session, &image)
            .await
            .map_err(|error| {
                wasm_bindgen::JsValue::from_str(&error.to_string())
            })?;

        serde_json::to_string_pretty(&page).map_err(|error| {
            wasm_bindgen::JsValue::from_str(&format!(
                "failed to serialize layout result: {error}"
            ))
        })
    }
}

#[cfg(target_arch = "wasm32")]
async fn init_ort_webgpu() -> Result<(), wasm_bindgen::JsValue> {
    let api = ort_web::api(ort_web::FEATURE_WEBGPU)
        .await
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
    ort::set_api(api);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
async fn analyze_image_with_session(
    session: &mut Session,
    image: &image::DynamicImage,
) -> Result<LayoutPage, LayoutError> {
    let input = preprocess_image(image, PreprocessOptions::default())?;
    let image_tensor =
        TensorRef::from_array_view(&input.image).map_err(|error| {
            LayoutError::Backend(format!(
                "failed to create image tensor: {error}"
            ))
        })?;
    let im_shape_tensor =
        TensorRef::from_array_view(&input.im_shape).map_err(|error| {
            LayoutError::Backend(format!(
                "failed to create im_shape tensor: {error}"
            ))
        })?;
    let scale_factor_tensor = TensorRef::from_array_view(&input.scale_factor)
        .map_err(|error| {
        LayoutError::Backend(format!(
            "failed to create scale_factor tensor: {error}"
        ))
    })?;
    let run_options = RunOptions::new().map_err(|error| {
        LayoutError::Backend(format!("failed to create run options: {error}"))
    })?;
    let mut outputs = session
        .run_async(
            ort::inputs! {
                "im_shape" => im_shape_tensor,
                "image" => image_tensor,
                "scale_factor" => scale_factor_tensor,
            },
            &run_options,
        )
        .await
        .map_err(|error| {
            LayoutError::Backend(format!(
                "failed to run ONNX inference: {error}"
            ))
        })?;
    ort_web::sync_outputs(&mut outputs).await.map_err(|error| {
        LayoutError::Backend(format!("failed to sync ORT Web outputs: {error}"))
    })?;
    let fetch = outputs.get("fetch_name_0").ok_or_else(|| {
        LayoutError::Backend("ONNX output fetch_name_0 is missing".to_owned())
    })?;
    let (shape, values) =
        fetch.try_extract_tensor::<f32>().map_err(|error| {
            LayoutError::Backend(format!(
                "failed to extract fetch_name_0 tensor: {error}"
            ))
        })?;
    let columns = shape
        .last()
        .and_then(|value| usize::try_from(*value).ok())
        .ok_or_else(|| {
            LayoutError::Postprocess(format!(
                "fetch_name_0 has invalid shape: {shape}"
            ))
        })?;

    postprocess_fetch_rows(
        values,
        columns,
        input.original_width,
        input.original_height,
        PostprocessOptions::default(),
    )
}

#[cfg(target_arch = "wasm32")]
fn to_js_error(error: impl std::fmt::Display) -> wasm_bindgen::JsValue {
    wasm_bindgen::JsValue::from_str(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{WebExecutionProvider, WebLayoutConfig};

    #[test]
    fn default_config_points_to_shared_model_url() {
        let config = WebLayoutConfig::default();

        assert_eq!(
            config.model_url,
            "./models/pp-structure-v3-onnx/inference.onnx"
        );
        assert_eq!(config.execution_provider, WebExecutionProvider::WebGpu);
    }
}
