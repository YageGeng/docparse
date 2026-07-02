//! Native ORT analyzer session.

use std::{sync::Mutex, time::Instant};

use docparse_core::{
    LayoutAnalyzer, LayoutError, LayoutPage, MODEL_INPUT_IM_SHAPE,
    MODEL_INPUT_IMAGE, MODEL_INPUT_SCALE_FACTOR, MODEL_OUTPUT_FETCH_ROW_COUNTS,
    MODEL_OUTPUT_FETCH_ROWS, PostprocessOptions, PreprocessOptions,
    postprocess_fetch_rows_batch, preprocess_images,
};
use ort::{session::Session, value::TensorRef};

use crate::OrtLayoutConfig;

/// Native ORT layout analyzer.
#[derive(Debug)]
pub struct OrtLayoutAnalyzer {
    config: OrtLayoutConfig,
    session: Mutex<Session>,
}

impl OrtLayoutAnalyzer {
    /// Creates a native analyzer after validating the model path.
    pub fn new(config: OrtLayoutConfig) -> Result<Self, LayoutError> {
        if !config.model_path.is_file() {
            return Err(LayoutError::Backend(format!(
                "model file does not exist: {}",
                config.model_path.display()
            )));
        }
        let started_at = Instant::now();
        tracing::info!(
            model_path = %config.model_path.display(),
            "loading layout ONNX model"
        );
        let session = Session::builder()
            .map_err(|error| {
                LayoutError::Backend(format!(
                    "failed to create ORT session builder: {error}"
                ))
            })?
            .commit_from_file(&config.model_path)
            .map_err(|error| {
                LayoutError::Backend(format!(
                    "failed to load ONNX model {}: {error}",
                    config.model_path.display()
                ))
            })?;
        tracing::info!(
            model_path = %config.model_path.display(),
            elapsed_ms = started_at.elapsed().as_millis(),
            "loaded layout ONNX model"
        );
        Ok(Self {
            config,
            session: Mutex::new(session),
        })
    }

    /// Returns the active configuration.
    #[must_use]
    pub fn config(&self) -> &OrtLayoutConfig {
        &self.config
    }

    /// Analyzes multiple images in one ONNX Runtime call and returns pages in input order.
    pub async fn analyze_images(
        &self,
        images: &[image::DynamicImage],
    ) -> Result<Vec<LayoutPage>, LayoutError> {
        self.analyze_images_with_options(images, PostprocessOptions::default())
            .await
    }

    /// Analyzes multiple images with caller-provided postprocessing thresholds.
    pub async fn analyze_images_with_options(
        &self,
        images: &[image::DynamicImage],
        postprocess_options: PostprocessOptions,
    ) -> Result<Vec<LayoutPage>, LayoutError> {
        let total_started_at = Instant::now();
        tracing::info!(
            batch_size = images.len(),
            threshold = postprocess_options.threshold,
            nms_iou_threshold = postprocess_options.nms_iou_threshold,
            cross_label_iou_threshold =
                postprocess_options.cross_label_iou_threshold,
            "starting layout batch analysis"
        );

        let preprocess_started_at = Instant::now();
        let input = preprocess_images(images, PreprocessOptions::default())?;
        tracing::info!(
            batch_size = input.original_sizes.len(),
            tensor_shape = ?input.image.shape(),
            elapsed_ms = preprocess_started_at.elapsed().as_millis(),
            "prepared layout model inputs"
        );

        let tensor_started_at = Instant::now();
        let image_tensor =
            TensorRef::from_array_view(&input.image).map_err(|error| {
                LayoutError::Backend(format!(
                    "failed to create image tensor: {error}"
                ))
            })?;
        let im_shape_tensor = TensorRef::from_array_view(&input.im_shape)
            .map_err(|error| {
                LayoutError::Backend(format!(
                    "failed to create im_shape tensor: {error}"
                ))
            })?;
        let scale_factor_tensor = TensorRef::from_array_view(
            &input.scale_factor,
        )
        .map_err(|error| {
            LayoutError::Backend(format!(
                "failed to create scale_factor tensor: {error}"
            ))
        })?;
        tracing::info!(
            elapsed_ms = tensor_started_at.elapsed().as_millis(),
            "created layout ORT tensors"
        );

        let inference_started_at = Instant::now();
        let mut session = self.session.lock().map_err(|error| {
            LayoutError::Backend(format!(
                "ORT session mutex is poisoned: {error}"
            ))
        })?;
        let outputs = session
            .run(ort::inputs! {
                MODEL_INPUT_IM_SHAPE => im_shape_tensor,
                MODEL_INPUT_IMAGE => image_tensor,
                MODEL_INPUT_SCALE_FACTOR => scale_factor_tensor,
            })
            .map_err(|error| {
                LayoutError::Backend(format!(
                    "failed to run ONNX inference: {error}"
                ))
            })?;
        tracing::info!(
            elapsed_ms = inference_started_at.elapsed().as_millis(),
            "completed layout ONNX inference"
        );

        let output_started_at = Instant::now();
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
            output_shape = ?shape,
            row_counts = ?row_counts,
            elapsed_ms = output_started_at.elapsed().as_millis(),
            "extracted layout model outputs"
        );

        let postprocess_started_at = Instant::now();
        let pages = postprocess_fetch_rows_batch(
            values,
            columns,
            &row_counts,
            &input.original_sizes,
            postprocess_options,
        )?;
        let total_blocks: usize =
            pages.iter().map(|page| page.blocks.len()).sum();
        tracing::info!(
            pages = pages.len(),
            blocks = total_blocks,
            postprocess_elapsed_ms =
                postprocess_started_at.elapsed().as_millis(),
            total_elapsed_ms = total_started_at.elapsed().as_millis(),
            "completed layout batch analysis"
        );

        Ok(pages)
    }
}

impl LayoutAnalyzer for OrtLayoutAnalyzer {
    async fn analyze_image(
        &self,
        image: &image::DynamicImage,
    ) -> Result<LayoutPage, LayoutError> {
        // Route the legacy single-image API through the batch path so both
        // code paths consume the row-count output and decode model output identically.
        let pages = self.analyze_images(std::slice::from_ref(image)).await?;
        pages.into_iter().next().ok_or_else(|| {
            LayoutError::Backend(
                "ONNX batch inference returned no pages".to_owned(),
            )
        })
    }
}

/// Extracts per-image detection counts from the PP-StructureV3 batch output.
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

#[cfg(test)]
mod tests {
    use super::OrtLayoutAnalyzer;
    use crate::OrtLayoutConfig;

    #[test]
    fn new_rejects_missing_model_file() {
        let config = OrtLayoutConfig {
            model_path: std::path::PathBuf::from(
                "models/pp-structure-v3-onnx/missing.onnx",
            ),
            ..OrtLayoutConfig::default()
        };

        let error = OrtLayoutAnalyzer::new(config)
            .expect_err("missing model should fail");
        assert!(error.to_string().contains("model file does not exist"));
    }
}
