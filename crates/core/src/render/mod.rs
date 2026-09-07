mod json;
mod markdown;
mod overlay;
mod text;

pub use json::JsonRenderer;
pub use markdown::MarkdownRenderer;
pub use overlay::{OverlayArtifacts, OverlayRenderer};
pub use text::TextRenderer;

/// Selects literal page content or a relation-aware presentation view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderView {
    Raw,
    Semantic,
}

/// Failures produced while serializing canonical or diagnostic output.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// Canonical JSON serialization failed.
    #[error("failed to serialize document JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// RGB pixels could not be encoded as PNG.
    #[error("failed to encode overlay PNG: {0}")]
    Image(#[from] image::ImageError),
    /// An output buffer rejected encoded PNG bytes.
    #[error("failed to write overlay bytes: {0}")]
    Io(#[from] std::io::Error),
    /// In-memory SVG formatting failed unexpectedly.
    #[error("failed to format overlay SVG: {0}")]
    Format(#[from] std::fmt::Error),
    /// The PDFium overlay source failed before model-independent diagnostics rendered.
    #[error("failed to render PDF overlay source: {0}")]
    Pdfium(String),
    /// The canonical result and source PDF do not describe the same page set.
    #[error("overlay page mismatch: {0}")]
    PageMismatch(String),
}

/// Collects block IDs hidden only in the semantic rendering projection.
pub(crate) fn hidden_repeated_chrome(
    document: &crate::DocumentResult,
) -> std::collections::BTreeSet<String> {
    document
        .relations
        .relations
        .iter()
        .filter(|relation| relation.kind == crate::RelationKind::RepeatedChrome)
        .flat_map(|relation| {
            [&relation.source.block_id, &relation.target.block_id]
        })
        .map(|id| id.as_str().to_owned())
        .collect()
}

/// Renders one line in item order and inserts placeholders only for missing formulas.
pub(crate) fn render_line(
    line: &crate::Line,
    formula_placeholder: &str,
) -> String {
    let mut rendered = String::new();
    for ordinal in 0..=line.text_items.len() {
        for span in line.inline_spans.iter().filter(|span| {
            span.content_status == crate::InlineContentStatus::Missing
                && span.text_item_range.start == ordinal
        }) {
            let _ = span;
            rendered.push_str(formula_placeholder);
        }
        if let Some(item) = line.text_items.get(ordinal) {
            rendered.push_str(&item.raw_text);
        }
    }
    rendered
}
