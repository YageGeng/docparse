use std::{sync::Arc, time::Duration};

use docparse_config::{OcrPolicy, RawConfig, TableMode, ValidatedConfig};
use docparse_core::{DocParser, OcrEngine, OcrError, OcrRequest, OcrResult};
use docparse_layout::wasm_compat::WasmBoxedFuture;
use docparse_layout::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest,
};
use tokio::sync::{Semaphore, mpsc};

/// Reports real layout scheduling without relying on inference speed.
struct ObservedLayout {
    events: mpsc::UnboundedSender<u32>,
    table: bool,
    formula: bool,
}

impl LayoutEngine for ObservedLayout {
    /// Identifies this deterministic geometry-only engine.
    fn name(&self) -> &str {
        "observed-layout"
    }
    /// Keeps the canonical model revision stable across runs.
    fn model_revision(&self) -> &str {
        "pipeline-test"
    }
    /// Records entry into layout before returning geometry fallback.
    fn detect(
        &self,
        request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async move {
            let _ = self.events.send(request.page_number);
            let (width, height) = request.transform.viewport_size();
            Ok([
                (self.table, docparse_layout::LayoutLabel::Table),
                (self.formula, docparse_layout::LayoutLabel::DisplayFormula),
            ]
            .into_iter()
            .enumerate()
            .filter(|(_, (enabled, _))| *enabled)
            .map(|(index, (_, label))| {
                let bounds = if label == docparse_layout::LayoutLabel::Table {
                    [0.0, 0.0, width, height]
                } else {
                    [50.0, 110.0, 100.0, 125.0]
                };
                LayoutDetection::builder()
                    .source_detection_index(index as u32)
                    .raw_label(label.to_str().into())
                    .class_id(index as i64)
                    .label(label)
                    .confidence(0.99)
                    .bbox(
                        docparse_layout::Bbox::try_from(bounds)
                            .expect("bounds"),
                    )
                    .geometry_source(
                        docparse_layout::GeometrySource::DerivedFromBbox,
                    )
                    .model_order(index as i64)
                    .metadata(Default::default())
                    .build()
            })
            .collect())
        })
    }
}

/// Retains the OCR future until the test explicitly releases it.
struct GatedEnrichment(Arc<Semaphore>);

impl OcrEngine for GatedEnrichment {
    /// Identifies the test's controllable enrichment boundary.
    fn name(&self) -> &str {
        "gated-ocr"
    }
    /// Blocks optional inference while allowing independent pipeline stages to progress.
    fn recognize(
        &self,
        _request: OcrRequest,
    ) -> WasmBoxedFuture<'_, Result<OcrResult, OcrError>> {
        Box::pin(async move {
            self.0.acquire().await.expect("open gate").forget();
            Ok(OcrResult::builder().items(Vec::new()).build())
        })
    }
}

impl OcrEngine for ObservedLayout {
    /// Identifies the independent OCR scheduling probe.
    fn name(&self) -> &str {
        "observed-ocr"
    }
    /// Reports entry into OCR before returning no additional text.
    fn recognize(
        &self,
        request: OcrRequest,
    ) -> WasmBoxedFuture<'_, Result<OcrResult, OcrError>> {
        Box::pin(async move {
            let _ = self.events.send(request.page_number);
            Ok(OcrResult::builder().items(Vec::new()).build())
        })
    }
}

impl docparse_core::TableStructureEngine for GatedEnrichment {
    /// Identifies a deliberately stalled TSR provider.
    fn name(&self) -> &str {
        "gated-tsr"
    }
    /// Preserves source text through the normal external-table failure path after release.
    fn recognize(
        &self,
        _request: docparse_core::TsrTableRequest,
    ) -> WasmBoxedFuture<
        '_,
        Result<
            docparse_core::TsrTableInput,
            docparse_core::TableStructureError,
        >,
    > {
        Box::pin(async move {
            self.0.acquire().await.expect("open gate").forget();
            Err(docparse_core::TableStructureError::Engine {
                message: "test table fallback".into(),
            })
        })
    }
}

