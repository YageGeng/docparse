//! WASM ORT Web analyzer session.

use docparse_core::{LayoutAnalyzer, LayoutError, LayoutPage};
#[cfg(target_arch = "wasm32")]
use docparse_core::{
    LayoutLabel, MODEL_INPUT_IM_SHAPE, MODEL_INPUT_IMAGE,
    MODEL_INPUT_SCALE_FACTOR, MODEL_OUTPUT_FETCH_ROW_COUNTS,
    MODEL_OUTPUT_FETCH_ROWS, PostprocessOptions, PreprocessOptions,
    postprocess_fetch_rows_batch, preprocess_images,
};
#[cfg(target_arch = "wasm32")]
use ort::{
    session::{RunOptions, Session},
    value::TensorRef,
};
#[cfg(target_arch = "wasm32")]
use std::sync::Once;

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
        init_wasm_tracing();
        let started_at = js_sys::Date::now();
        init_ort_webgpu().await?;
        tracing::info!(
            elapsed_ms = js_sys::Date::now() - started_at,
            "initialized ORT Web with WebGPU"
        );

        let load_started_at = js_sys::Date::now();
        let session = Session::builder()
            .map_err(to_js_error)?
            .with_execution_providers([ort::ep::WebGPU::default().build()])
            .map_err(to_js_error)?
            .commit_from_url(&model_url)
            .await
            .map_err(to_js_error)?;
        tracing::info!(
            model_url_len = model_url.len(),
            elapsed_ms = js_sys::Date::now() - load_started_at,
            "loaded layout ONNX model"
        );

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
        self.analyze_image_bytes_with_threshold(
            image_bytes,
            PostprocessOptions::default().threshold,
        )
        .await
    }

    /// Analyzes image bytes with a caller-provided confidence threshold.
    pub async fn analyze_image_bytes_with_threshold(
        &mut self,
        image_bytes: &[u8],
        threshold: f32,
    ) -> Result<String, wasm_bindgen::JsValue> {
        self.analyze_image_bytes_with_options(
            image_bytes,
            threshold,
            PostprocessOptions::default().nms_iou_threshold,
            PostprocessOptions::default().cross_label_iou_threshold,
        )
        .await
    }

    /// Analyzes image bytes with caller-provided postprocessing thresholds.
    pub async fn analyze_image_bytes_with_options(
        &mut self,
        image_bytes: &[u8],
        threshold: f32,
        nms_iou_threshold: f32,
        cross_label_iou_threshold: f32,
    ) -> Result<String, wasm_bindgen::JsValue> {
        if !(0.0..=1.0).contains(&threshold) {
            return Err(wasm_bindgen::JsValue::from_str(
                "threshold must be between 0 and 1",
            ));
        }
        if !(0.0..=1.0).contains(&nms_iou_threshold) {
            return Err(wasm_bindgen::JsValue::from_str(
                "nms_iou_threshold must be between 0 and 1",
            ));
        }
        if !(0.0..=1.0).contains(&cross_label_iou_threshold) {
            return Err(wasm_bindgen::JsValue::from_str(
                "cross_label_iou_threshold must be between 0 and 1",
            ));
        }
        let decode_started_at = js_sys::Date::now();
        let image = image::load_from_memory(image_bytes).map_err(|error| {
            wasm_bindgen::JsValue::from_str(&format!(
                "failed to decode image bytes: {error}"
            ))
        })?;
        tracing::info!(
            bytes = image_bytes.len(),
            width = image.width(),
            height = image.height(),
            elapsed_ms = js_sys::Date::now() - decode_started_at,
            "decoded layout image bytes"
        );
        let page = analyze_image_with_session(
            &mut self.session,
            &image,
            PostprocessOptions {
                threshold,
                nms_iou_threshold,
                cross_label_iou_threshold,
                ..PostprocessOptions::default()
            },
        )
        .await
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;

        serde_json::to_string(&page).map_err(|error| {
            wasm_bindgen::JsValue::from_str(&format!(
                "failed to serialize layout result: {error}"
            ))
        })
    }

    /// Analyzes a JavaScript array of image byte arrays in one batched model call.
    pub async fn analyze_image_bytes_batch_with_options(
        &mut self,
        image_bytes_batch: js_sys::Array,
        threshold: f32,
        nms_iou_threshold: f32,
        cross_label_iou_threshold: f32,
    ) -> Result<String, wasm_bindgen::JsValue> {
        if !(0.0..=1.0).contains(&threshold) {
            return Err(wasm_bindgen::JsValue::from_str(
                "threshold must be between 0 and 1",
            ));
        }
        if !(0.0..=1.0).contains(&nms_iou_threshold) {
            return Err(wasm_bindgen::JsValue::from_str(
                "nms_iou_threshold must be between 0 and 1",
            ));
        }
        if !(0.0..=1.0).contains(&cross_label_iou_threshold) {
            return Err(wasm_bindgen::JsValue::from_str(
                "cross_label_iou_threshold must be between 0 and 1",
            ));
        }
        let decode_started_at = js_sys::Date::now();
        let mut images =
            Vec::with_capacity(image_bytes_batch.length() as usize);
        let mut total_bytes = 0usize;
        for bytes in image_bytes_batch.iter() {
            let bytes = js_sys::Uint8Array::new(&bytes).to_vec();
            total_bytes += bytes.len();
            images.push(image::load_from_memory(&bytes).map_err(|error| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "failed to decode image bytes: {error}"
                ))
            })?);
        }
        tracing::info!(
            batch_size = images.len(),
            bytes = total_bytes,
            elapsed_ms = js_sys::Date::now() - decode_started_at,
            "decoded layout image batch"
        );

        let pages = analyze_images_with_session(
            &mut self.session,
            &images,
            PostprocessOptions {
                threshold,
                nms_iou_threshold,
                cross_label_iou_threshold,
                ..PostprocessOptions::default()
            },
        )
        .await
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;

        serde_json::to_string(&pages).map_err(|error| {
            wasm_bindgen::JsValue::from_str(&format!(
                "failed to serialize layout results: {error}"
            ))
        })
    }
}

