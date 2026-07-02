//! Backend-independent document parsing primitives.

pub mod document;
pub mod layout;
pub mod pdf;

pub use document::{
    DocumentInput, DocumentPage, LoadDocumentOptions, load_document_pages,
};
pub use layout::{
    LayoutAnalyzer, LayoutBatchInput, LayoutBlock, LayoutBox, LayoutError,
    LayoutInput, LayoutLabel, LayoutPage, LayoutTensor, ModelOutput,
    OriginalImageSize, PostprocessOptions, PreprocessOptions,
    postprocess_fetch_rows, postprocess_fetch_rows_batch, preprocess_image,
    preprocess_images,
};
