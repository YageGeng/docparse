use std::sync::Arc;

use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_layout::timing::{TimingStage, Timings};

use super::{
    PdfInput, PdfiumExecutor, PdfiumRuntimeError, PreScannedPage, RenderedPage,
};
use crate::{GlyphResolver, WasmBoxedFuture, WasmCompatSend, WasmCompatSync};

/// Opens document sessions while leaving thread or process ownership to the provider.
pub trait PdfiumProvider: WasmCompatSend + WasmCompatSync {
    /// Opens a document and attributes provider queueing separately from actual PDF opening.
    fn open<'a>(
        &'a self,
        input: PdfInput,
        limits: &'a RuntimeConfig,
        timings: Timings,
    ) -> WasmBoxedFuture<'a, Result<Box<dyn PdfiumSession>, PdfiumRuntimeError>>;
}

/// A document-affine session exposing owned facts without leaking PDFium handles.
pub trait PdfiumSession: WasmCompatSend + WasmCompatSync {
    /// Returns the positive page count obtained while opening the document.
    fn page_count(&self) -> u32;
    /// Extracts one page and optionally resolves unmapped glyph outlines in the caller.
    fn pre_scan_page(
        &self,
        page_number: u32,
        resolver: Option<Arc<dyn GlyphResolver>>,
    ) -> WasmBoxedFuture<'_, Result<PreScannedPage, PdfiumRuntimeError>>;
    /// Renders one page into owned pixels and validated coordinate transforms.
    fn render_page<'a>(
        &'a self,
        page_number: u32,
        config: &'a RenderConfig,
    ) -> WasmBoxedFuture<'a, Result<RenderedPage, PdfiumRuntimeError>>;
    /// Releases all document resources before allowing its execution slot to be reused.
    fn close(
        self: Box<Self>,
    ) -> WasmBoxedFuture<'static, Result<(), PdfiumRuntimeError>>;
}

/// Default native-thread or browser-local provider; no worker executable is required.
#[derive(Debug, Default)]
pub struct LocalPdfiumProvider;

impl PdfiumProvider for LocalPdfiumProvider {
    /// Keeps the existing local executor and accounts only its opening work.
    fn open<'a>(
        &'a self,
        input: PdfInput,
        limits: &'a RuntimeConfig,
        timings: Timings,
    ) -> WasmBoxedFuture<'a, Result<Box<dyn PdfiumSession>, PdfiumRuntimeError>>
    {
        Box::pin(async move {
            let _opening = timings.start(TimingStage::PdfOpen);
            let executor = PdfiumExecutor::open(input, limits).await?;
            Ok(Box::new(executor) as Box<dyn PdfiumSession>)
        })
    }
}

impl PdfiumSession for PdfiumExecutor {
    /// Returns the local executor's immutable document page count.
    fn page_count(&self) -> u32 {
        self.page_count()
    }
    /// Delegates extraction to the existing document-owning worker.
    fn pre_scan_page(
        &self,
        page_number: u32,
        resolver: Option<Arc<dyn GlyphResolver>>,
    ) -> WasmBoxedFuture<'_, Result<PreScannedPage, PdfiumRuntimeError>> {
        Box::pin(self.pre_scan_page(page_number, resolver))
    }
    /// Delegates rasterization without moving PDFium handles across the boundary.
    fn render_page<'a>(
        &'a self,
        page_number: u32,
        config: &'a RenderConfig,
    ) -> WasmBoxedFuture<'a, Result<RenderedPage, PdfiumRuntimeError>> {
        Box::pin(self.render_page(page_number, config))
    }
    /// Joins the local worker before reporting the session closed.
    fn close(
        self: Box<Self>,
    ) -> WasmBoxedFuture<'static, Result<(), PdfiumRuntimeError>> {
        Box::pin((*self).close())
    }
}