/// Returns the Rust-defined display color for a serialized layout label.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn layout_label_color(label: &str) -> String {
    LayoutLabel::try_from(label)
        .unwrap_or(LayoutLabel::Unknown)
        .color()
        .to_owned()
}

#[cfg(target_arch = "wasm32")]
async fn init_ort_webgpu() -> Result<(), wasm_bindgen::JsValue> {
    let api = ort_web::api(ort_web::FEATURE_WEBGPU)
        .await
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
    ort::set_api(api);
    Ok(())
}

/// Installs the browser console tracing subscriber once for WASM diagnostics.
#[cfg(target_arch = "wasm32")]
fn init_wasm_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // This can fail if the host page has already installed a tracing
        // subscriber, so initialization is best-effort instead of fatal.
        let _ = tracing_wasm::try_set_as_global_default();
    });
}

#[cfg(target_arch = "wasm32")]
async fn analyze_image_with_session(
    session: &mut Session,
    image: &image::DynamicImage,
    postprocess_options: PostprocessOptions,
) -> Result<LayoutPage, LayoutError> {
    // Route single-image inference through the batch decoder so row counts are
    // handled consistently with PDF multi-page detection.
    let pages = analyze_images_with_session(
        session,
        std::slice::from_ref(image),
        postprocess_options,
    )
    .await?;
    pages.into_iter().next().ok_or_else(|| {
        LayoutError::Backend(
            "ORT Web batch inference returned no pages".to_owned(),
        )
    })
}

