use std::collections::BTreeMap;
use std::sync::Arc;

use super::stages::PageAnalysisInput;
use crate::wasm_compat::{TaskError, TaskSet};
use docparse_common::timing::{TimingStage, Timings};
use docparse_config::ValidatedConfig;
use docparse_layout::LayoutEngine;
use tokio::sync::mpsc;
use typed_builder::TypedBuilder;

use super::{PdfInput, PdfiumRuntimeError, PreScannedPage};
use crate::PdfiumSession;
use crate::page::{OcrCompletion, PageAnalyzer};
use crate::{
    DocumentContext, DocumentContextBuilder, DocumentLinker, DocumentResult,
    ExtractedPage, OcrEngine, PageAnalysisError, PageError, PageResult,
    PageWarning, ResultValidator, SchemaVersion, ValidationError,
};

/// Failures that cannot be represented as a safe degraded document result.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ParseRuntimeError {
    /// The serialized PDFium actor failed at document or page scope.
    #[error(transparent)]
    Pdfium(#[from] PdfiumRuntimeError),
    /// External table configuration is checked before the document opens.
    #[error(transparent)]
    Table(#[from] crate::TableStructureError),
    /// Frozen document statistics could not be built from pre-scan facts.
    #[error(transparent)]
    Context(#[from] crate::ContextError),
    /// One pure page pipeline violated an internal or geometry invariant.
    #[error(transparent)]
    Page(#[from] PageAnalysisError),
    /// A page or producer task panicked or was cancelled.
    #[error("runtime task failed: {0}")]
    Task(String),
    /// A rendered page could not be paired with its pre-scanned facts.
    #[error("missing pre-scanned facts for page {page_number}")]
    MissingExtractedPage { page_number: u32 },
    /// A rendered actor response does not match its extracted page identity.
    #[error("rendered page {actual} does not match extracted page {expected}")]
    RenderedPageMismatch { expected: u32, actual: u32 },
    /// Final document validation detected an internal invariant violation.
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

/// Owned scan results shared by page scheduling and deterministic document finalization.
#[derive(TypedBuilder)]
struct ScannedDocument {
    context: Arc<DocumentContext>,
    extracted_pages: BTreeMap<u32, ExtractedPage>,
    pre_scan_warnings: BTreeMap<u32, PageWarning>,
    page_errors: Vec<PageError>,
}

impl ScannedDocument {
    /// Orders completed pages, merges scan diagnostics, and links and validates the final document.
    fn finish(
        self,
        mut pages: Vec<PageResult>,
        timings: &Timings,
        observer: Option<&dyn crate::ParseObserver>,
    ) -> Result<DocumentResult, ParseRuntimeError> {
        let Self {
            context,
            mut pre_scan_warnings,
            mut page_errors,
            ..
        } = self;
        let page_count = context.page_count;
        pages.sort_by_key(|page| page.page_number);
        for page in &mut pages {
            if let Some(warning) = pre_scan_warnings.remove(&page.page_number) {
                page.warnings.push(warning);
                page.warnings.sort_by(|left, right| {
                    left.stage
                        .cmp(&right.stage)
                        .then_with(|| left.code.cmp(&right.code))
                        .then_with(|| left.message.cmp(&right.message))
                });
            }
        }
        page_errors.sort_by_key(|error| error.page_number);

        if let Some(observer) = observer {
            observer.on_progress(crate::ParseProgress::Linking {
                total: page_count,
            });
        }

        let linking = timings.start(TimingStage::LinkValidate);
        let relations = DocumentLinker::new().link(&context, &pages)?;
        let result = DocumentResult::builder()
            .schema_version(SchemaVersion::V2_0)
            .context((*context).clone())
            .pages(pages)
            .relations(relations)
            .errors(page_errors)
            .build();
        ResultValidator::validate(&result)?;
        drop(linking);
        Ok(result)
    }
}

/// Document pipeline with immutable injected engines and bounded runtime settings.
#[derive(Clone, TypedBuilder)]
pub(crate) struct ParseRuntime {
    render_queue: docparse_common::PageQueue,
    #[builder(default = Arc::new(crate::LocalPdfiumProvider))]
    pdfium_provider: Arc<dyn crate::PdfiumProvider>,
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    #[builder(default)]
    table_engine: Option<Arc<dyn crate::TableStructureEngine>>,
    #[builder(default)]
    formula_engine: Option<Arc<dyn docparse_formula::FormulaEngine>>,
    #[builder(default)]
    glyph_resolver: Option<Arc<dyn crate::GlyphResolver>>,
}

/// A reserved/open session avoids reacquiring the only PDFium worker during server dispatch.
pub(crate) enum DocumentSource {
    Input(PdfInput),
    Session(Box<dyn PdfiumSession>),
}
impl From<PdfInput> for DocumentSource {
    /// Keeps existing library input APIs on the provider-owned opening path.
    fn from(input: PdfInput) -> Self {
        Self::Input(input)
    }
}
impl From<Box<dyn PdfiumSession>> for DocumentSource {
    /// Transfers an already-open document without asking the pool for another process.
    fn from(session: Box<dyn PdfiumSession>) -> Self {
        Self::Session(session)
    }
}

impl ParseRuntime {
    /// Orchestrates scanning, bounded page analysis, and finalization with one per-call table runtime.
    pub(crate) async fn parse_document_with_options(
        &self,
        input: impl Into<DocumentSource> + crate::WasmCompatSend,
        options: crate::ParseOptions<'_>,
    ) -> Result<DocumentResult, ParseRuntimeError> {
        let observer = options.observer;
        let tables =
            options.table_runtime(&self.config, self.table_engine.as_ref())?;
        let (collector, mut timing_receiver) = Timings::channel();
        let timings = if observer.is_some() {
            collector
        } else {
            Timings::default()
        };
        let total_timer = timings.start(TimingStage::ParseTotal);
        if let Some(observer) = observer {
            observer.on_progress(crate::ParseProgress::Opening);
        }
        tracing::info!(
            "starting document parse with layout engine {}",
            self.layout_engine.name()
        );
        let executor = match input.into() {
            DocumentSource::Input(input) => {
                self.pdfium_provider
                    .open(input, self.config.runtime(), timings.clone())
                    .await?
            }
            DocumentSource::Session(session) => session,
        };
        let page_count = executor.page_count();
        // Scan errors share one shutdown boundary; the page driver takes ownership only after scanning succeeds.
        let mut scanned = match self
            .scan_document(
                executor.as_ref(),
                &timings,
                &mut timing_receiver,
                observer,
            )
            .await
        {
            Ok(scanned) => scanned,
            Err(error) => {
                let _ = executor.close().await;
                return Err(error);
            }
        };
        let pages = self
            .analyze_pages(
                executor,
                &mut scanned,
                tables,
                &timings,
                &mut timing_receiver,
                observer,
            )
            .await?;
        let result = scanned.finish(pages, &timings, observer)?;
        drop(total_timer);
        while let Ok(timing) = timing_receiver.try_recv() {
            if let Some(observer) = observer {
                observer.on_timing(timing);
            }
        }

        if let Some(observer) = observer {
            observer.on_progress(crate::ParseProgress::Complete {
                total: page_count,
            });
        }

        tracing::info!(
            "completed document parse with {} pages and {} page errors",
            result.pages.len(),
            result.errors.len()
        );
        Ok(result)
    }

    /// Extracts page facts and freezes document-wide statistics without owning the executor's shutdown policy.
    async fn scan_document(
        &self,
        executor: &dyn PdfiumSession,
        timings: &Timings,
        timing_receiver: &mut mpsc::UnboundedReceiver<crate::Timing>,
        observer: Option<&dyn crate::ParseObserver>,
    ) -> Result<ScannedDocument, ParseRuntimeError> {
        let page_count = executor.page_count();
        if let Some(observer) = observer {
            observer.on_progress(crate::ParseProgress::Scanning {
                completed: 0,
                total: page_count,
            });
        }
        let mut extracted_pages = BTreeMap::new();
        let mut pre_scan_warnings = BTreeMap::new();
        let mut page_errors = Vec::new();
        let mut context_builder = DocumentContextBuilder::builder()
            .page_count(page_count)
            .metadata(BTreeMap::from([(
                "layout_engine".to_owned(),
                self.layout_engine.name().to_owned(),
            )]))
            .model_revision(Some(
                self.layout_engine.model_revision().to_owned(),
            ))
            .build();
        for page_number in 1..=page_count {
            tracing::debug!("pre-scanning page {}", page_number);
            let extraction = timings
                .for_page(page_number)
                .start(TimingStage::TextExtract);
            // Preserve the parser's recovery policy across every page handled by the PDFium worker.
            let outcome = executor
                .pre_scan_page(
                    page_number,
                    self.glyph_resolver.as_ref().map(Arc::clone),
                )
                .await;
            drop(extraction);
            while let Ok(timing) = timing_receiver.try_recv() {
                if let Some(observer) = observer {
                    observer.on_timing(timing);
                }
            }
            match outcome {
                Ok(outcome) => {
                    let (extracted, warning, page_error) =
                        match Self::recover_pre_scan(
                            outcome,
                            self.config.runtime().continue_on_error,
                        ) {
                            Ok(recovered) => recovered,
                            Err(error) => {
                                tracing::error!(
                                    "native extraction failed for page {}: {}",
                                    page_number,
                                    error
                                );
                                return Err(ParseRuntimeError::Pdfium(error));
                            }
                        };
                    extracted_pages.insert(page_number, extracted);
                    if let Some(warning) = warning {
                        pre_scan_warnings.insert(page_number, warning);
                    }
                    if let Some(page_error) = page_error {
                        page_errors.push(page_error);
                    }
                    if let Some(observer) = observer {
                        observer.on_progress(crate::ParseProgress::Scanning {
                            completed: page_number,
                            total: page_count,
                        });
                    }
                }
                Err(error) => {
                    tracing::error!(
                        "pre-scan failed for page {}: {}",
                        page_number,
                        error
                    );
                    return Err(ParseRuntimeError::Pdfium(error));
                }
            }
        }
        let context_timer = timings.start(TimingStage::DocumentContext);
        // Freeze watermark decisions before body-font/chrome statistics or any page fusion.
        if let Err(error) = crate::watermark::classify(
            extracted_pages.values_mut(),
            self.config.fusion(),
        ) {
            tracing::error!("watermark classification failed: {}", error);
            return Err(PageAnalysisError::Line(error).into());
        }
        for extracted in extracted_pages.values() {
            if let Err(error) =
                context_builder.push_page(crate::PageProbe::from(extracted))
            {
                tracing::error!(
                    "document context rejected page {}: {}",
                    extracted.page_number,
                    error
                );
                return Err(error.into());
            }
        }
        let context = match context_builder.build() {
            Ok(context) => context,
            Err(error) => {
                tracing::error!(
                    "document context construction failed: {}",
                    error
                );
                return Err(error.into());
            }
        };
        if let Some(observer) = observer {
            observer.on_progress(crate::ParseProgress::Analyzing {
                completed: 0,
                total: page_count,
            });
        }
        drop(context_timer);
        Ok(ScannedDocument::builder()
            .context(context)
            .extracted_pages(extracted_pages)
            .pre_scan_warnings(pre_scan_warnings)
            .page_errors(page_errors)
            .build())
    }

    /// Drives bounded page stages and owns render-producer shutdown and fatal-task cancellation.
    async fn analyze_pages(
        &self,
        executor: Box<dyn PdfiumSession>,
        scanned: &mut ScannedDocument,
        tables: Arc<super::TableRuntime>,
        timings: &Timings,
        timing_receiver: &mut mpsc::UnboundedReceiver<crate::Timing>,
        observer: Option<&dyn crate::ParseObserver>,
    ) -> Result<Vec<PageResult>, ParseRuntimeError> {
        let page_count = executor.page_count();
        // This channel only hands off results; shared completion-counted slots own the actual capacity.
        let (render_sender, mut render_receiver) = mpsc::channel(1);
        let render_config = self.config.render().clone();
        let render_timings = timings.clone();
        let render_queue = self.render_queue.clone();
        let mut producer = crate::wasm_compat::spawn(async move {
            for page_number in 1..=page_count {
                let lease = render_queue.reserve().await.map_err(|error| {
                    PdfiumRuntimeError::Transport(error.to_string())
                })?;
                let timer = render_timings
                    .for_page(page_number)
                    .start(TimingStage::PdfRender);
                let rendered = lease
                    .scope(executor.render_page(page_number, &render_config))
                    .await;
                drop(timer);
                if render_sender
                    .send((page_number, rendered, lease))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Free PDFium immediately after final delivery, independently of remaining model work.
            executor.close().await
        });
        let mut tasks = TaskSet::new();
        let mut pages = Vec::with_capacity(page_count as usize);
        let mut receiver_open = true;
        let mut producer_finished = false;
        let mut fatal_error = None;
        while receiver_open || !tasks.is_empty() || !producer_finished {
            tokio::select! {
                outcome = &mut producer, if !producer_finished => {
                    // A failed render owner must cancel pending page work immediately; successful Close only finishes production.
                    producer_finished = true;
                    let result = outcome.map_err(|error| ParseRuntimeError::Task(error.to_string()))
                        .and_then(|result| result.map_err(ParseRuntimeError::Pdfium));
                    if let Err(error) = result {
                        tracing::error!("render producer failed: {}", error);
                        fatal_error = Some(error);
                        break;
                    }
                }
                Some(timing) = timing_receiver.recv(), if observer.is_some() => {
                    if let Some(observer) = observer { observer.on_timing(timing); }
                }
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = result {
                        // A completed result keeps its slot until this collection boundary.
                        match Self::collect_task(result.map(|(outcome, _lease)| outcome)) {
                            Ok(page) => {
                                pages.push(page);
                                if let Some(observer) = observer {
                                    observer.on_progress(crate::ParseProgress::Analyzing { completed: pages.len() as u32, total: page_count });
                                }
                            }
                            Err(error) => { fatal_error = Some(error); break; }
                        }
                    }
                }
                rendered = render_receiver.recv(), if receiver_open => {
                    let Some((page_number, rendered, lease)) = rendered else { receiver_open = false; continue; };
                    let Some(extracted) = scanned.extracted_pages.remove(&page_number) else {
                        fatal_error = Some(ParseRuntimeError::MissingExtractedPage { page_number });
                        break;
                    };
                    let timings = timings.for_page(page_number);
                    let config = Arc::clone(&self.config);
                    let layout_engine = Arc::clone(&self.layout_engine);
                    let ocr_engine = self.ocr_engine.as_ref().map(Arc::clone);
                    let formula_engine = self.formula_engine.as_ref().map(Arc::clone);
                    let tables = Arc::clone(&tables);
                    let context = Arc::clone(&scanned.context);
                    let rendered = rendered.map(|mut rendered| {
                        Arc::make_mut(&mut rendered.image).retain_page(lease.clone());
                        if let Some(observer) = observer { observer.on_page_image(page_number, rendered.image.as_ref()); }
                        rendered
                    });
                    tasks.spawn(async move {
                        let outcome = lease.scope(async move {
                            match rendered {
                                Ok(rendered) => super::analyze_rendered_page(PageAnalysisInput::builder()
                                    .config(config).layout_engine(layout_engine).ocr_engine(ocr_engine).formula_engine(formula_engine)
                                    .context(context).extracted(extracted).rendered(rendered).timings(timings).tables(tables).build()).await,
                                Err(error) if config.runtime().continue_on_error && !error.is_fatal() => {
                                    tracing::warn!("render failed for page {}, using native fallback: {}", page_number, error);
                                    docparse_common::run_cpu(move || analyze_without_render(config, context, extracted, error, timings))
                                        .await.map_err(|error| ParseRuntimeError::Task(error.to_string()))?
                                }
                                Err(error) => Err(ParseRuntimeError::Pdfium(error)),
                            }
                        }).await;
                        (outcome, lease)
                    });
                }
            }
        }
        if let Some(error) = fatal_error {
            // Stop a producer waiting for capacity before draining owned page tasks; background work keeps its own lease.
            render_receiver.close();
            drop(producer);
            while render_receiver.try_recv().is_ok() {}
            drop(render_receiver);
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            return Err(error);
        }
        Ok(pages)
    }

    /// Applies configured page-error continuation to one page-shell outcome.
    fn recover_pre_scan(
        outcome: PreScannedPage,
        continue_on_error: bool,
    ) -> Result<
        (ExtractedPage, Option<PageWarning>, Option<PageError>),
        PdfiumRuntimeError,
    > {
        let PreScannedPage {
            extracted,
            extraction_error,
        } = outcome;
        let Some(error) = extraction_error else {
            return Ok((extracted, None, None));
        };
        if !continue_on_error {
            return Err(error);
        }
        let message = error.to_string();
        let warning = PageWarning {
            code: "NativeExtractionUnavailable".to_owned(),
            stage: "extract".to_owned(),
            message: message.clone(),
        };
        let page_error = PageError::builder()
            .page_number(extracted.page_number)
            .stage("extract".to_owned())
            .code("NativeExtractionUnavailable".to_owned())
            .message(message)
            .build();
        Ok((extracted, Some(warning), Some(page_error)))
    }

    /// Converts any joined stage outcome while preserving fatal errors across stage boundaries.
    fn collect_task<T>(
        result: Result<Result<T, ParseRuntimeError>, TaskError>,
    ) -> Result<T, ParseRuntimeError> {
        match result {
            Ok(Ok(page)) => Ok(page),
            Ok(Err(error)) => {
                tracing::error!("page analysis failed: {}", error);
                Err(error)
            }
            Err(error) => {
                tracing::error!("page task failed: {}", error);
                Err(ParseRuntimeError::Task(error.to_string()))
            }
        }
    }
}

/// Produces a native-only page when rendering fails but continuation is enabled.
fn analyze_without_render(
    config: Arc<ValidatedConfig>,
    context: Arc<DocumentContext>,
    extracted: ExtractedPage,
    error: PdfiumRuntimeError,
    timings: Timings,
) -> Result<PageResult, ParseRuntimeError> {
    let analyzer = PageAnalyzer::new(config).with_timings(timings.clone());
    let preparation = timings.start(TimingStage::TextPrepare);
    let draft = analyzer.prepare(extracted, Vec::new(), context)?;
    drop(preparation);
    let _finishing = timings.start(TimingStage::TextFinish);
    let mut page = analyzer.finish(draft, OcrCompletion::Unavailable)?;
    page.warnings.push(PageWarning {
        code: "RenderUnavailable".to_owned(),
        stage: "render".to_owned(),
        message: error.to_string(),
    });
    page.warnings.sort_by(|left, right| {
        left.stage
            .cmp(&right.stage)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    Ok(page)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use docparse_config::{RawConfig, ValidatedConfig};
    use docparse_layout::{
        LayoutDetection, LayoutEngine, LayoutError, LayoutRequest,
    };

    use super::ParseRuntime;
    use crate::runtime::{PdfInput, PdfiumRuntimeError, PreScannedPage};
    use crate::{ExtractError, ExtractedPage};

    /// Empty deterministic engine used to exercise pure-geometry fallback.
    struct EmptyLayoutEngine;

    impl LayoutEngine for EmptyLayoutEngine {
        /// Returns one stable fake engine name.
        fn name(&self) -> &str {
            "empty-layout"
        }

        /// Returns one stable fake model revision.
        fn model_revision(&self) -> &str {
            "test-revision"
        }

        /// Returns no detections without reading external model artifacts.
        fn detect(
            &self,
            _request: LayoutRequest,
        ) -> docparse_common::WasmBoxedFuture<
            '_,
            Result<Vec<LayoutDetection>, LayoutError>,
        > {
            Box::pin(async move { Ok(Vec::new()) })
        }
    }

    /// Panicking engine records how many pages reached a failed task boundary.
    struct PanickingLayoutEngine {
        calls: Arc<AtomicUsize>,
    }

    impl LayoutEngine for PanickingLayoutEngine {
        /// Returns one stable fake engine name.
        fn name(&self) -> &str {
            "failing-layout"
        }

        /// Returns one stable fake model revision.
        fn model_revision(&self) -> &str {
            "test-revision"
        }

        /// Records the page and triggers a deterministic task-join failure.
        #[allow(clippy::panic)]
        fn detect(
            &self,
            _request: LayoutRequest,
        ) -> docparse_common::WasmBoxedFuture<
            '_,
            Result<Vec<LayoutDetection>, LayoutError>,
        > {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                panic!("intentional test task failure")
            })
        }
    }

    /// Resolves the deterministic extraction fixture path.
    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf/extraction_metadata.pdf")
    }

    /// Builds valid runtime configuration without requiring model files.
    fn config() -> Arc<ValidatedConfig> {
        let mut raw = RawConfig::default();
        raw.tsr.mode = docparse_config::TableMode::RulesOnly;
        raw.layout.model_path = PathBuf::from("/tmp/missing-model.onnx");
        raw.layout.model_config_path = PathBuf::from("/tmp/missing-model.yml");
        raw.layout.model_manifest_path =
            PathBuf::from("/tmp/missing-model.json");
        Arc::new(
            ValidatedConfig::try_from(raw).expect("test config must validate"),
        )
    }

    /// Verifies the complete actor-to-analyzer path produces a valid document.
    #[tokio::test]
    async fn runtime_parses_document_with_injected_layout_engine() {
        let runtime = ParseRuntime::builder()
            .render_queue(docparse_common::PageQueue::new(16))
            .config(config())
            .layout_engine(Arc::new(EmptyLayoutEngine) as Arc<dyn LayoutEngine>)
            .build();

        let result = runtime
            .parse_document_with_options(
                PdfInput::Path(fixture_path()),
                crate::ParseOptions::default(),
            )
            .await
            .expect("fixture parse must succeed");

        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.context.page_count, 1);
        assert_eq!(
            result.context.model_revision.as_deref(),
            Some("test-revision")
        );
        let page = result.pages.first().expect("one page must exist");
        assert!(!page.blocks.is_empty());
        assert!(
            !page
                .iter_text_items()
                .next()
                .expect("text must survive")
                .raw_text
                .is_empty()
        );
        crate::ResultValidator::validate(&result)
            .expect("runtime result must validate");
    }

    /// Verifies the first fatal page failure prevents later layout work.
    #[tokio::test]
    async fn runtime_stops_scheduling_after_first_fatal_page() {
        let mut raw = RawConfig::default();
        raw.tsr.mode = docparse_config::TableMode::RulesOnly;
        raw.layout.model_path = PathBuf::from("/tmp/missing-model.onnx");
        raw.layout.model_config_path = PathBuf::from("/tmp/missing-model.yml");
        raw.layout.model_manifest_path =
            PathBuf::from("/tmp/missing-model.json");
        raw.render.queue_size = 1;
        let config = Arc::new(
            ValidatedConfig::try_from(raw).expect("test config must validate"),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = ParseRuntime::builder()
            .render_queue(docparse_common::PageQueue::new(
                config.render().queue_size,
            ))
            .config(config)
            .layout_engine(Arc::new(PanickingLayoutEngine {
                calls: Arc::clone(&calls),
            }) as Arc<dyn LayoutEngine>)
            .build();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf/multipage_layout.pdf");

        let _error = runtime
            .parse_document_with_options(
                PdfInput::Path(path),
                crate::ParseOptions::default(),
            )
            .await
            .expect_err("the injected layout error must fail parsing");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Verifies text extraction failures become page diagnostics only when continuation is enabled.
    #[test]
    fn pre_scan_failure_policy_preserves_page_shell_conditionally() {
        let outcome = PreScannedPage {
            extracted: ExtractedPage::builder()
                .page_number(2)
                .width(100.0)
                .height(200.0)
                .rotation(0)
                .text_items(Vec::new())
                .build(),
            extraction_error: Some(PdfiumRuntimeError::Extraction {
                page_number: 2,
                source: ExtractError::MissingCharacterGeometry { index: 4 },
            }),
        };

        let (page, warning, error) =
            ParseRuntime::recover_pre_scan(outcome, true)
                .expect("continuation must preserve the page shell");

        assert_eq!(page.page_number, 2);
        assert_eq!(
            warning.expect("warning must exist").code,
            "NativeExtractionUnavailable"
        );
        assert_eq!(error.expect("page error must exist").page_number, 2);

        let fatal = PreScannedPage {
            extracted: page,
            extraction_error: Some(PdfiumRuntimeError::Extraction {
                page_number: 2,
                source: ExtractError::MissingCharacterGeometry { index: 4 },
            }),
        };
        let _error = ParseRuntime::recover_pre_scan(fatal, false)
            .expect_err("disabled continuation must return the source error");
    }
}
