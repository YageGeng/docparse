//! Platform-specific PDF sources that retain storage for borrowed PDFium handles.
use crate::runtime::pdfium_executor::PdfiumRuntimeError;
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use std::path::PathBuf;

    /// Native PDF input whose storage outlives all borrowed PDFium handles.
    #[derive(Debug, Clone)]
    pub(crate) enum PdfInput {
        Path(PathBuf),
        Bytes(Arc<[u8]>),
    }
    impl PdfInput {
        /// Opens a native source without losing the original input lifetime.
        pub(crate) fn open<'a>(
            &'a self,
            library: &'a pdfium::Library,
        ) -> Result<pdfium::Document<'a>, PdfiumRuntimeError> {
            match self {
                Self::Path(path) => library.load_document(
                    path.to_str().ok_or(PdfiumRuntimeError::NonUtf8Path)?,
                    None,
                ),
                Self::Bytes(bytes) => {
                    library.load_document_from_bytes(bytes, None)
                }
            }
            .map_err(PdfiumRuntimeError::OpenDocument)
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;

    /// Browser PDFs always enter as owned bytes.
    #[derive(Debug, Clone)]
    pub(crate) enum PdfInput {
        Bytes(Arc<[u8]>),
    }
    impl PdfInput {
        /// Opens a borrowed PDF whose owning bytes remain on the actor stack.
        pub(crate) fn open<'a>(
            &'a self,
            library: &'a pdfium::Library,
        ) -> Result<pdfium::Document<'a>, PdfiumRuntimeError> {
            let Self::Bytes(bytes) = self;
            library
                .load_document_from_bytes(bytes, None)
                .map_err(PdfiumRuntimeError::OpenDocument)
        }
    }
}

pub(crate) use platform::PdfInput;
