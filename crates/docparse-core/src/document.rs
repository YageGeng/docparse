//! Document input loading.

use std::path::PathBuf;

use image::DynamicImage;

use crate::layout::LayoutError;
use crate::pdf::{PdfRenderOptions, render_pdf_pages};

/// A document input accepted by layout analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentInput {
    /// Path to an image or PDF file.
    pub path: PathBuf,
}

/// Options for loading pages from a document input.
#[derive(Debug, Clone, Copy)]
pub struct LoadDocumentOptions {
    /// Maximum number of pages to load from a PDF.
    pub max_pages: usize,
    /// PDF render resolution.
    pub pdf_dpi: f32,
}

impl Default for LoadDocumentOptions {
    fn default() -> Self {
        Self {
            max_pages: 1,
            pdf_dpi: 144.0,
        }
    }
}

/// One rasterized page ready for layout analysis.
#[derive(Debug, Clone)]
pub struct DocumentPage {
    /// Zero-based page index within the input document.
    pub page_index: usize,
    /// Rasterized page image.
    pub image: DynamicImage,
}

/// Loads image files directly and renders PDF pages with PDFium.
pub fn load_document_pages(
    input: &DocumentInput,
    options: LoadDocumentOptions,
) -> Result<Vec<DocumentPage>, LayoutError> {
    if options.max_pages == 0 {
        return Err(LayoutError::Preprocess(
            "max_pages must be greater than 0".to_owned(),
        ));
    }
    if options.pdf_dpi <= 0.0 {
        return Err(LayoutError::Preprocess(
            "pdf_dpi must be greater than 0".to_owned(),
        ));
    }

    if is_pdf(&input.path) {
        render_pdf_pages(
            &input.path,
            PdfRenderOptions {
                max_pages: options.max_pages,
                dpi: options.pdf_dpi,
            },
        )
    } else {
        load_image_page(input)
    }
}

fn load_image_page(
    input: &DocumentInput,
) -> Result<Vec<DocumentPage>, LayoutError> {
    let image = image::open(&input.path).map_err(|error| {
        LayoutError::Preprocess(format!(
            "failed to open image {}: {error}",
            input.path.display()
        ))
    })?;

    Ok(vec![DocumentPage {
        page_index: 0,
        image,
    }])
}

fn is_pdf(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use image::RgbImage;

    use super::{DocumentInput, LoadDocumentOptions, load_document_pages};

    fn tiny_fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "docparse-core-test-tiny-{}.png",
            std::process::id()
        ));
        RgbImage::new(1, 1)
            .save(&path)
            .expect("tiny image fixture should be written");
        path
    }

    #[test]
    fn image_input_loads_as_single_zero_based_page() {
        let pages = load_document_pages(
            &DocumentInput {
                path: tiny_fixture(),
            },
            LoadDocumentOptions {
                max_pages: 3,
                pdf_dpi: 144.0,
            },
        )
        .expect("image input should load");

        assert_eq!(pages.len(), 1);
        let page = pages.first().expect("one page should be loaded");
        assert_eq!(page.page_index, 0);
        assert_eq!(page.image.width(), 1);
        assert_eq!(page.image.height(), 1);
    }

    #[test]
    fn max_pages_must_be_non_zero() {
        let error = load_document_pages(
            &DocumentInput {
                path: tiny_fixture(),
            },
            LoadDocumentOptions {
                max_pages: 0,
                pdf_dpi: 144.0,
            },
        )
        .expect_err("zero max_pages should be rejected");

        assert!(
            error
                .to_string()
                .contains("max_pages must be greater than 0")
        );
    }
}
