//! PDF rendering support.

use std::path::Path;

use image::{DynamicImage, RgbImage};
use liteparse_pdfium::Document;

use crate::{DocumentPage, LayoutError};

/// Options for rendering PDF pages to images.
#[derive(Debug, Clone, Copy)]
pub struct PdfRenderOptions {
    /// Maximum number of pages to render.
    pub max_pages: usize,
    /// Render resolution in dots per inch.
    pub dpi: f32,
}

/// Renders PDF pages with PDFium.
pub fn render_pdf_pages(
    path: &Path,
    options: PdfRenderOptions,
) -> Result<Vec<DocumentPage>, LayoutError> {
    let lib = std::panic::catch_unwind(liteparse_pdfium::Library::init)
        .map_err(|panic| {
            LayoutError::Preprocess(format!(
                "failed to initialize pdfium: {}",
                panic_message(panic)
            ))
        })?;
    let path_str = path.to_str().ok_or_else(|| {
        LayoutError::Preprocess(format!(
            "PDF path is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    let document = lib.load_document(path_str, None).map_err(|error| {
        LayoutError::Preprocess(format!(
            "failed to open PDF {}: {error}",
            path.display()
        ))
    })?;
    render_pdf_document(&document, &path.display().to_string(), options)
}

/// Renders PDF bytes with PDFium.
pub fn render_pdf_bytes(
    data: &[u8],
    options: PdfRenderOptions,
) -> Result<Vec<DocumentPage>, LayoutError> {
    let lib = std::panic::catch_unwind(liteparse_pdfium::Library::init)
        .map_err(|panic| {
            LayoutError::Preprocess(format!(
                "failed to initialize pdfium: {}",
                panic_message(panic)
            ))
        })?;
    let document =
        lib.load_document_from_bytes(data, None).map_err(|error| {
            LayoutError::Preprocess(format!(
                "failed to open PDF bytes: {error}"
            ))
        })?;

    render_pdf_document(&document, "PDF bytes", options)
}

fn render_pdf_document(
    document: &Document<'_>,
    source: &str,
    options: PdfRenderOptions,
) -> Result<Vec<DocumentPage>, LayoutError> {
    let page_count =
        usize::try_from(document.page_count()).map_err(|error| {
            LayoutError::Preprocess(format!(
                "PDF page count is invalid: {error}"
            ))
        })?;
    let page_limit = page_count.min(options.max_pages);
    let mut pages = Vec::with_capacity(page_limit);

    for page_index in 0..page_limit {
        let page = document.page(page_index as i32).map_err(|error| {
            LayoutError::Preprocess(format!(
                "failed to load PDF page {} from {source}: {error}",
                page_index + 1,
            ))
        })?;
        let bitmap = page.render(options.dpi).map_err(|error| {
            LayoutError::Preprocess(format!(
                "failed to render PDF page {} from {source}: {error}",
                page_index + 1,
            ))
        })?;
        let width = u32::try_from(bitmap.width()).map_err(|error| {
            LayoutError::Preprocess(format!(
                "rendered PDF width is invalid: {error}"
            ))
        })?;
        let height = u32::try_from(bitmap.height()).map_err(|error| {
            LayoutError::Preprocess(format!(
                "rendered PDF height is invalid: {error}"
            ))
        })?;
        let image = RgbImage::from_raw(width, height, bitmap.to_rgb())
            .map(DynamicImage::ImageRgb8)
            .ok_or_else(|| {
                LayoutError::Preprocess(format!(
                    "failed to build RGB image for PDF page {} from {source}",
                    page_index + 1,
                ))
            })?;

        pages.push(DocumentPage { page_index, image });
    }

    Ok(pages)
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    "unknown panic".to_owned()
}
