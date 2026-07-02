//! Public layout result types.

/// Layout analysis failure.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// The input image cannot be preprocessed.
    #[error("layout preprocessing failed: {0}")]
    Preprocess(String),
    /// The model output cannot be decoded.
    #[error("layout postprocessing failed: {0}")]
    Postprocess(String),
    /// A backend failed to load or run.
    #[error("layout backend failed: {0}")]
    Backend(String),
}

/// One page worth of layout detections.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutPage {
    /// Original image width in pixels.
    pub width: u32,
    /// Original image height in pixels.
    pub height: u32,
    /// Detected layout blocks.
    pub blocks: Vec<LayoutBlock>,
}

/// One detected layout block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutBlock {
    /// Typed detector label.
    pub label: super::LayoutLabel,
    /// Detector confidence from 0 to 1.
    pub score: f32,
    /// Axis-aligned xywh bounding box in original image pixels.
    pub bbox: LayoutBox,
    /// Reading order, when emitted by the model.
    pub order: Option<i64>,
}

/// Axis-aligned bounding box in xywh format.
#[derive(
    Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct LayoutBox {
    /// Left coordinate.
    pub x: f32,
    /// Top coordinate.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl LayoutBox {
    /// Builds a clamped xywh box from xyxy coordinates.
    #[must_use]
    pub fn from_xyxy_clamped(
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        image_width: u32,
        image_height: u32,
    ) -> Self {
        let max_x = image_width as f32;
        let max_y = image_height as f32;
        let left = x1.clamp(0.0, max_x);
        let top = y1.clamp(0.0, max_y);
        let right = x2.clamp(0.0, max_x);
        let bottom = y2.clamp(0.0, max_y);
        Self {
            x: left,
            y: top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LayoutBox;

    #[test]
    fn bbox_converts_xyxy_to_clamped_xywh() {
        let bbox =
            LayoutBox::from_xyxy_clamped(-10.0, 5.0, 120.0, 80.0, 100, 60);

        assert_eq!(
            bbox,
            LayoutBox {
                x: 0.0,
                y: 5.0,
                width: 100.0,
                height: 55.0
            }
        );
    }
}