/// Two delivery slots allow overlapping stages while completed rasterization frees PDFium.
#[tokio::test]
async fn slow_ocr_allows_later_layout_and_another_document() {
    let mut raw = RawConfig::default();
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.tsr.mode = TableMode::RulesOnly;
    raw.ocr.policy = OcrPolicy::Always;
    raw.render.queue_size = 2;
    let gate = Arc::new(Semaphore::new(0));
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: sender,
            table: false,
            formula: false,
        }))
        .ocr_engine(Arc::new(GatedEnrichment(Arc::clone(&gate))))
        .build()
        .await
        .expect("parser");
    let first = parser.clone();
    let task = tokio::spawn(async move {
        first
            .parse_bytes(Arc::from(
                include_bytes!("fixtures/pdf/multipage_layout.pdf").as_slice(),
            ))
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("first layout"),
        Some(1)
    );
    let later =
        tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await;
    // Always release inference before asserting, so failure cannot strand the process-global PDFium lock.
    gate.add_permits(100);
    task.await.expect("join").expect("document");
    assert_eq!(
        later.expect("layout must advance while OCR is blocked"),
        Some(2)
    );

    while receiver.try_recv().is_ok() {}
    let gate = Arc::new(Semaphore::new(0));
    let mut raw = RawConfig::default();
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.tsr.mode = TableMode::RulesOnly;
    raw.ocr.policy = OcrPolicy::Always;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: sender,
            table: false,
            formula: false,
        }))
        .ocr_engine(Arc::new(GatedEnrichment(Arc::clone(&gate))))
        .build()
        .await
        .expect("parser");
    let first = parser.clone();
    let task = tokio::spawn(async move {
        first
            .parse_bytes(Arc::from(
                include_bytes!("fixtures/pdf/extraction_metadata.pdf")
                    .as_slice(),
            ))
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("first document layout"),
        Some(1)
    );
    let second = tokio::spawn(async move {
        parser
            .parse_bytes(Arc::from(
                include_bytes!("fixtures/pdf/extraction_metadata.pdf")
                    .as_slice(),
            ))
            .await
    });
    let opened =
        tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await;
    gate.add_permits(100);
    task.await.expect("join first").expect("first document");
    second.await.expect("join second").expect("second document");
    assert_eq!(
        opened.expect("PDFium must close before OCR finishes"),
        Some(1)
    );
    // A slow TSR request holds one delivery, leaving the second available for OCR.
    let gate = Arc::new(Semaphore::new(0));
    let mut raw = RawConfig::default();
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.tsr.mode = TableMode::TsrOnly;
    raw.ocr.policy = OcrPolicy::Always;
    raw.render.queue_size = 2;
    let (layout_events, _) = mpsc::unbounded_channel();
    let (ocr_events, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: layout_events,
            table: true,
            formula: false,
        }))
        .ocr_engine(Arc::new(ObservedLayout {
            events: ocr_events,
            table: false,
            formula: false,
        }))
        .table_engine(Arc::new(GatedEnrichment(Arc::clone(&gate))))
        .build()
        .await
        .expect("parser");
    let task = tokio::spawn(async move {
        parser
            .parse_bytes(Arc::from(
                include_bytes!("fixtures/pdf/multipage_layout.pdf").as_slice(),
            ))
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("first OCR"),
        Some(1)
    );
    let later =
        tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await;
    gate.add_permits(100);
    task.await
        .expect("join")
        .expect("tables retain native fallback");
    assert_eq!(
        later.expect("OCR must advance while TSR is blocked"),
        Some(2)
    );
}

impl docparse_formula::FormulaEngine for GatedEnrichment {
    /// Identifies a deliberately stalled formula provider.
    fn name(&self) -> &str {
        "gated-formula"
    }

    /// Holds a formula batch until the test releases inference.
    fn recognize(
        &self,
        images: Vec<Arc<docparse_layout::PageImage>>,
        _timings: docparse_common::timing::Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, docparse_formula::FormulaError>>
    {
        Box::pin(async move {
            self.0.acquire().await.expect("open gate").forget();
            Ok(vec!["x".into(); images.len()])
        })
    }
}

impl docparse_core::TableStructureEngine for ObservedLayout {
    /// Identifies the independent table scheduling probe.
    fn name(&self) -> &str {
        "observed-tsr"
    }

    /// Reports TSR admission and preserves source lines through the existing fallback path.
    fn recognize(
        &self,
        request: docparse_core::TsrTableRequest,
    ) -> WasmBoxedFuture<
        '_,
        Result<
            docparse_core::TsrTableInput,
            docparse_core::TableStructureError,
        >,
    > {
        Box::pin(async move {
            let _ = self.events.send(request.page_number);
            Err(docparse_core::TableStructureError::Engine {
                message: "test table fallback".into(),
            })
        })
    }
}

/// Slow formulas must release the table stage while preserving page order, source text, and formulas.
#[tokio::test]
async fn slow_formulas_allow_later_tables() {
    let mut raw = RawConfig::default();
    raw.tsr.mode = TableMode::TsrOnly;
    raw.ocr.policy = OcrPolicy::Disabled;
    raw.formula.inline_enabled = true;
    raw.formula.display_enabled = true;
    raw.render.queue_size = 2;
    let gate = Arc::new(Semaphore::new(0));
    let (layout_events, _) = mpsc::unbounded_channel();
    let (table_events, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: layout_events,
            table: true,
            formula: true,
        }))
        .table_engine(Arc::new(ObservedLayout {
            events: table_events,
            table: false,
            formula: false,
        }))
        .formula_engine(Arc::new(GatedEnrichment(Arc::clone(&gate))))
        .build()
        .await
        .expect("parser");
    let task = tokio::spawn(async move {
        parser
            .parse_bytes(Arc::from(
                include_bytes!("fixtures/pdf/multipage_layout.pdf").as_slice(),
            ))
            .await
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .expect("first table"),
        Some(1)
    );
    let later =
        tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await;
    let waiting_for_formula = !task.is_finished();
    // Two unfinished pages exhaust the shared render queue even after both have finished table processing.
    let beyond_capacity =
        tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await;
    // Release blocked work before asserting so a regression cannot leave native resources stranded.
    gate.add_permits(100);
    let result = task.await.expect("join").expect("document");
    assert_eq!(
        later.expect("TSR must advance while formulas are blocked"),
        Some(2)
    );
    assert!(
        beyond_capacity.is_err(),
        "table buffering must remain bounded"
    );
    assert!(
        waiting_for_formula,
        "the document must wait for formula completion"
    );
    assert_eq!(
        result
            .pages
            .iter()
            .map(|page| page.page_number)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    for page in &result.pages {
        assert_eq!(page.formulas.len(), 1);
        let formula = page.formulas.first().expect("recognized formula");
        assert_eq!(formula.latex.as_deref(), Some("x"));
        assert!(formula.error.is_none());
    }
    docparse_core::ResultValidator::validate(&result).expect("valid document");
}
