use docparse_common::timing::Timings;
use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::DocParser;
use docparse_formula::{FormulaEngine, FormulaError};
use docparse_layout::{
    Bbox, GeometrySource, LayoutDetection, LayoutEngine, LayoutError,
    LayoutLabel, LayoutRequest, PageImage, wasm_compat::WasmBoxedFuture,
};
use std::sync::{Arc, Mutex};

/// Existing layout detections provide the only formula regions; numbering is a separate class.
struct Layout {
    display_formulas: bool,
}
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
        let display_formulas = self.display_formulas;
        Box::pin(async move {
            Ok([
                LayoutLabel::InlineFormula,
                LayoutLabel::DisplayFormula,
                LayoutLabel::DisplayFormula,
                LayoutLabel::FormulaNumber,
            ]
            .into_iter()
            .enumerate()
            .filter(|(_, label)| {
                display_formulas || *label != LayoutLabel::DisplayFormula
            })
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
        raw.formula.batch_size = 2;
        raw.render.queue_size = 1;
        let batches = Arc::new(Mutex::new(Vec::new()));
        let parser = DocParser::builder()
            .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
            .layout_engine(Arc::new(Layout {
                display_formulas: true,
            }))
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
        assert_eq!(*batches.lock().expect("batches"), [1; 9]);
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

/// A slow first crop must not prevent later crops from entering the shared engine.
struct RefillingRecognizer {
    started: std::sync::atomic::AtomicUsize,
    ready: tokio::sync::Notify,
    admission: Arc<tokio::sync::Semaphore>,
}

impl FormulaEngine for RefillingRecognizer {
    /// Allows two prepared crops so the second request can release the first.
    fn admission(&self) -> Option<Arc<tokio::sync::Semaphore>> {
        Some(Arc::clone(&self.admission))
    }
    /// Identifies the admission regression independently of model artifacts.
    fn name(&self) -> &str {
        "refilling-formulas"
    }

    /// The first crop can finish only after a second crop reaches the engine.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        _timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>> {
        Box::pin(async move {
            if self
                .started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                self.ready.notified().await;
            } else {
                self.ready.notify_one();
            }
            Ok(vec!["x".into(); images.len()])
        })
    }
}

/// Per-page submission continuously replenishes work instead of awaiting an entire chunk.
#[tokio::test]
async fn slow_crop_does_not_block_subsequent_submission() {
    let mut raw = RawConfig::default();
    raw.tsr.mode = docparse_config::TableMode::RulesOnly;
    raw.formula.batch_size = 1;
    raw.formula.timeout_ms = 500;
    raw.render.queue_size = 1;
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(Layout {
            display_formulas: true,
        }))
        .formula_engine(Arc::new(RefillingRecognizer {
            started: Default::default(),
            ready: Default::default(),
            admission: Arc::new(tokio::sync::Semaphore::new(2)),
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
    assert!(
        result
            .pages
            .iter()
            .flat_map(|page| &page.formulas)
            .all(|formula| formula.error.is_none()),
        "later crops must release the first crop before its deadline"
    );
}

/// Independent toggles cover all combinations, skipping disabled crops while preserving native facts.
#[tokio::test]
async fn independent_formula_toggles_preserve_native_source() {
    for display_formulas in [true, false] {
        let mut original_source = None;
        for (display_enabled, inline_enabled) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let mut raw = RawConfig::default();
            raw.tsr.mode = docparse_config::TableMode::RulesOnly;
            raw.formula.batch_size = 2;
            raw.render.queue_size = 1;
            let mut value = serde_json::to_value(raw).expect("config JSON");
            value
                .get_mut("formula")
                .expect("formula config")
                .as_object_mut()
                .expect("object")
                .insert("inline_enabled".into(), inline_enabled.into());
            value
                .get_mut("formula")
                .expect("formula")
                .as_object_mut()
                .expect("object")
                .insert("display_enabled".into(), display_enabled.into());
            let raw: RawConfig =
                serde_json::from_value(value).expect("inline formula option");
            let batches = Arc::new(Mutex::new(Vec::new()));
            let parser = DocParser::builder()
                .config(Arc::new(
                    ValidatedConfig::try_from(raw).expect("config"),
                ))
                .layout_engine(Arc::new(Layout { display_formulas }))
                .formula_engine(Arc::new(Recognizer {
                    batches: Arc::clone(&batches),
                    fail: false,
                }))
                .build()
                .await
                .expect("parser");
            let result = parser
                .parse_bytes(Arc::from(
                    include_bytes!("fixtures/pdf/multipage_layout.pdf")
                        .as_slice(),
                ))
                .await
                .expect("document");
            let expected = usize::from(inline_enabled)
                + if display_enabled && display_formulas {
                    2
                } else {
                    0
                };
            assert_eq!(
                batches.lock().expect("batches").iter().sum::<usize>(),
                expected * 3
            );
            for page in &result.pages {
                assert_eq!(page.formulas.len(), expected);
                assert!(
                    inline_enabled
                        || page.formulas.iter().all(|formula| formula.label
                            == LayoutLabel::DisplayFormula)
                );
                assert!(
                    page.warnings
                        .iter()
                        .all(|warning| warning.code
                            != "FormulaRecognitionFailed")
                );
            }
            let source: Vec<_> = result
                .pages
                .iter()
                .flat_map(|page| &page.blocks)
                .map(|block| (block.text.clone(), block.lines.clone()))
                .collect();
            if let Some(original) = &original_source {
                assert_eq!(&source, original);
            } else {
                original_source = Some(source);
            }
        }
    }
}
