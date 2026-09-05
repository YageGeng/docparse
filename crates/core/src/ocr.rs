use std::collections::BTreeMap;
use std::sync::Arc;

use docparse_layout::{Bbox, PageImage, PageTransform, Polygon};
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

/// Observable OCR availability for one requested page or missing region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OcrContentStatus {
    Recognized,
    Unavailable,
    Failed,
}

/// Immutable page facts supplied to one externally injected OCR engine.
#[derive(Debug, Clone, TypedBuilder)]
pub struct OcrRequest {
    pub page_number: u32,
    pub image: Arc<PageImage>,
    pub transform: PageTransform,
    pub dpi: u32,
    pub missing_regions: Vec<Bbox>,
    pub native_text_coverage: f64,
}

/// One raw OCR fact before DocParse assigns stable source-result indices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct OcrTextItem {
    pub text: String,
    pub bbox: Bbox,
    #[builder(default)]
    pub polygon: Option<Polygon>,
    pub confidence: f64,
}

/// Raw deterministic OCR response plus engine-owned non-sensitive metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct OcrResult {
    pub items: Vec<OcrTextItem>,
    #[builder(default)]
    pub metadata: BTreeMap<String, String>,
}

/// Stable boundary errors returned by custom OCR engines or result validation.
#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    /// The external engine failed without changing existing native/layout facts.
    #[error("OCR engine failed: {message}")]
    Engine { message: String },

    /// One raw OCR result contains unusable text, confidence, or geometry.
    #[error("invalid OCR result {index}: {reason}")]
    InvalidResult { index: usize, reason: String },
}

/// Public asynchronous extension point for caller-provided OCR implementations.
#[async_trait::async_trait]
pub trait OcrEngine: Send + Sync {
    /// Returns a stable human-readable engine name for diagnostics.
    fn name(&self) -> &str;

    /// Recognizes text facts without constructing or mutating semantic blocks.
    async fn recognize(
        &self,
        request: OcrRequest,
    ) -> Result<OcrResult, OcrError>;
}
