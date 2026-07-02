//! Native ORT analyzer session.

use std::sync::Mutex;

use docparse_core::{
    LayoutAnalyzer, LayoutError, LayoutPage, PostprocessOptions,
    PreprocessOptions, postprocess_fetch_rows, preprocess_image,
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
}

impl LayoutAnalyzer for OrtLayoutAnalyzer {
    async fn analyze_image(
        &self,
        image: &image::DynamicImage,
    ) -> Result<LayoutPage, LayoutError> {
        let input = preprocess_image(image, PreprocessOptions::default())?;
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
        let mut session = self.session.lock().map_err(|error| {
            LayoutError::Backend(format!(
                "ORT session mutex is poisoned: {error}"
            ))
        })?;
        let outputs = session
            .run(ort::inputs! {
                "im_shape" => im_shape_tensor,
                "image" => image_tensor,
                "scale_factor" => scale_factor_tensor,
            })
            .map_err(|error| {
                LayoutError::Backend(format!(
                    "failed to run ONNX inference: {error}"
                ))
            })?;
        let fetch = outputs.get("fetch_name_0").ok_or_else(|| {
            LayoutError::Backend(
                "ONNX output fetch_name_0 is missing".to_owned(),
            )
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
