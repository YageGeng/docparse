mod json;
mod markdown;
mod overlay;
mod text;

pub use json::JsonRenderer;
pub use markdown::MarkdownRenderer;
pub use overlay::{OverlayArtifacts, OverlayRenderer};
pub use text::TextRenderer;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use docparse_config::ValidatedConfig;

use crate::runtime::{PdfInput, PdfiumExecutor};

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

/// Reopens one PDF and writes per-page PNG/SVG overlays without rerunning layout inference.
pub async fn write_pdf_overlays(
    config: &ValidatedConfig,
    input: &Path,
    document: &crate::DocumentResult,
    output_dir: &Path,
) -> Result<Vec<PathBuf>, RenderError> {
    write_pdf_overlays_for_pages(config, input, document, output_dir, None)
        .await
}

/// Writes a deterministic subset of page overlays while still validating source page count.
pub async fn write_pdf_overlays_for_pages(
    config: &ValidatedConfig,
    input: &Path,
    document: &crate::DocumentResult,
    output_dir: &Path,
    page_numbers: Option<&BTreeSet<u32>>,
) -> Result<Vec<PathBuf>, RenderError> {
    std::fs::create_dir_all(output_dir)?;
    let executor = PdfiumExecutor::open(
        PdfInput::Path(input.to_path_buf()),
        config.runtime(),
    )
    .await
    .map_err(|error| RenderError::Pdfium(error.to_string()))?;
    let operation = async {
        if usize::try_from(executor.page_count()).unwrap_or(usize::MAX)
            != document.pages.len()
        {
            return Err(RenderError::PageMismatch(format!(
                "source has {} pages but result has {}",
                executor.page_count(),
                document.pages.len()
            )));
        }
        let mut outputs = Vec::with_capacity(document.pages.len() * 2);
        for page in &document.pages {
            if page_numbers
                .is_some_and(|selected| !selected.contains(&page.page_number))
            {
                continue;
            }
            let rendered = executor
                .render_page(page.page_number, config.render())
                .await
                .map_err(|error| RenderError::Pdfium(error.to_string()))?;
            let png_name = format!("page-{:04}.png", page.page_number);
            let svg_name = format!("page-{:04}.svg", page.page_number);
            let artifacts = OverlayRenderer::render_page(
                rendered.image.as_ref(),
                page,
                &png_name,
            )?;
            let png_path = output_dir.join(png_name);
            let svg_path = output_dir.join(svg_name);
            std::fs::write(&png_path, artifacts.png)?;
            std::fs::write(&svg_path, artifacts.svg)?;
            outputs.push(png_path);
            outputs.push(svg_path);
        }
        Ok(outputs)
    }
    .await;
    let close_result = executor
        .close()
        .await
        .map_err(|error| RenderError::Pdfium(error.to_string()));
    let outputs = operation?;
    close_result?;
    Ok(outputs)
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
