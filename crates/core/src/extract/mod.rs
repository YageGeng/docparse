mod glyph;
pub(crate) mod metadata;
pub(crate) mod text;

use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use crate::TextItem;

/// Native text facts and lightweight page geometry extracted before layout inference.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct ExtractedPage {
    pub page_number: u32,
    pub width: f64,
    pub height: f64,
    pub rotation: i32,
    #[builder(default)]
    pub content_bounds: Option<Bbox>,
    pub text_items: Vec<TextItem>,
    /// Measured words, vector separators, and tagged cells used only during table reconstruction.
    #[builder(default)]
    pub table_evidence: crate::TableEvidence,
    /// Explicit watermark annotation bounds, independent of the native text layer.
    #[builder(default)]
    pub watermark_annotations: Vec<Bbox>,
    /// Transient rule decisions become final Block evidence after ownership is resolved.
    #[builder(default)]
    pub watermark_evidence:
        std::collections::BTreeMap<crate::TextItemId, crate::Evidence>,
}