/// Runs one ORT Web inference for a batch of images and decodes pages in order.
#[cfg(target_arch = "wasm32")]
async fn analyze_images_with_session(
    session: &mut Session,
    images: &[image::DynamicImage],
    postprocess_options: PostprocessOptions,
) -> Result<Vec<LayoutPage>, LayoutError> {
    let total_started_at = js_sys::Date::now();
    tracing::info!(
        batch_size = images.len(),
        threshold = postprocess_options.threshold,
        nms_iou_threshold = postprocess_options.nms_iou_threshold,
        cross_label_iou_threshold =
            postprocess_options.cross_label_iou_threshold,
        "starting layout batch analysis"
    );

    let preprocess_started_at = js_sys::Date::now();
    let input = preprocess_images(images, PreprocessOptions::default())?;
    tracing::info!(
        batch_size = input.original_sizes.len(),
        input_height = input.image.shape().get(2).copied().unwrap_or(0),
        input_width = input.image.shape().get(3).copied().unwrap_or(0),
        elapsed_ms = js_sys::Date::now() - preprocess_started_at,
        "prepared layout model inputs"
    );

    let tensor_started_at = js_sys::Date::now();
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
    tracing::info!(
        elapsed_ms = js_sys::Date::now() - tensor_started_at,
        "created layout ORT tensors"
    );

    let run_options = RunOptions::new().map_err(|error| {
        LayoutError::Backend(format!("failed to create run options: {error}"))
    })?;
    let inference_started_at = js_sys::Date::now();
    let mut outputs = session
        .run_async(
            ort::inputs! {
                MODEL_INPUT_IM_SHAPE => im_shape_tensor,
                MODEL_INPUT_IMAGE => image_tensor,
                MODEL_INPUT_SCALE_FACTOR => scale_factor_tensor,
            },
            &run_options,
        )
        .await
        .map_err(|error| {
            LayoutError::Backend(format!(
                "failed to run ONNX inference: {error}"
            ))
        })?;
    tracing::info!(
        elapsed_ms = js_sys::Date::now() - inference_started_at,
        "completed layout ONNX inference"
    );

    let sync_started_at = js_sys::Date::now();
    ort_web::sync_outputs(&mut outputs).await.map_err(|error| {
        LayoutError::Backend(format!("failed to sync ORT Web outputs: {error}"))
    })?;
    tracing::info!(
        elapsed_ms = js_sys::Date::now() - sync_started_at,
        "synced layout ORT Web outputs"
    );

    let output_started_at = js_sys::Date::now();
    let fetch = outputs.get(MODEL_OUTPUT_FETCH_ROWS).ok_or_else(|| {
        LayoutError::Backend(format!(
            "ONNX output {MODEL_OUTPUT_FETCH_ROWS} is missing"
        ))
    })?;
    let (shape, values) =
        fetch.try_extract_tensor::<f32>().map_err(|error| {
            LayoutError::Backend(format!(
                "failed to extract {MODEL_OUTPUT_FETCH_ROWS} tensor: {error}"
            ))
        })?;
    let columns = shape
        .last()
        .and_then(|value| usize::try_from(*value).ok())
        .ok_or_else(|| {
            LayoutError::Postprocess(format!(
                "{MODEL_OUTPUT_FETCH_ROWS} has invalid shape: {shape}"
            ))
        })?;
    let row_counts = extract_fetch_row_counts(&outputs)?;
    tracing::info!(
        columns,
        rows = row_counts.iter().sum::<usize>(),
        elapsed_ms = js_sys::Date::now() - output_started_at,
        "extracted layout model outputs"
    );

    let postprocess_started_at = js_sys::Date::now();
    let pages = postprocess_fetch_rows_batch(
        values,
        columns,
        &row_counts,
        &input.original_sizes,
        postprocess_options,
    )?;
    let total_blocks: usize = pages.iter().map(|page| page.blocks.len()).sum();
    tracing::info!(
        pages = pages.len(),
        blocks = total_blocks,
        postprocess_elapsed_ms = js_sys::Date::now() - postprocess_started_at,
        total_elapsed_ms = js_sys::Date::now() - total_started_at,
        "completed layout batch analysis"
    );

    Ok(pages)
}

/// Extracts per-image detection counts from the PP-StructureV3 batch output.
#[cfg(target_arch = "wasm32")]
fn extract_fetch_row_counts(
    outputs: &ort::session::SessionOutputs<'_>,
) -> Result<Vec<usize>, LayoutError> {
    let fetch =
        outputs.get(MODEL_OUTPUT_FETCH_ROW_COUNTS).ok_or_else(|| {
            LayoutError::Backend(format!(
                "ONNX output {MODEL_OUTPUT_FETCH_ROW_COUNTS} is missing"
            ))
        })?;
    let (_shape, values) =
        fetch.try_extract_tensor::<i32>().map_err(|error| {
            LayoutError::Backend(format!(
                "failed to extract {MODEL_OUTPUT_FETCH_ROW_COUNTS} tensor: {error}"
            ))
        })?;
    values
        .iter()
        .map(|value| {
            usize::try_from(*value).map_err(|error| {
                LayoutError::Postprocess(format!(
                    "{MODEL_OUTPUT_FETCH_ROW_COUNTS} contains an invalid row count {value}: {error}"
                ))
            })
        })
        .collect()
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
