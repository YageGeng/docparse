//! Independently schedulable page stages shared by document and standalone parsing.
use super::pipeline::ParseRuntimeError;
use super::{RenderedPage, TableRuntime};
use crate::page::{OcrCompletion, PageAnalyzer};
use crate::{
    DocumentContext, ExtractedPage, OcrEngine, OcrRequest, PageResult,
    PageWarning,
};
use docparse_config::{OcrPolicy, ValidatedConfig};
use docparse_layout::timing::{TimingStage, Timings};
use docparse_layout::{LayoutEngine, LayoutRequest};
use std::sync::Arc;
use typed_builder::TypedBuilder;

/// Runs layout and optional OCR against one rendered page before pure fusion.
#[derive(TypedBuilder)]
pub(crate) struct PageAnalysisInput {
    config: Arc<ValidatedConfig>,
    layout_engine: Arc<dyn LayoutEngine>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    #[builder(default)]
    formula_engine: Option<Arc<dyn docparse_formula::FormulaEngine>>,
    context: Arc<DocumentContext>,
    extracted: ExtractedPage,
    rendered: RenderedPage,
    timings: Timings,
    tables: Arc<TableRuntime>,
}

/// Runs layout, OCR, table resolution, and final page validation over owned page inputs.
impl PageAnalysisInput {
    /// Runs layout and prepares native facts without consuming an OCR or table-stage slot.
    pub(crate) async fn prepare(
        self,
    ) -> Result<PageStage<crate::page::PageAnalysisDraft>, ParseRuntimeError>
    {
        let PageAnalysisInput {
            config,
            layout_engine,
            ocr_engine,
            formula_engine,
            context,
            extracted,
            rendered,
            timings,
            tables,
        } = self;
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
        let formulas = detections
            .iter()
            .filter(|detection| {
                matches!(
                    detection.label,
                    docparse_layout::LayoutLabel::InlineFormula
                        | docparse_layout::LayoutLabel::DisplayFormula
                )
            })
            .cloned()
            .collect();
        let analyzer = PageAnalyzer::new(Arc::clone(&config))
            .with_timings(timings.clone());
        let preparation = timings.start(TimingStage::TextPrepare);
        let draft = analyzer.prepare(extracted, detections, context)?;
        drop(preparation);
        Ok(PageStage::builder()
            .config(config)
            .ocr_engine(ocr_engine)
            .formula_engine(formula_engine)
            .formulas(formulas)
            .rendered(rendered)
            .timings(timings)
            .tables(tables)
            .layout_warning(layout_warning)
            .draft(draft)
            .build())
    }
}

/// Owned page data retained only by its current bounded stage.
#[derive(TypedBuilder)]
pub(crate) struct PageStage<D> {
    config: Arc<ValidatedConfig>,
    #[builder(default)]
    ocr_engine: Option<Arc<dyn OcrEngine>>,
    #[builder(default)]
    formula_engine: Option<Arc<dyn docparse_formula::FormulaEngine>>,
    formulas: Vec<docparse_layout::LayoutDetection>,
    rendered: RenderedPage,
    timings: Timings,
    tables: Arc<TableRuntime>,
    #[builder(default)]
    layout_warning: Option<PageWarning>,
    draft: D,
}

