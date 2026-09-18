//! Independent PaddleOCR inference; model execution and decoding never depend on OAR or DocParse core.
mod decode;
mod detect;
mod engine;
mod model;
mod preprocess;
mod wasm_compat;

pub use engine::{OcrArtifacts, PaddleOcrEngine, RecognizedText};

/// Typed failures from OCR artifacts, preprocessing and ONNX execution.
#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    /// Retains the original error when one physical model failure affects several callers.
    #[error("shared OCR batch failed: {0}")]
    Shared(#[source] std::sync::Arc<OcrError>),
    #[error("OCR artifact verification failed: {0}")]
    Artifacts(#[from] docparse_layout::ModelManifestError),
    #[error("OCR backend failed: {0}")]
    Backend(#[from] docparse_layout::LayoutError),
    #[error("OCR runtime failed: {0}")]
    Runtime(#[from] ort::Error),
    #[error("OCR worker failed: {0}")]
    Task(#[from] docparse_common::TaskError),
    #[error("OCR geometry failed: {0}")]
    Geometry(#[from] docparse_layout::GeometryError),
    #[error("invalid OCR data: {0}")]
    InvalidData(String),
    #[error("invalid OCR model: {0}")]
    InvalidModel(String),
    #[error("browser OCR initialization requires explicit model artifacts")]
    ModelArtifactsRequired,
}
