//! Backend-neutral analyzer trait.

/// Common layout analyzer contract implemented by native and web backends.
#[allow(async_fn_in_trait)]
pub trait LayoutAnalyzer {
    /// Analyzes one image and returns page layout detections.
    async fn analyze_image(
        &self,
        image: &image::DynamicImage,
    ) -> Result<super::LayoutPage, super::LayoutError>;
}
