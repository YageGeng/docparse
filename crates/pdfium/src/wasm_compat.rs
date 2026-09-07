//! Platform-specific PDFium locking, symbol resolution, and FFI dispatch.
#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!("docparse PDFium Web builds require the wasm feature");
#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
compile_error!("docparse supports wasm32-unknown-unknown browser builds only");
use crate::{Document, Library, TextPage};

/// Unified FFI call macro. On wasm, calls pdfium_sys extern functions directly.
/// On non-wasm, calls through the runtime-loaded function pointers.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
macro_rules! ffi {
    ($fn_name:ident($($args:expr),* $(,)?)) => {
        (pdfium_sys::dynamic::pdfium().$fn_name)($($args),*)
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
macro_rules! ffi {
    ($fn_name:ident($($args:expr),* $(,)?)) => {
        pdfium_sys::$fn_name($($args),*)
    }
}

pub(crate) use ffi;

/// The `fpdf_signature` entry points, resolved together. `None` when the
/// loaded pdfium build does not export them.
pub(crate) struct SignatureApi {
    pub(crate) count:
        unsafe extern "C" fn(pdfium_sys::FPDF_DOCUMENT) -> std::os::raw::c_int,
    pub(crate) object: unsafe extern "C" fn(
        pdfium_sys::FPDF_DOCUMENT,
        std::os::raw::c_int,
    ) -> pdfium_sys::FPDF_SIGNATURE,
    pub(crate) byte_range: unsafe extern "C" fn(
        pdfium_sys::FPDF_SIGNATURE,
        *mut std::os::raw::c_int,
        std::os::raw::c_ulong,
    ) -> std::os::raw::c_ulong,
}

impl SignatureApi {
    /// Resolves the optional native signature functions as one usable API.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    pub(crate) fn load() -> Option<Self> {
        let bindings = pdfium_sys::dynamic::pdfium();
        Some(Self {
            count: bindings.FPDF_GetSignatureCount?,
            object: bindings.FPDF_GetSignatureObject?,
            byte_range: bindings.FPDFSignatureObj_GetByteRange?,
        })
    }

    /// Uses the signature functions supplied by the pinned browser archive.
    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    pub(crate) fn load() -> Option<Self> {
        Some(Self {
            count: pdfium_sys::FPDF_GetSignatureCount,
            object: pdfium_sys::FPDF_GetSignatureObject,
            byte_range: pdfium_sys::FPDFSignatureObj_GetByteRange,
        })
    }
}

impl<'lib> Document<'lib> {
    /// Read `/UserUnit` through the fork's `FPDFPage_GetUserUnit` export.
    /// `None` when the loaded pdfium binary does not provide it.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    pub(crate) fn user_unit_from_api(
        page: pdfium_sys::FPDF_PAGE,
    ) -> Option<f32> {
        let get_user_unit =
            pdfium_sys::dynamic::pdfium().FPDFPage_GetUserUnit?;
        let user_unit = unsafe { get_user_unit(page) };
        // The API already clamps to >= 1.0; guard anyway so a misbehaving
        // binary can't zero out all geometry.
        Some(if user_unit.is_finite() && user_unit >= 1.0 {
            user_unit
        } else {
            1.0
        })
    }
    /// On wasm the export is statically linked (the pinned pdfium-binaries
    /// release ships it), so unlike the dynamic path this can never be
    /// absent at runtime — bumping the pin below a release that carries
    /// `FPDFPage_GetUserUnit` would be a link error, not a silent fallback.
    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    pub(crate) fn user_unit_from_api(
        page: pdfium_sys::FPDF_PAGE,
    ) -> Option<f32> {
        let user_unit = unsafe { pdfium_sys::FPDFPage_GetUserUnit(page) };
        Some(if user_unit.is_finite() && user_unit >= 1.0 {
            user_unit
        } else {
            1.0
        })
    }
}
/// The optional page-flatten API, resolved together so a build missing either
/// half degrades to "no flattening" rather than failing the whole pdfium load.
pub(crate) struct FlattenApi {
    pub(crate) flatten: unsafe extern "C" fn(
        pdfium_sys::FPDF_PAGE,
        std::os::raw::c_int,
    ) -> std::os::raw::c_int,
    pub(crate) set_flags: unsafe extern "C" fn(
        pdfium_sys::FPDF_ANNOTATION,
        std::os::raw::c_int,
    ) -> pdfium_sys::FPDF_BOOL,
}

