use docparse_common::timing::Timings;
use docparse_config::{
    RawConfig, RenderConfig, RuntimeConfig, ValidatedConfig,
};
use docparse_core::{
    DocParser, GlyphResolver, LocalPdfiumProvider, PdfInput, PdfiumProvider,
    PdfiumRuntimeError, PdfiumSession, PreScannedPage, RenderedPage,
    WasmBoxedFuture,
};
use docparse_layout::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Notify, Semaphore};

/// A real native session fails at Close only after downstream inference has entered its gate.
struct ClosingSession {
    inner: Box<dyn PdfiumSession>,
    entered: Arc<Notify>,
    panic: bool,
}
impl PdfiumSession for ClosingSession {
    /// Preserves the actual PDF page count.
    fn page_count(&self) -> u32 {
        self.inner.page_count()
    }
    /// Uses real native text extraction for the regression.
    fn pre_scan_page(
        &self,
        page: u32,
        resolver: Option<Arc<dyn GlyphResolver>>,
    ) -> WasmBoxedFuture<'_, Result<PreScannedPage, PdfiumRuntimeError>> {
        self.inner.pre_scan_page(page, resolver)
    }
    /// Uses real PDFium rasterization before the injected failure.
    fn render_page<'a>(
        &'a self,
        page: u32,
        config: &'a RenderConfig,
    ) -> WasmBoxedFuture<'a, Result<RenderedPage, PdfiumRuntimeError>> {
        self.inner.render_page(page, config)
    }
    /// Fails only after native cleanup and after downstream inference has entered its gate.
    #[allow(
        clippy::panic,
        reason = "the regression deliberately exercises producer task panics"
    )]
    fn close(
        self: Box<Self>,
    ) -> WasmBoxedFuture<'static, Result<(), PdfiumRuntimeError>> {
        Box::pin(async move {
            self.inner.close().await?;
            self.entered.notified().await;
            if self.panic {
                panic!("injected producer panic");
            }
            Err(PdfiumRuntimeError::Transport(
                "injected fatal close failure".into(),
            ))
        })
    }
}
struct GatedLayout {
    entered: Arc<Notify>,
    release: Arc<Semaphore>,
}
impl LayoutEngine for GatedLayout {
    /// Identifies the downstream scheduling gate independently of model artifacts.
    fn name(&self) -> &str {
        "producer-failure-gate"
    }
    /// Retains stable metadata for repeated parses.
    fn model_revision(&self) -> &str {
        "1"
    }
    /// Keeps the page unfinished until cancellation or an explicit release.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async {
            self.entered.notify_one();
            self.release.acquire().await.expect("open gate").forget();
            Ok(Vec::new())
        })
    }
}

/// Both fatal close errors and producer panics must cancel pending pages and leave the shared queue reusable.
#[tokio::test]
async fn producer_failures_interrupt_pages_and_release_capacity() {
    for panic in [false, true] {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let mut raw = RawConfig::default();
        raw.formula.inline_enabled = false;
        raw.formula.display_enabled = false;
        raw.tsr.mode = docparse_config::TableMode::RulesOnly;
        raw.render.queue_size = 1;
        let parser = DocParser::builder()
            .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
            .layout_engine(Arc::new(GatedLayout {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }))
            .build()
            .await
            .expect("parser");
        let bytes: Arc<[u8]> = Arc::from(
            include_bytes!("fixtures/pdf/extraction_metadata.pdf").as_slice(),
        );
        let session = LocalPdfiumProvider
            .open(
                PdfInput::Bytes(Arc::clone(&bytes)),
                &RuntimeConfig::default(),
                Timings::default(),
            )
            .await
            .expect("real document session");
        let session = Box::new(ClosingSession {
            inner: session,
            entered,
            panic,
        }) as Box<dyn PdfiumSession>;
        let failed = tokio::time::timeout(
            Duration::from_secs(2),
            parser.parse_session_with_options(
                session,
                docparse_core::ParseOptions::default(),
            ),
        )
        .await;
        // Always unblock native work before assertions, including when the old implementation times out.
        release.add_permits(2);
        let recovered = tokio::time::timeout(
            Duration::from_secs(2),
            parser.parse_bytes(bytes),
        )
        .await;
        let error = failed.expect("producer failure must be observed while the model is still pending").expect_err("fatal producer failure");
        let expected = if panic {
            "injected producer panic"
        } else {
            "injected fatal close failure"
        };
        assert!(
            error.to_string().contains(expected),
            "original failure was lost: {error}"
        );
        assert_eq!(
            recovered
                .expect("shared queue must recover")
                .expect("second document")
                .pages
                .len(),
            1
        );
    }
}