impl PageStage<crate::page::PageAnalysisDraft> {
    /// Completes OCR and CPU text composition independently of later table inference.
    pub(crate) async fn recognize(
        self,
    ) -> Result<PageStage<crate::page::PageTableDraft>, ParseRuntimeError> {
        let Self {
            config,
            ocr_engine,
            formula_engine,
            formulas,
            rendered,
            timings,
            tables,
            layout_warning,
            draft,
        } = self;
        let page_number = draft.extracted.page_number;
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
                .timings(timings.clone())
                .build();
            let _ocr = timings.start(TimingStage::Ocr);
            // Bound optional enrichment without losing native facts when an engine stalls.
            match crate::wasm_compat::timeout(
                std::time::Duration::from_millis(config.ocr().timeout_ms),
                engine.recognize(request),
            )
            .await
            {
                Ok(Ok(result)) => OcrCompletion::Succeeded(result),
                Ok(Err(error)) => OcrCompletion::Failed(error.to_string()),
                Err(_) => OcrCompletion::Failed(format!(
                    "OCR exceeded {} ms",
                    config.ocr().timeout_ms
                )),
            }
        } else {
            OcrCompletion::Unavailable
        };
        // Native CPU fusion must not monopolize an HTTP/heartbeat executor thread.
        let cpu_config = Arc::clone(&config);
        let cpu_timings = timings.clone();
        let draft = docparse_layout::wasm_compat::run_cpu(move || {
            let _timer = cpu_timings.start(TimingStage::TextFinish);
            PageAnalyzer::new(cpu_config)
                .with_timings(cpu_timings)
                .compose(draft, completion)
        })
        .await
        .map_err(|error| ParseRuntimeError::Task(error.to_string()))??;
        Ok(PageStage::builder()
            .config(config)
            .formula_engine(formula_engine)
            .formulas(formulas)
            .rendered(rendered)
            .timings(timings)
            .tables(tables)
            .layout_warning(layout_warning)
            .draft(draft)
            .build())
    }
}

impl PageStage<crate::page::PageTableDraft> {
    /// Resolves tables and releases their stage slot before waiting for formula inference.
    pub(crate) async fn resolve_tables(
        self,
    ) -> Result<PageStage<PageFormulaDraft>, ParseRuntimeError> {
        let Self {
            config,
            rendered,
            timings,
            tables,
            layout_warning,
            mut draft,
            formula_engine,
            formulas,
            ..
        } = self;
        tables
            .resolve(
                &mut draft,
                &rendered.image,
                &rendered.transform,
                config.fusion(),
                &timings,
            )
            .await;
        // Table assembly has finished; retain its measured source words for exact formula byte ranges.
        let words = std::mem::take(&mut draft.extracted.table_evidence.words);
        let cpu_config = Arc::clone(&config);
        let cpu_timings = timings.clone();
        let page = docparse_layout::wasm_compat::run_cpu(move || {
            let _timer = cpu_timings.start(TimingStage::TextFinish);
            PageAnalyzer::new(cpu_config)
                .with_timings(cpu_timings)
                .complete(draft)
        })
        .await
        .map_err(|error| ParseRuntimeError::Task(error.to_string()))??;
        tracing::debug!(
            "completed table stage for page {}; handing off {} formula regions",
            page.page_number,
            formulas.len()
        );
        Ok(PageStage::builder()
            .config(config)
            .formula_engine(formula_engine)
            .formulas(formulas)
            .rendered(rendered)
            .timings(timings)
            .tables(tables)
            .layout_warning(layout_warning)
            .draft(PageFormulaDraft { page, words })
            .build())
    }
}

/// Retains finalized table bindings and measured words until formula projection completes.
pub(crate) struct PageFormulaDraft {
    page: PageResult,
    words: std::collections::BTreeMap<crate::TextItemId, Vec<crate::TableWord>>,
}

impl PageStage<PageFormulaDraft> {
    /// Recognizes formulas in a separately bounded stage without holding table capacity.
    pub(crate) async fn finish(self) -> Result<PageResult, ParseRuntimeError> {
        let Self {
            config,
            rendered,
            timings,
            layout_warning,
            formula_engine,
            formulas,
            draft: PageFormulaDraft { mut page, words },
            ..
        } = self;
        if config.formula().enabled {
            page.recognize_formulas(
                formulas,
                &rendered,
                formula_engine.as_deref(),
                &config,
                &timings,
                &words,
            )
            .await;
        }
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
}

/// Runs the same staged implementation for a single owned page.
pub(crate) async fn analyze_rendered_page(
    input: PageAnalysisInput,
) -> Result<PageResult, ParseRuntimeError> {
    input
        .prepare()
        .await?
        .recognize()
        .await?
        .resolve_tables()
        .await?
        .finish()
        .await
}