impl FlattenApi {
    /// Resolves native flattening only when both required functions are available.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    pub(crate) fn load() -> Option<Self> {
        let bindings = pdfium_sys::dynamic::pdfium();
        Some(Self {
            flatten: bindings.FPDFPage_Flatten?,
            set_flags: bindings.FPDFAnnot_SetFlags?,
        })
    }

    /// Uses the flattening functions supplied by the pinned browser archive.
    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    pub(crate) fn load() -> Option<Self> {
        Some(Self {
            flatten: pdfium_sys::FPDFPage_Flatten,
            set_flags: pdfium_sys::FPDFAnnot_SetFlags,
        })
    }
}

impl<'page, 'lib: 'page> TextPage<'page, 'lib> {
    /// Fill `buf` with per-character records starting at `start` using the
    /// fork's `FPDFText_GetCharInfoBatch` (chromium/8028+), replacing several
    /// FFI round-trips per character with one per chunk. Returns the number
    /// of records written (0 at end of range), or `None` when the loaded
    /// pdfium build predates the batch API — callers must fall back to the
    /// per-character getters.
    ///
    /// Record fields mirror the single-char getters exactly: raw page-space
    /// boxes, `char_type` as the raw `CPDF_TextPage::CharType` value
    /// (1 = generated, 2 = no-unicode-mapping), and `text_render_mode` of the
    /// char's text object (-1 when it has none).
    pub fn char_infos_batch(
        &self,
        start: i32,
        buf: &mut [pdfium_sys::FPDF_CHARINFO_LP],
    ) -> Option<usize> {
        #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
        let written = {
            let batch_fn =
                pdfium_sys::dynamic::pdfium().FPDFText_GetCharInfoBatch?;
            if buf.is_empty() {
                return Some(0);
            }
            unsafe {
                batch_fn(self.handle, start, buf.len() as i32, buf.as_mut_ptr())
            }
        };
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        if buf.is_empty() {
            return Some(0);
        }
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        let written = unsafe {
            pdfium_sys::FPDFText_GetCharInfoBatch(
                self.handle,
                start,
                buf.len() as i32,
                buf.as_mut_ptr(),
            )
        };
        Some(written.max(0) as usize)
    }
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
use std::sync::{Mutex, MutexGuard, OnceLock};
/// Process-global PDFium serialization lock.
///
/// PDFium's FFI is **not thread-safe**: concurrent calls (even across distinct
/// documents) corrupt internal state and cause heap UB (double-free / heap
/// corruption). Every [`Library`] handle holds this mutex for its entire
/// lifetime, and the owning PDFium resources ([`Document`], `Page`,
/// `TextPage`, `Bitmap`) borrow from a [`Library`] via their `'lib` lifetime,
/// so the borrow checker statically prevents PDFium work outside the lock.
/// `Font` is a borrowed, non-owning handle whose lifetime is tied to its text
/// object when obtained through the safe `TextChar::font()` API. Its raw
/// constructor remains unsafe for low-level callers that prove the lifetime.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
fn pdfium_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Library {
    /// Whether the loaded pdfium binary exports the fork's
    /// `FPDFPage_GetUserUnit`. When it does, `Document::page` reads
    /// `/UserUnit` through it and the byte-scan table is skipped entirely.
    pub(crate) fn user_unit_api_available() -> bool {
        #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
        {
            pdfium_sys::dynamic::pdfium().FPDFPage_GetUserUnit.is_some()
        }
        // Statically linked on wasm; a pinned release without the export
        // would fail at link time, so present-at-runtime is guaranteed.
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        {
            true
        }
    }
}
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub(crate) type LibraryGuard = MutexGuard<'static, ()>;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub(crate) struct LibraryGuard;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
/// Loads native symbols and acquires the process-wide PDFium serialization lock.
pub(crate) fn library_guard() -> Result<LibraryGuard, crate::PdfiumError> {
    pdfium_sys::dynamic::load_default()
        .map_err(|_| crate::PdfiumError::OperationFailed)?;
    Ok(pdfium_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
/// Uses the single-Worker PDFium instance without native locking.
pub(crate) fn library_guard() -> Result<LibraryGuard, crate::PdfiumError> {
    Ok(LibraryGuard)
}
