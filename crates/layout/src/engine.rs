use crate::{LayoutDetection, LayoutError, LayoutRequest};

/// Asynchronous, runtime-selectable page layout detection boundary.
#[async_trait::async_trait]
pub trait LayoutEngine: Send + Sync {
    /// Returns a stable diagnostic engine name.
    fn name(&self) -> &str;

    /// Returns the immutable model revision used by this engine.
    fn model_revision(&self) -> &str;

    /// Detects layout regions for one rendered page image.
    async fn detect(
        &self,
        request: LayoutRequest,
    ) -> Result<Vec<LayoutDetection>, LayoutError>;
}
