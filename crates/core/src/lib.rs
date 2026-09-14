//! Native PDF extraction and layout-text fusion for docparse.

pub mod wasm_compat;
pub use wasm_compat::*;

mod context;
mod error;
mod extract;
mod fusion;
mod glyph_resolver;
mod label_policy;
mod line;
mod ocr;
mod page;
mod parser;
mod pdfium;
mod progress;
mod render;
mod runtime;
mod semantic;
mod table;
mod text_rules;
mod types;
mod validate;
mod watermark;

pub use context::{DocumentContextBuilder, DocumentLinker};
pub use docparse_ocr::{OcrArtifacts, PaddleOcrEngine};
pub use error::{
    ContextError, ExtractError, LineError, OrderError, PageAnalysisError,
    SemanticError, ValidationError,
};
pub use extract::ExtractedPage;
pub use extract::metadata::PageProbe;
pub use glyph_resolver::{GLYPH_RESOLVER_FONT_SIZE, GlyphResolver};
pub use ocr::{
    OcrContentStatus, OcrEngine, OcrError, OcrRequest, OcrResult, OcrTextItem,
};
pub use parser::{
    DocParseError, DocParser, DocParserBuilder, PageInput, ParseOptions,
    ParserArtifacts,
};
pub use progress::{ParseObserver, ParseProgress, Timing};
pub use render::{
    JsonRenderer, MarkdownRenderer, OverlayArtifacts, OverlayRenderer,
    RenderError, RenderView, TextRenderer,
};
pub use table::{
    Table, TableCell, TableCellLine, TableEvidence, TableMode, TableOptions,
    TableRule, TableStructureEngine, TableStructureError, TableStructureSource,
    TableTextSpan, TableWord, TaggedTable, TaggedTableCell, TsrGeometryPolicy,
    TsrRequestReason, TsrTableInput, TsrTableRequest,
};
pub use types::*;
pub use validate::ResultValidator;

pub use crate::pdfium::{
    LocalPdfiumProvider, PdfInput, PdfiumProvider, PdfiumRuntimeError,
    PdfiumSession, PreScannedPage, RenderedPage,
};
