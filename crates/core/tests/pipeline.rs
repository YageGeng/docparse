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
            if self.table {
                let (width, height) = request.transform.viewport_size();
                return Ok(vec![
                    LayoutDetection::builder()
                        .source_detection_index(0)
                        .raw_label("table".into())
                        .class_id(0)
                        .label(docparse_layout::LayoutLabel::Table)
                        .confidence(0.99)
                        .bbox(
                            docparse_layout::Bbox::try_from([
                                0.0, 0.0, width, height,
                            ])
                            .expect("page bounds"),
                        )
                        .geometry_source(
                            docparse_layout::GeometrySource::DerivedFromBbox,
                        )
                        .model_order(0)
                        .metadata(Default::default())
                        .build(),
                ]);
            }
            Ok(Vec::new())
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

/// Slow OCR must neither consume the next page's layout slot nor retain finished PDFium work.
#[tokio::test]
async fn slow_ocr_allows_later_layout_and_another_document() {
    let mut raw = RawConfig::default();
    raw.tsr.mode = TableMode::RulesOnly;
    raw.ocr.policy = OcrPolicy::Always;
    raw.runtime.page_concurrency = 1;
    raw.runtime.render_queue_capacity = 1;
    let gate = Arc::new(Semaphore::new(0));
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: sender,
            table: false,
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
    raw.tsr.mode = TableMode::RulesOnly;
    raw.ocr.policy = OcrPolicy::Always;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: sender,
            table: false,
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
    // A full TSR stage must not retain OCR's slot for the following page.
    let gate = Arc::new(Semaphore::new(0));
    let mut raw = RawConfig::default();
    raw.tsr.mode = TableMode::TsrOnly;
    raw.ocr.policy = OcrPolicy::Always;
    raw.runtime.page_concurrency = 1;
    raw.runtime.render_queue_capacity = 1;
    let (layout_events, _) = mpsc::unbounded_channel();
    let (ocr_events, mut receiver) = mpsc::unbounded_channel();
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(ObservedLayout {
            events: layout_events,
            table: true,
        }))
        .ocr_engine(Arc::new(ObservedLayout {
            events: ocr_events,
            table: false,
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
