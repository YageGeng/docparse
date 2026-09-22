//! Native and browser runtime boundaries with explicitly scoped compatibility submodules.
// Runtime mechanics are shared directly rather than routed through a model crate.
pub(crate) use docparse_common::{TaskSet, spawn, timeout};

pub use crate::pdfium::PdfInput;

pub use docparse_common::{
    TaskError, WasmBoxedFuture, WasmCompatSend, WasmCompatSync,
};

#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!("docparse Web builds require the wasm feature");
#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
compile_error!("docparse supports wasm32-unknown-unknown browser builds only");

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod native;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use native::{
    FigureAssets, write_pdf_overlays, write_pdf_overlays_for_pages,
};

// The outline database is a native adapter; browser callers can inject an in-memory resolver.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod font_db;
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub use font_db::FontDbResolver;

/// Reads the optional native font database setting once when the parser is assembled.
#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
pub(crate) fn default_glyph_resolver()
-> Option<std::sync::Arc<dyn crate::GlyphResolver>> {
    let path = std::env::var_os("DOCPARSE_FONT_DB_DIR")
        .filter(|path| !path.is_empty())?;
    Some(std::sync::Arc::new(FontDbResolver::new(path)))
}

/// Browser recovery is supplied explicitly and never consults a filesystem or environment.
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub(crate) fn default_glyph_resolver()
-> Option<std::sync::Arc<dyn crate::GlyphResolver>> {
    None
}

#[cfg(all(feature = "pdfium-ipc", not(target_arch = "wasm32")))]
pub use crate::pdfium::ipc as pdfium_ipc;

/// Browser image delivery is inline and owns no filesystem resources.
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub struct FigureAssets {
    pub(crate) config: docparse_config::FigureConfig,
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl FigureAssets {
    /// Preserves delivery validation without consulting a browser filesystem.
    pub(crate) fn new(
        config: docparse_config::FigureConfig,
        _prefix: String,
    ) -> Self {
        Self { config }
    }

    /// Inline results have no pending directory to publish or clean.
    pub fn keep(&self, _committed: bool) {}

    /// Reports unsupported file delivery without manufacturing browser-local paths.
    pub(crate) fn write(
        &self,
        _block_id: &str,
        _media_type: crate::FigureMediaType,
        _bytes: &[u8],
    ) -> Result<String, String> {
        Err("file figure delivery is unavailable in the browser".to_owned())
    }
}
