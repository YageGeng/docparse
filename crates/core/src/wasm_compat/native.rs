//! Native filesystem parsing and overlay output, selected by the compatibility entry point.
use super::PdfInput;
use crate::{DocParseError, DocParser, DocumentResult};
use std::path::{Path, PathBuf};

impl DocParser {
    /// Parses one filesystem PDF while retaining path context on failure.
    pub async fn parse_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<DocumentResult, DocParseError> {
        let path = path.as_ref().to_path_buf();
        self.runtime()
            .parse_document_with_options(
                PdfInput::Path(path.clone()),
                crate::ParseOptions::default(),
            )
            .await
            .map_err(DocParseError::from)
            .map_err(|source| DocParseError::ParsePath {
                path,
                source: Box::new(source),
            })
    }
    /// Runs the async path parser from an ordinary synchronous thread.
    /// Parses a native path with per-call table policy and an optional external structure engine.
    pub async fn parse_path_with_options(
        &self,
        path: impl AsRef<Path>,
        options: crate::ParseOptions<'_>,
    ) -> Result<DocumentResult, DocParseError> {
        options.table.validate(options.table_engine.is_some())?;
        let path = path.as_ref().to_path_buf();
        self.runtime()
            .parse_document_with_options(PdfInput::Path(path.clone()), options)
            .await
            .map_err(DocParseError::from)
            .map_err(|source| DocParseError::ParsePath {
                path,
                source: Box::new(source),
            })
    }

    /// Runs path parsing without nesting a Tokio runtime.
    pub fn parse_path_blocking(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<DocumentResult, DocParseError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(DocParseError::BlockingInsideRuntime);
        }
        let path = path.as_ref().to_path_buf();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(DocParseError::BlockingRuntime)?;
        runtime.block_on(self.parse_path(path))
    }
}
use crate::runtime::PdfiumExecutor;
use crate::{OverlayRenderer, RenderError};
use docparse_config::ValidatedConfig;
use std::collections::BTreeSet;

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
