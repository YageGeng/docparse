use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::DocParser;
use docparse_formula::{FormulaEngine, FormulaError};
use docparse_layout::{
    Bbox, GeometrySource, LayoutDetection, LayoutEngine, LayoutError,
    LayoutLabel, LayoutRequest, PageImage, timing::Timings,
    wasm_compat::WasmBoxedFuture,
};
use std::sync::{Arc, Mutex};

/// Existing layout detections provide the only formula regions; numbering is a separate class.
struct Layout;
impl LayoutEngine for Layout {
    /// Returns the test engine identity.
    fn name(&self) -> &str {
        "formula-test-layout"
    }
    /// Returns the stable test model revision.
    fn model_revision(&self) -> &str {
        "fixed"
    }
    /// Supplies formula detections independently of recognized content.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> WasmBoxedFuture<'_, Result<Vec<LayoutDetection>, LayoutError>> {
        Box::pin(async {
            Ok([
                LayoutLabel::InlineFormula,
                LayoutLabel::DisplayFormula,
                LayoutLabel::DisplayFormula,
                LayoutLabel::FormulaNumber,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, label)| {
                LayoutDetection::builder()
                    .source_detection_index(i as u32)
                    .raw_label(label.to_str().to_owned())
                    .class_id(i as i64)
                    .label(label)
                    .confidence(0.9)
                    .bbox(
                        Bbox::try_from([
                            50.0,
                            110.0 + i as f64 * 25.0,
                            100.0,
                            125.0 + i as f64 * 25.0,
                        ])
                        .expect("bbox"),
                    )
                    .polygon(None)
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .model_order(i as i64)
                    .metadata(Default::default())
                    .build()
            })
            .collect())
        })
    }
}

/// Records real batch membership without introducing production test-only hooks.
struct Recognizer {
    batches: Arc<Mutex<Vec<usize>>>,
    fail: bool,
}
impl FormulaEngine for Recognizer {
    /// Returns the test engine identity.
    fn name(&self) -> &str {
        "formula-test-recognizer"
    }
    /// Records crop cardinality and returns deterministic LaTeX or an explicit failure.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        _timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>> {
        Box::pin(async move {
            self.batches.lock().expect("batches").push(images.len());
            assert!(
                images
                    .iter()
                    .all(|image| image.width() > 0 && image.height() > 0)
            );
            if self.fail {
                Err(FormulaError::Invalid("injected model failure".into()))
            } else {
                Ok(vec![r"\frac{a}{b}".into(); images.len()])
            }
        })
    }
}

/// All formula classes are batched, tail crops survive, and failures remain explicit per region.
#[tokio::test]
async fn batch_recognition_covers_every_layout_formula_and_preserves_failures()
{
    for fail in [false, true] {
        let mut raw = RawConfig::default();
        raw.tsr.mode = docparse_config::TableMode::RulesOnly;
        raw.formula.enabled = true;
        raw.formula.batch_size = 2;
        raw.runtime.page_concurrency = 1;
        raw.runtime.render_queue_capacity = 1;
        raw.runtime.blocking_task_limit = 1;
        let batches = Arc::new(Mutex::new(Vec::new()));
        let parser = DocParser::builder()
            .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
            .layout_engine(Arc::new(Layout))
            .formula_engine(Arc::new(Recognizer {
                batches: Arc::clone(&batches),
                fail,
            }))
            .build()
            .await
            .expect("parser");
        let result = parser
            .parse_path(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/pdf/multipage_layout.pdf"),
            )
            .await
            .expect("document");
        assert_eq!(*batches.lock().expect("batches"), [2, 1, 2, 1, 2, 1]);
        for page in result.pages {
            assert_eq!(page.formulas.len(), 3);
            for formula in &page.formulas {
                assert_ne!(formula.label, LayoutLabel::FormulaNumber);
                if fail {
                    assert!(formula.latex.is_none());
                    assert!(formula.markdown.is_none());
                    assert!(formula.error.is_some());
                } else {
                    assert_eq!(formula.latex.as_deref(), Some(r"\frac{a}{b}"));
                    assert_eq!(
                        formula.markdown.as_deref(),
                        Some(if formula.label == LayoutLabel::InlineFormula {
                            r"$\frac{a}{b}$"
                        } else {
                            "$$\n\\frac{a}{b}\n$$"
                        })
                    );
                }
            }
            assert_eq!(
                page.warnings
                    .iter()
                    .any(|warning| warning.code == "FormulaRecognitionFailed"),
                fail
            );
        }
    }
}
