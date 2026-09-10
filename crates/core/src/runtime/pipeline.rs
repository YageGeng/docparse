use std::collections::BTreeMap;
use std::sync::Arc;

use super::TableRuntime;
use crate::wasm_compat::{TaskError, TaskSet};
use docparse_config::{OcrPolicy, ValidatedConfig};
use docparse_layout::timing::{TimingStage, Timings};
use docparse_layout::{LayoutEngine, LayoutRequest};
use tokio::sync::mpsc;
use typed_builder::TypedBuilder;

use super::{
    PdfInput, PdfiumExecutor, PdfiumRuntimeError, PreScannedPage, RenderedPage,
};
use crate::page::{OcrCompletion, PageAnalyzer};
use crate::{
    DocumentContext, DocumentContextBuilder, DocumentLinker, DocumentResult,
    ExtractedPage, OcrEngine, OcrRequest, PageAnalysisError, PageError,
    PageResult, PageWarning, ResultValidator, SchemaVersion, ValidationError,
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

/// Document pipeline with immutable injected engines and bounded runtime settings.
#[derive(Clone)]
pub(crate) struct ParseRuntime {
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    ocr_engine: Option<Arc<dyn OcrEngine>>,
}

impl ParseRuntime {
    /// Creates a runtime without touching default model artifacts.
    pub(crate) const fn new(
        config: Arc<ValidatedConfig>,
        layout_engine: Arc<dyn LayoutEngine>,
        ocr_engine: Option<Arc<dyn OcrEngine>>,
    ) -> Self {
        Self {
            config,
            layout_engine,
            ocr_engine,
        }
    }

    /// Keeps external table state confined to one document invocation.
    pub(crate) async fn parse_document_with_options(
        &self,
        input: PdfInput,
        options: crate::ParseOptions<'_>,
    ) -> Result<DocumentResult, ParseRuntimeError> {
        let observer = options.observer;
        let tables = TableRuntime::shared(options.table, options.table_engine)?;
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
        let opening = timings.start(TimingStage::PdfOpen);
        let executor =
            PdfiumExecutor::open(input, self.config.runtime()).await?;
        drop(opening);
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
            let outcome = executor.pre_scan_page(page_number).await;
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
                            self.config.runtime().continue_on_page_error,
                        ) {
                            Ok(recovered) => recovered,
                            Err(error) => {
                                tracing::error!(
                                    "native extraction failed for page {}: {}",
                                    page_number,
                                    error
                                );
                                let _ = executor.close().await;
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
                    let _ = executor.close().await;
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
            let _ = executor.close().await;
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
                let _ = executor.close().await;
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
                let _ = executor.close().await;
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
        let (render_sender, mut render_receiver) =
            mpsc::channel(self.config.runtime().render_queue_capacity);
        let render_config = self.config.render().clone();
        let render_timings = timings.clone();
        let producer = crate::wasm_compat::spawn(async move {
            for page_number in 1..=page_count {
                let timer = render_timings
                    .for_page(page_number)
                    .start(TimingStage::PdfRender);
                let rendered =
                    executor.render_page(page_number, &render_config).await;
                drop(timer);
                if render_sender.send((page_number, rendered)).await.is_err() {
                    break;
                }
            }
            executor
        });

        let mut page_tasks = TaskSet::new();
        let mut pages = Vec::with_capacity(page_count as usize);
        let mut fatal_error = None;
        let mut receiver_open = true;
        'processing: while receiver_open || !page_tasks.is_empty() {
            // Deliver observations on this future, never from native tasks or the ORT actor.
            while let Ok(timing) = timing_receiver.try_recv() {
                if let Some(observer) = observer {
                    observer.on_timing(timing);
                }
            }
            if page_tasks.len() >= self.config.runtime().page_concurrency {
                if let Some(result) = page_tasks.join_next().await {
                    match Self::collect_page_task(result) {
                        Ok(page) => {
                            pages.push(page);
                            if let Some(observer) = observer {
                                observer.on_progress(
                                    crate::ParseProgress::Analyzing {
                                        completed: pages.len() as u32,
                                        total: page_count,
                                    },
                                );
                            }
                        }
                        Err(error) => {
                            fatal_error = Some(error);
                            break 'processing;
                        }
                    }
                }
                continue;
            }
            tokio::select! {
                Some(timing) = timing_receiver.recv(), if observer.is_some() => {
                    if let Some(observer) = observer { observer.on_timing(timing); }
                }
                result = page_tasks.join_next(), if !page_tasks.is_empty() => {
                    if let Some(result) = result {
                        match Self::collect_page_task(result) {
                            Ok(page) => {
                                pages.push(page);
                                if let Some(observer) = observer {
                                    observer.on_progress(crate::ParseProgress::Analyzing { completed: pages.len() as u32, total: page_count });
                                }
                            },
                            Err(error) => {
                                fatal_error = Some(error);
                                break 'processing;
                            }
                        }
                    }
                }
                rendered = render_receiver.recv(), if receiver_open => {
                    match rendered {
                        Some((page_number, result)) => {
                            let Some(extracted) = extracted_pages.remove(&page_number) else {
                                fatal_error = Some(
                                    ParseRuntimeError::MissingExtractedPage { page_number },
                                );
                                break 'processing;
                            };
                            let page_timings = timings.for_page(page_number);
                            let config = Arc::clone(&self.config);
                            let layout_engine = Arc::clone(&self.layout_engine);
                            let ocr_engine = self.ocr_engine.as_ref().map(Arc::clone);
                            let tables = Arc::clone(&tables);
                            let context = Arc::clone(&context);
                            match result {
                                Ok(rendered) => {
                                    // Observers see the owned raster before it moves into analysis.
                                    // No PDFium handles escape, and unobserved parses make no copy.
                                    if let Some(observer) = observer {
                                        observer.on_page_image(page_number, rendered.image.as_ref());
                                    }
                                    page_tasks.spawn(async move {
                                        analyze_rendered_page(PageAnalysisInput::builder()
                                            .config(config).layout_engine(layout_engine).ocr_engine(ocr_engine)
                                            .context(context).extracted(extracted).rendered(rendered).timings(page_timings).tables(tables).build())
                                        .await
                                    });
                                }
                                Err(error) if self.config.runtime().continue_on_page_error => {
                                    tracing::warn!(
                                        "render failed for page {}, using native fallback: {}",
                                        page_number,
                                        error
                                    );
                                    page_tasks.spawn(async move {
                                        analyze_without_render(config, context, extracted, error, page_timings)
                                    });
                                }
                                Err(error) => {
                                    tracing::error!(
                                        "render failed for page {}: {}",
                                        page_number,
                                        error
                                    );
                                    fatal_error = Some(ParseRuntimeError::Pdfium(error));
                                    break 'processing;
                                }
                            }
                        }
                        None => receiver_open = false,
                    }
                }
            }
        }
        if fatal_error.is_some() {
            // Closing the receiver stops the serial PDFium producer after at most its current
            // render, while aborting the JoinSet prevents queued page analyses from starting.
            render_receiver.close();
            page_tasks.abort_all();
            while page_tasks.join_next().await.is_some() {}
        }
        let executor = match producer.await {
            Ok(executor) => executor,
            Err(error) => {
                if let Some(fatal_error) = fatal_error {
                    tracing::warn!(
                        "render producer also failed during fatal page cleanup: {}",
                        error
                    );
                    return Err(fatal_error);
                }
                return Err(ParseRuntimeError::Task(error.to_string()));
            }
        };
        let close_result = executor.close().await;
        if let Some(error) = fatal_error {
            if let Err(close_error) = close_result {
                tracing::warn!(
                    "PDFium executor close also failed after fatal page error: {}",
                    close_error
                );
            }
            return Err(error);
        }
        close_result?;

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

    /// Applies configured page-error continuation to one page-shell outcome.
    fn recover_pre_scan(
        outcome: PreScannedPage,
        continue_on_page_error: bool,
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
        if !continue_on_page_error {
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

    /// Converts one joined page outcome into either a completed page or a fatal error.
    fn collect_page_task(
        result: Result<Result<PageResult, ParseRuntimeError>, TaskError>,
    ) -> Result<PageResult, ParseRuntimeError> {
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

/// Runs layout and optional OCR against one rendered page before pure fusion.
#[derive(TypedBuilder)]
pub(crate) struct PageAnalysisInput {
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    context: Arc<DocumentContext>,
    extracted: ExtractedPage,
    rendered: RenderedPage,
    timings: Timings,
    tables: Arc<TableRuntime>,
}

/// Runs layout, OCR, table resolution, and final page validation over owned page inputs.
pub(crate) async fn analyze_rendered_page(
    input: PageAnalysisInput,
) -> Result<PageResult, ParseRuntimeError> {
    let PageAnalysisInput {
        config,
        layout_engine,
        ocr_engine,
        context,
        extracted,
        rendered,
        timings,
        tables,
    } = input;
    let page_number = extracted.page_number;
    if rendered.page_number != page_number {
        return Err(ParseRuntimeError::RenderedPageMismatch {
            expected: page_number,
            actual: rendered.page_number,
        });
    }
    tracing::debug!("starting layout detection for page {}", page_number);
    let (detections, layout_warning) = match layout_engine
        .detect(
            LayoutRequest::builder()
                .page_number(page_number)
                .image(Arc::clone(&rendered.image))
                .transform(rendered.transform.clone())
                .timings(timings.clone())
                .build(),
        )
        .await
    {
        Ok(detections) => (detections, None),
        Err(error) => {
            tracing::warn!(
                "layout detection failed for page {}, using geometry fallback: {}",
                page_number,
                error
            );
            (
                Vec::new(),
                Some(PageWarning {
                    code: "LayoutUnavailable".to_owned(),
                    stage: "layout".to_owned(),
                    message: error.to_string(),
                }),
            )
        }
    };
    let analyzer =
        PageAnalyzer::new(Arc::clone(&config)).with_timings(timings.clone());
    let preparation = timings.start(TimingStage::TextPrepare);
    let draft = analyzer.prepare(extracted, detections, context)?;
    drop(preparation);
    let completion = if config.ocr().policy == OcrPolicy::Disabled
        || draft.missing_regions.is_empty()
    {
        OcrCompletion::NotRequested
    } else if let Some(engine) = ocr_engine {
        tracing::debug!(
            "starting OCR engine {} for page {}",
            engine.name(),
            page_number
        );
        let request = OcrRequest::builder()
            .page_number(page_number)
            .image(Arc::clone(&rendered.image))
            .transform(rendered.transform.clone())
            .dpi(config.render().dpi)
            .missing_regions(draft.missing_regions.clone())
            .native_text_coverage(draft.native_text_coverage)
            .build();
        let _ocr = timings.start(TimingStage::Ocr);
        match engine.recognize(request).await {
            Ok(result) => OcrCompletion::Succeeded(result),
            Err(error) => OcrCompletion::Failed(error.to_string()),
        }
    } else {
        OcrCompletion::Unavailable
    };
    let finishing = timings.start(TimingStage::TextFinish);
    let mut table_draft = analyzer.compose(draft, completion)?;
    drop(finishing);
    tables
        .resolve(
            &mut table_draft,
            &rendered.image,
            &rendered.transform,
            config.fusion(),
            &timings,
        )
        .await;
    let finishing = timings.start(TimingStage::TextFinish);
    let mut page = analyzer.complete(table_draft)?;
    drop(finishing);
    if let Some(warning) = layout_warning {
        page.warnings.push(warning);
        page.warnings.sort_by(|left, right| {
            left.stage
                .cmp(&right.stage)
                .then_with(|| left.code.cmp(&right.code))
                .then_with(|| left.message.cmp(&right.message))
        });
    }
    Ok(page)
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
        ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
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
        ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
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
        let runtime =
            ParseRuntime::new(config(), Arc::new(EmptyLayoutEngine), None);

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
        raw.layout.model_path = PathBuf::from("/tmp/missing-model.onnx");
        raw.layout.model_config_path = PathBuf::from("/tmp/missing-model.yml");
        raw.layout.model_manifest_path =
            PathBuf::from("/tmp/missing-model.json");
        raw.runtime.page_concurrency = 1;
        raw.runtime.render_queue_capacity = 1;
        raw.runtime.blocking_task_limit = 1;
        let config = Arc::new(
            ValidatedConfig::try_from(raw).expect("test config must validate"),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = ParseRuntime::new(
            config,
            Arc::new(PanickingLayoutEngine {
                calls: Arc::clone(&calls),
            }),
            None,
        );
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
