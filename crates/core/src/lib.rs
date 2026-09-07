//! Native PDF extraction and layout-text fusion for docparse.

pub mod wasm_compat;
pub use wasm_compat::*;

mod context;
mod error;
mod extract;
mod fusion;
mod label_policy;
mod line;
mod ocr;
mod page;
mod parser;
mod render;
mod runtime;
mod semantic;
mod types;
mod validate;

pub use context::{DocumentContextBuilder, DocumentLinker};
pub use error::{
    ContextError, ExtractError, LineError, OrderError, PageAnalysisError,
    SemanticError, ValidationError,
};
pub use extract::ExtractedPage;
pub use extract::metadata::PageProbe;
pub use ocr::{
    OcrContentStatus, OcrEngine, OcrError, OcrRequest, OcrResult, OcrTextItem,
};
pub use parser::{DocParseError, DocParser, DocParserBuilder, PageInput};
pub use render::{
    JsonRenderer, MarkdownRenderer, OverlayArtifacts, OverlayRenderer,
    RenderError, RenderView, TextRenderer,
};
pub use types::*;
pub use validate::ResultValidator;
