// SPDX-License-Identifier: Apache-2.0
// Derived from LiteParse revision b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and modified for docparse.
use std::ffi::CString;
use std::sync::Once;

use crate::document::Document;
use crate::error::PdfiumError;
use crate::ffi;

static INIT: Once = Once::new();

/// A live, locked PDFium session.
///
/// Holding a `Library` proves the current thread has exclusive,
/// process-wide access to PDFium. All PDFium resources ([`Document`] etc.)
/// borrow from this handle, which makes it impossible to call into PDFium
/// without first acquiring the lock.
///
/// `Library` is intentionally **not `Clone`**. To use PDFium from a
/// different scope, call [`Library::init`] again — this will block until
/// any other in-flight PDFium work has finished.
///
/// On `wasm32` there is no threading, so the lock is elided.
///
/// The snippet below must fail to compile — a `Document` cannot outlive
/// the `Library` that opened it:
///
/// ```compile_fail
/// use docparse_pdfium::{Library, Document};
/// let doc: Document<'static> = {
///     let lib = Library::init();
///     lib.load_document("x.pdf", None).unwrap()
/// };
/// // `lib` was dropped above — using `doc` here is a use-after-unlock.
/// let _ = doc.page_count();
/// ```
pub struct Library {
    _guard: crate::wasm_compat::LibraryGuard,
}

impl Library {
    /// Acquires the platform guard before initializing PDFium exactly once.
    pub fn try_init() -> Result<Library, PdfiumError> {
        let guard = crate::wasm_compat::library_guard()?;
        INIT.call_once(|| unsafe { ffi!(FPDF_InitLibrary()) });
        Ok(Library { _guard: guard })
    }

    /// Acquire the process-wide PDFium lock, blocking the current thread
    /// until any other in-flight PDFium work has finished. Initializes the
    /// library on first call.
    ///
    /// Multiple concurrent callers are serialized; only one `Library`
    /// instance exists at a time.
    pub fn init() -> Library {
        // Preserve the legacy infallible API while runtime-facing code uses `try_init`.
        Self::try_init().expect("failed to load pdfium shared library")
    }

    pub fn load_document(
        &self,
        path: &str,
        password: Option<&str>,
    ) -> Result<Document<'_>, PdfiumError> {
        let c_path =
            CString::new(path).map_err(|_| PdfiumError::FileNotFound)?;
        let c_password = password
            .map(|p| CString::new(p).map_err(|_| PdfiumError::OperationFailed))
            .transpose()?;

        let handle = unsafe {
            ffi!(FPDF_LoadDocument(
                c_path.as_ptr(),
                c_password.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            ))
        };

        if handle.is_null() {
            return Err(PdfiumError::from_last_error());
        }

        // PDFium ignores `/UserUnit`, so recover it from the raw bytes (see
        // `crate::user_unit`) when the fork's dict-reading export isn't
        // available. The chunked containment probe keeps the common
        // no-UserUnit case to a single streaming pass with no whole-file
        // allocation.
        let page_user_units = if !Self::user_unit_api_available()
            && crate::user_unit::file_mentions_user_unit(path)
        {
            std::fs::read(path)
                .map(|bytes| Self::page_user_units(handle, &bytes))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(Document {
            handle,
            page_user_units,
            _lib: std::marker::PhantomData,
        })
    }

    /// The input bytes must remain borrowed for the entire returned document lifetime.
    ///
    /// ```compile_fail
    /// use docparse_pdfium::Library;
    /// let library = Library::init();
    /// let document = {
    ///     let bytes = vec![b'%', b'P', b'D', b'F'];
    ///     library.load_document_from_bytes(&bytes, None).unwrap()
    /// };
    /// let _ = document.page_count();
    /// ```
    pub fn load_document_from_bytes<'data>(
        &'data self,
        data: &'data [u8],
        password: Option<&str>,
    ) -> Result<Document<'data>, PdfiumError> {
        let c_password = password
            .map(|p| CString::new(p).map_err(|_| PdfiumError::OperationFailed))
            .transpose()?;
        let data_length = checked_document_length(data.len())?;

        let handle = unsafe {
            ffi!(FPDF_LoadMemDocument(
                data.as_ptr() as *const std::ffi::c_void,
                data_length,
                c_password.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            ))
        };

        if handle.is_null() {
            return Err(PdfiumError::from_last_error());
        }

        // The shared `'data` lifetime now statically keeps both library and bytes alive.
        Ok(Document {
            handle,
            page_user_units: if Self::user_unit_api_available() {
                Vec::new()
            } else {
                Self::page_user_units(handle, data)
            },
            _lib: std::marker::PhantomData,
        })
    }

    /// Build the per-page `/UserUnit` table by matching scanned page objects
    /// against PDFium's reported page sizes (see `crate::user_unit`).
    fn page_user_units(
        handle: pdfium_sys::FPDF_DOCUMENT,
        data: &[u8],
    ) -> Vec<f32> {
        let entries = crate::user_unit::scan_user_units(data);
        if entries.is_empty() {
            return Vec::new();
        }
        let page_count = unsafe { ffi!(FPDF_GetPageCount(handle)) };
        (0..page_count.max(0))
            .map(|index| {
                let mut size = pdfium_sys::FS_SIZEF {
                    width: 0.0,
                    height: 0.0,
                };
                let ok = unsafe {
                    ffi!(FPDF_GetPageSizeByIndexF(handle, index, &mut size))
                };
                if ok != 0 {
                    crate::user_unit::match_user_unit(
                        &entries,
                        size.width,
                        size.height,
                    )
                } else {
                    1.0
                }
            })
            .collect()
    }
}

/// Converts a memory-document size to PDFium's signed C ABI length.
fn checked_document_length(length: usize) -> Result<i32, PdfiumError> {
    i32::try_from(length).map_err(|_source| PdfiumError::OperationFailed)
}

#[cfg(test)]
mod tests {
    use super::checked_document_length;
    use crate::PdfiumError;

    /// Verifies buffers larger than PDFium's signed length ABI are rejected.
    #[test]
    fn memory_document_length_must_fit_i32() {
        assert_eq!(
            checked_document_length(i32::MAX as usize + 1),
            Err(PdfiumError::OperationFailed)
        );
        assert_eq!(checked_document_length(42), Ok(42));
    }
}
