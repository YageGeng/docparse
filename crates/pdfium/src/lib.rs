// SPDX-License-Identifier: Apache-2.0
// Derived from LiteParse revision b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and modified for docparse.
mod bitmap;
mod document;
mod error;
mod font;
mod library;
mod page;
mod struct_tree;
mod text_page;
mod types;
mod user_unit;

pub use bitmap::Bitmap;
pub use document::{
    Document, FormEnvironment, OutlineEntry, SignatureSummary, XfaPacket,
};
pub use error::PdfiumError;
pub use font::{Font, FontType};
pub use library::Library;
pub use page::{
    ImageBounds, ImageObjectInfo, ImageObjects, Page, PathObject, PathSegment,
    PdfAnnotation, PdfFormField, PdfLink, SegmentKind, ViewportTransform,
};
pub use struct_tree::{StructNode, StructureAttributeValue, StructureElement};
pub use text_page::{TextChar, TextCharIter, TextPage};
pub use types::*;

/// Raw FFI layer, re-exported for callers that need to hold raw handles
/// (e.g. `FPDF_PAGEOBJECT`) returned by the safe wrappers.
pub use pdfium_sys;

mod wasm_compat;
pub(crate) use wasm_compat::ffi;
