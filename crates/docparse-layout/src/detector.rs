//! Public layout detector API.

use std::path::PathBuf;

use burn_tensor::backend::Backend;
use image::DynamicImage;

use crate::ml::backend::{AutoBackend, auto_device};
#[cfg(not(target_arch = "wasm32"))]
use crate::pp_doclayout::load_pp_doclayout_runtime;
use crate::pp_doclayout::{
    LayoutDetection, PpDocLayoutRuntime, load_pp_doclayout_runtime_from_bytes,
};

/// Default layout backend: direct wgpu.
pub(crate) type DefaultLayoutBackend = AutoBackend;

/// Supported document layout model checkpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LayoutModel {
    /// `PaddlePaddle/PP-DocLayoutV3_safetensors`.
    #[default]
    PpDocLayoutV3,
}

/// Layout detector options.
#[derive(Debug, Clone, Default)]
pub struct LayoutOptions {
    /// Which layout checkpoint to load.
    pub model: LayoutModel,
    /// Optional model download cache directory.
    pub cache_dir: Option<PathBuf>,
}

/// In-memory model files needed by browser-side layout inference.
#[derive(Debug, Clone, Copy)]
pub struct LayoutModelBytes<'a> {
    /// `config.json` bytes.
    pub config_json: &'a [u8],
    /// `preprocessor_config.json` bytes.
    pub preprocessor_json: &'a [u8],
    /// `model.safetensors` bytes.
    pub weights: &'a [u8],
}

/// Layout detector runtime.
#[derive(Debug)]
pub struct LayoutDetector<B: Backend = DefaultLayoutBackend> {
    runtime: PpDocLayoutRuntime<B>,
    device: B::Device,
}

/// Layout detection failure.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// Layout model files or weights failed to load.
    #[error("Layout model load failed")]
    Load {
        /// Underlying loader error.
        source: anyhow::Error,
    },

    /// Layout preprocessing or inference failed.
    #[error("Layout detection failed")]
    Detect {
        /// Underlying detection error.
        source: anyhow::Error,
    },
}

/// Layout output for one page/image.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct LayoutPage {
    /// Source image width in pixels.
    pub width: u32,
    /// Source image height in pixels.
    pub height: u32,
    /// Detected layout blocks in reading order.
    pub blocks: Vec<LayoutBlock>,
}

/// Detected layout block.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct LayoutBlock {
    /// Detector label.
    pub label: String,
    /// Detector confidence from 0 to 1.
    pub confidence: f32,
    /// Bounding box in image pixel coordinates.
    pub bbox: LayoutRect,
    /// Detector reading order.
    pub order: i64,
}

/// Axis-aligned layout bounding box.
#[derive(
    Debug, Clone, Copy, PartialEq, serde::Deserialize, serde::Serialize,
)]
pub struct LayoutRect {
    /// Left coordinate in pixels.
    pub x: f32,
    /// Top coordinate in pixels.
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
}

impl LayoutDetector<DefaultLayoutBackend> {
    /// Load default layout detector.
    ///
    /// # Errors
    ///
    /// Returns an error when model files cannot be loaded.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn new(options: LayoutOptions) -> Result<Self, LayoutError> {
        let device = auto_device();
        Self::new_with_device(&device, options).await
    }

    /// Load default layout detector from in-memory model files.
    ///
    /// # Errors
    ///
    /// Returns an error when config or weights cannot be loaded.
    pub fn from_model_bytes(
        model: LayoutModelBytes<'_>,
    ) -> Result<Self, LayoutError> {
        let device = auto_device();
        Self::from_model_bytes_with_device(&device, model)
    }
}

impl<B> LayoutDetector<B>
where
    B: Backend<FloatElem = f32>,
{
    /// Load layout detector on caller-provided device.
    ///
    /// # Errors
    ///
    /// Returns an error when model files cannot be loaded.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn new_with_device(
        device: &B::Device,
        options: LayoutOptions,
    ) -> Result<Self, LayoutError> {
        let runtime = load_pp_doclayout_runtime(device, options.cache_dir)
            .await
            .map_err(|source| LayoutError::Load { source })?;

        Ok(Self {
            runtime,
            device: device.clone(),
        })
    }

    /// Load layout detector on caller-provided device from in-memory files.
    ///
    /// # Errors
    ///
    /// Returns an error when config or weights cannot be loaded.
    pub fn from_model_bytes_with_device(
        device: &B::Device,
        model: LayoutModelBytes<'_>,
    ) -> Result<Self, LayoutError> {
        let runtime = load_pp_doclayout_runtime_from_bytes(
            device,
            model.config_json,
            model.preprocessor_json,
            model.weights,
        )
        .map_err(|source| LayoutError::Load { source })?;

        Ok(Self {
            runtime,
            device: device.clone(),
        })
    }

    /// Detect layout blocks from decoded image.
    ///
    /// # Errors
    ///
    /// Returns an error when preprocessing or inference fails.
    pub fn detect_image(
        &self,
        image: &DynamicImage,
    ) -> Result<LayoutPage, LayoutError> {
        let detections = self
            .runtime
            .detect_image(image, &self.device)
            .map_err(|source| LayoutError::Detect { source })?;

        Ok(page_from_detections(image, detections))
    }

    /// Detect layout blocks from decoded image without blocking wasm readbacks.
    ///
    /// # Errors
    ///
    /// Returns an error when preprocessing or inference fails.
    pub async fn detect_image_async(
        &self,
        image: &DynamicImage,
    ) -> Result<LayoutPage, LayoutError> {
        let detections = self
            .runtime
            .detect_image_async(image, &self.device)
            .await
            .map_err(|source| LayoutError::Detect { source })?;

        Ok(page_from_detections(image, detections))
    }
}

/// Converts internal model detections into the public page shape.
fn page_from_detections(
    image: &DynamicImage,
    detections: Vec<LayoutDetection>,
) -> LayoutPage {
    let blocks = detections
        .into_iter()
        .map(|detection| LayoutBlock {
            label: detection.label,
            confidence: detection.score,
            bbox: LayoutRect {
                x: detection.bbox[0],
                y: detection.bbox[1],
                width: detection.bbox[2] - detection.bbox[0],
                height: detection.bbox[3] - detection.bbox[1],
            },
            order: detection.order,
        })
        .collect();

    LayoutPage {
        width: image.width(),
        height: image.height(),
        blocks,
    }
}
