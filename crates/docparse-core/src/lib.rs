//! Backend-independent document parsing primitives.

pub mod document;
pub mod layout;
pub mod pdf;

pub use document::{
    DocumentInput, DocumentPage, LoadDocumentOptions, load_document_pages,
};
pub use layout::{
    LayoutAnalyzer, LayoutBlock, LayoutBox, LayoutError, LayoutInput,
    LayoutLabel, LayoutPage, LayoutTensor, ModelOutput, PostprocessOptions,
    PreprocessOptions, postprocess_fetch_rows, preprocess_image,
};
